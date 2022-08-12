use crate::ConfigureLp;
use crate::ConfigureLpParams;
use anchor_lang::prelude::ProgramResult;

impl<'info> ConfigureLp<'info> {
    pub fn process(
        &mut self,
        ConfigureLpParams {
            min_fee,
            max_fee,
            liquidity_target,
            treasury_cut,
        }: ConfigureLpParams,
    ) -> ProgramResult {
        self.state.check_admin_authority(self.admin_authority.key)?;
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
