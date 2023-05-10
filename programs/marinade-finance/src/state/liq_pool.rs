use crate::{
    calc::proportional, error::MarinadeError, located::Located, state::Fee,
    State, ID,
};
use anchor_lang::{prelude::*, solana_program::native_token::LAMPORTS_PER_SOL};
use anchor_spl::token::spl_token;

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
    pub const MAX_FEE: Fee = Fee::from_basis_points(1000); // 10%
    pub const MIN_LIQUIDITY_TARGET: u64 = 50 * LAMPORTS_PER_SOL; // 50 SOL
    pub const MAX_TREASURY_CUT: Fee = Fee::from_basis_points(7500); // 75%

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

    pub fn on_lp_burn(&mut self, amount: u64) -> Result<()> {
        self.lp_supply = self
            .lp_supply
            .checked_sub(amount)
            .ok_or(MarinadeError::CalculationFailure)?;
        Ok(())
    }

    pub fn check_liquidity_cap(
        &self,
        transfering_lamports: u64,
        sol_leg_balance: u64,
    ) -> Result<()> {
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
            return Err(Error::from(ProgramError::Custom(3782)).with_source(source!()));
        }
        Ok(())
    }

    pub fn check_fees(&self) -> Result<()> {
        self.lp_min_fee.check()?;
        self.lp_max_fee.check()?;
        self.treasury_cut.check()?;
        // hard-limit, max liquid unstake-fee of 10%
        if self.lp_max_fee > Self::MAX_FEE {
            return Err(MarinadeError::FeeTooHigh.into());
        }
        if self.lp_min_fee > self.lp_max_fee {
            return Err(MarinadeError::FeesWrongWayRound.into());
        }
        if self.lp_liquidity_target < Self::MIN_LIQUIDITY_TARGET {
            return Err(MarinadeError::LiquidityTargetTooLow.into());
        }
        if self.treasury_cut > Self::MAX_TREASURY_CUT {
            return Err(MarinadeError::NumberTooHigh.into());
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

}
