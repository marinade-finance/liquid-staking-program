use crate::{error::CommonError, Fee, SetLpParams};
use anchor_lang::prelude::ProgramResult;
use anchor_lang::solana_program::native_token::sol_to_lamports;

impl<'info> SetLpParams<'info> {
    fn check_fees(&self, min_fee: Fee, max_fee: Fee) -> ProgramResult {
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

    fn check_liquidity_target(&self, liquidity_target: u64) -> ProgramResult {
        if liquidity_target < sol_to_lamports(50.0) {
            Err(CommonError::LiquidityTargetTooLow.into())
        } else {
            Ok(())
        }
    }

    pub fn process(&mut self, min_fee: Fee, max_fee: Fee, liquidity_target: u64) -> ProgramResult {
        self.state.check_admin_authority(self.admin_authority.key)?;
        self.check_fees(min_fee, max_fee)?;
        self.check_liquidity_target(liquidity_target)?;

        self.state.liq_pool.lp_min_fee = min_fee;
        self.state.liq_pool.lp_max_fee = max_fee;
        self.state.liq_pool.lp_liquidity_target = liquidity_target;
        Ok(())
    }
}
