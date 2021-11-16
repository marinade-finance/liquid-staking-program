use crate::{calc::proportional, checks::check_address, located::Located, Fee, State, ID};
use anchor_lang::prelude::*;

pub mod add_liquidity;
pub mod initialize;
pub mod remove_liquidity;
pub mod set_lp_params;

#[derive(Clone, AnchorSerialize, AnchorDeserialize, Debug)]
pub struct LiqPool {
    pub lp_mint: Pubkey,
    pub lp_mint_authority_bump_seed: u8,
    pub sol_leg_bump_seed: u8,
    pub msol_leg_authority_bump_seed: u8,
    pub msol_leg: Pubkey,

    //The next 3 values define the SOL/mSOL Liquidity pool fee curve params
    // We assume this pool is always UNBALANCED, there should be more SOL than mSOL 99% of the time
    ///Liquidity target. If the Liquidity reach this amount, the fee reaches lp_min_discount_fee
    pub lp_liquidity_target: u64, // 10_000 SOL initially
    /// Liquidity pool max fee
    pub lp_max_fee: Fee, //3% initially
    /// SOL/mSOL Liquidity pool min fee
    pub lp_min_fee: Fee, //0.3% initially
    /// Treasury cut
    pub treasury_cut: Fee, //2500 => 25% how much of the Liquid unstake fee goes to treasury_msol_account

    pub lp_supply: u64, // virtual lp token supply. May be > real supply because of burning tokens. Use UpdateLiqPool to align it with real value
    pub lent_from_sol_leg: u64,
    pub liquidity_sol_cap: u64,
}

impl LiqPool {
    pub const LP_MINT_AUTHORITY_SEED: &'static [u8] = b"liq_mint";
    pub const SOL_LEG_SEED: &'static [u8] = b"liq_sol";
    pub const MSOL_LEG_AUTHORITY_SEED: &'static [u8] = b"liq_st_sol_authority";
    pub const MSOL_LEG_SEED: &'static str = "liq_st_sol";

    pub fn find_lp_mint_authority(state: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[&state.to_bytes()[..32], Self::LP_MINT_AUTHORITY_SEED],
            &ID,
        )
    }

    pub fn find_sol_leg_address(state: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[&state.to_bytes()[..32], Self::SOL_LEG_SEED], &ID)
    }

    pub fn find_msol_leg_authority(state: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[&state.to_bytes()[..32], Self::MSOL_LEG_AUTHORITY_SEED],
            &ID,
        )
    }

    pub fn default_msol_leg_address(state: &Pubkey) -> Pubkey {
        Pubkey::create_with_seed(state, Self::MSOL_LEG_SEED, &spl_token::ID).unwrap()
    }

    pub fn check_lp_mint(&mut self, lp_mint: &Pubkey) -> ProgramResult {
        check_address(lp_mint, &self.lp_mint, "lp_mint")
    }

    pub fn check_liq_pool_msol_leg(&self, liq_pool_msol_leg: &Pubkey) -> ProgramResult {
        check_address(liq_pool_msol_leg, &self.msol_leg, "liq_pool_msol_leg")
    }

    pub fn delta(&self) -> u32 {
        self.lp_max_fee
            .basis_points
            .saturating_sub(self.lp_min_fee.basis_points)
    }

    ///compute a linear fee based on liquidity amount, it goes from fee(0)=max -> fee(x>=target)=min
    pub fn linear_fee(&self, lamports: u64) -> Fee {
        if lamports >= self.lp_liquidity_target {
            self.lp_min_fee
        } else {
            Fee {
                basis_points: self.lp_max_fee.basis_points
                    - proportional(self.delta() as u64, lamports, self.lp_liquidity_target).unwrap()
                        as u32,
            }
        }
    }

    pub fn on_lp_mint(&mut self, amount: u64) {
        self.lp_supply = self
            .lp_supply
            .checked_add(amount)
            .expect("lp_supply overflow");
    }

    pub fn on_lp_burn(&mut self, amount: u64) {
        self.lp_supply = self.lp_supply.saturating_sub(amount);
    }

    pub fn check_liquidity_cap(
        &self,
        transfering_lamports: u64,
        sol_leg_balance: u64,
    ) -> ProgramResult {
        let result_amount = sol_leg_balance
            .checked_add(transfering_lamports)
            .ok_or_else(|| {
                msg!("SOL overflow");
                ProgramError::InvalidArgument
            })?;
        if result_amount > self.liquidity_sol_cap {
            msg!(
                "Liquidity cap reached {}/{}",
                result_amount,
                self.liquidity_sol_cap
            );
            return Err(ProgramError::Custom(3782));
        }
        Ok(())
    }
}

pub trait LiqPoolHelpers {
    fn with_lp_mint_authority_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R;
    fn lp_mint_authority(&self) -> Pubkey;

    fn with_liq_pool_sol_leg_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R;
    fn liq_pool_sol_leg_address(&self) -> Pubkey;

    fn with_liq_pool_msol_leg_authority_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R;
    fn liq_pool_msol_leg_authority(&self) -> Pubkey;

    fn check_lp_mint_authority(&self, lp_mint_authority: &Pubkey) -> ProgramResult;
    fn check_liq_pool_sol_leg_pda(&self, liq_pool_sol_leg_pda: &Pubkey) -> ProgramResult;
    fn check_liq_pool_msol_leg_authority(
        &self,
        liq_pool_msol_leg_authority: &Pubkey,
    ) -> ProgramResult;
}

impl<T> LiqPoolHelpers for T
where
    T: Located<State>,
{
    // call a function adding lp_mint_authority_seeds
    fn with_lp_mint_authority_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R {
        f(&[
            &self.key().to_bytes()[..32],
            LiqPool::LP_MINT_AUTHORITY_SEED,
            &[self.as_ref().liq_pool.lp_mint_authority_bump_seed],
        ])
    }

    fn lp_mint_authority(&self) -> Pubkey {
        self.with_lp_mint_authority_seeds(|seeds| {
            Pubkey::create_program_address(seeds, &ID).unwrap()
        })
    }

    fn with_liq_pool_sol_leg_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R {
        f(&[
            &self.key().to_bytes()[..32],
            LiqPool::SOL_LEG_SEED,
            &[self.as_ref().liq_pool.sol_leg_bump_seed],
        ])
    }

    fn liq_pool_sol_leg_address(&self) -> Pubkey {
        self.with_liq_pool_sol_leg_seeds(|seeds| {
            Pubkey::create_program_address(seeds, &ID).unwrap()
        })
    }

    fn with_liq_pool_msol_leg_authority_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R {
        f(&[
            &self.key().to_bytes()[..32],
            LiqPool::MSOL_LEG_AUTHORITY_SEED,
            &[self.as_ref().liq_pool.msol_leg_authority_bump_seed],
        ])
    }

    fn liq_pool_msol_leg_authority(&self) -> Pubkey {
        self.with_liq_pool_msol_leg_authority_seeds(|seeds| {
            Pubkey::create_program_address(seeds, &ID).unwrap()
        })
    }

    fn check_lp_mint_authority(&self, lp_mint_authority: &Pubkey) -> ProgramResult {
        check_address(
            lp_mint_authority,
            &self.lp_mint_authority(),
            "lp_mint_authority",
        )
    }

    fn check_liq_pool_sol_leg_pda(&self, liq_pool_sol_leg_pda: &Pubkey) -> ProgramResult {
        check_address(
            liq_pool_sol_leg_pda,
            &self.liq_pool_sol_leg_address(),
            "liq_pool_sol_leg_pda",
        )
    }

    fn check_liq_pool_msol_leg_authority(
        &self,
        liq_pool_msol_leg_authority: &Pubkey,
    ) -> ProgramResult {
        check_address(
            liq_pool_msol_leg_authority,
            &self.liq_pool_msol_leg_authority(),
            "liq_pool_msol_leg_authority",
        )
    }
}
