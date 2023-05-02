use crate::ConfigLp;
use crate::ConfigLpParams;
use anchor_lang::prelude::*;

impl<'info> ConfigLp<'info> {
    pub fn process(
        &mut self,
        ConfigLpParams {
            min_fee,
            max_fee,
            liquidity_target,
            treasury_cut,
        }: ConfigLpParams,
    ) -> Result<()> {
        if let Some(min_fee) = min_fee {
            self.state.liq_pool.lp_min_fee = min_fee;
        }
        if let Some(max_fee) = max_fee {
            self.state.liq_pool.lp_max_fee = max_fee;
        }
        if let Some(liquidity_target) = liquidity_target {
            self.state.liq_pool.lp_liquidity_target = liquidity_target;
        }
        if let Some(treasury_cut) = treasury_cut {
            self.state.liq_pool.treasury_cut = treasury_cut;
        }

        self.state.liq_pool.check_fees()?;
        Ok(())
    }
}
