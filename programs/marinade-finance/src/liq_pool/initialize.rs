use super::LiqPool;
use crate::{
    checks::{
        check_address, check_freeze_authority, check_mint_authority, check_mint_empty,
        check_owner_program, check_token_mint, check_token_owner,
    },
    CommonError, Fee, Initialize, LiqPoolInitialize, LiqPoolInitializeData,
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;

impl<'info> LiqPoolInitialize<'info> {
    pub fn check_liq_mint(parent: &mut Initialize) -> ProgramResult {
        check_owner_program(&parent.liq_pool.lp_mint, &spl_token::ID, "lp_mint")?;
        if parent.liq_pool.lp_mint.to_account_info().key == parent.msol_mint.to_account_info().key {
            msg!("Use different mints for stake and liquidity pool");
            return Err(ProgramError::InvalidAccountData);
        }
        let (authority_address, authority_bump_seed) =
            LiqPool::find_lp_mint_authority(parent.state_address());

        check_mint_authority(&parent.liq_pool.lp_mint, authority_address, "lp_mint")?;

        parent.state.liq_pool.lp_mint_authority_bump_seed = authority_bump_seed;

        check_mint_empty(&parent.liq_pool.lp_mint, "lp_mint")?;
        check_freeze_authority(&parent.liq_pool.lp_mint, "lp_mint")?;

        Ok(())
    }

    pub fn check_sol_account_pda(parent: &mut Initialize) -> ProgramResult {
        check_owner_program(
            &parent.liq_pool.sol_leg_pda,
            &system_program::ID,
            "liq_sol_account_pda",
        )?;
        let (address, bump) = LiqPool::find_sol_leg_address(parent.state_address());
        check_address(
            parent.liq_pool.sol_leg_pda.key,
            &address,
            "liq_sol_account_pda",
        )?;
        parent.state.liq_pool.sol_leg_bump_seed = bump;
        {
            let lamports = parent.liq_pool.sol_leg_pda.lamports();
            if lamports != parent.state.rent_exempt_for_token_acc {
                msg!(
                    "Invalid initial liq_sol_account_pda lamports {} expected {}",
                    lamports,
                    parent.state.rent_exempt_for_token_acc
                );
                return Err(ProgramError::InvalidArgument);
            }
        }
        Ok(())
    }

    pub fn check_msol_account(parent: &mut Initialize) -> ProgramResult {
        check_owner_program(&parent.liq_pool.msol_leg, &spl_token::ID, "liq_msol_leg")?;
        check_token_mint(
            &parent.liq_pool.msol_leg,
            *parent.msol_mint.to_account_info().key,
            "liq_msol",
        )?;
        let (msol_authority, msol_authority_bump_seed) =
            LiqPool::find_msol_leg_authority(parent.state_address());
        check_token_owner(&parent.liq_pool.msol_leg, &msol_authority, "liq_msol_leg")?;
        parent.state.liq_pool.msol_leg_authority_bump_seed = msol_authority_bump_seed;
        Ok(())
    }
    pub fn check_fees(min_fee: Fee, max_fee: Fee) -> ProgramResult {
        min_fee.check()?;
        max_fee.check()?;
        //hard-limit, max liquid unstake-fee of 10%
        if max_fee.basis_points > 1000 {
            return Err(CommonError::FeeTooHigh.into());
        }
        if min_fee > max_fee {
            return Err(CommonError::FeesWrongWayRound.into());
        }
        Ok(())
    }

    pub fn process(parent: &mut Initialize, data: LiqPoolInitializeData) -> ProgramResult {
        Self::check_liq_mint(parent)?;
        Self::check_sol_account_pda(parent)?;
        Self::check_msol_account(parent)?;
        Self::check_fees(data.lp_min_fee, data.lp_max_fee)?;
        data.lp_treasury_cut.check()?;

        parent.state.liq_pool.lp_mint = *parent.liq_pool.lp_mint.to_account_info().key;

        parent.state.liq_pool.msol_leg = *parent.liq_pool.msol_leg.to_account_info().key;

        parent.state.liq_pool.treasury_cut = data.lp_treasury_cut; // Fee { basis_points: 2500 }; //25% from the liquid-unstake fee

        parent.state.liq_pool.lp_liquidity_target = data.lp_liquidity_target; //10_000 SOL
        parent.state.liq_pool.lp_min_fee = data.lp_min_fee; // Fee { basis_points: 30 }; //0.3%
        parent.state.liq_pool.lp_max_fee = data.lp_max_fee; // Fee { basis_points: 300 }; //3%
        parent.state.liq_pool.liquidity_sol_cap = std::u64::MAX; // Unlimited

        Ok(())
    }
}
