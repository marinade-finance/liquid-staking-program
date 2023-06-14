use anchor_lang::prelude::*;

use crate::{
    events::{admin::ConfigLpEvent, FeeValueChange, U64ValueChange},
    state::Fee,
    MarinadeError, State,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct ConfigLpParams {
    pub min_fee: Option<Fee>,
    pub max_fee: Option<Fee>,
    pub liquidity_target: Option<u64>,
    pub treasury_cut: Option<Fee>,
}

#[derive(Accounts)]
pub struct ConfigLp<'info> {
    #[account(
        mut,
        has_one = admin_authority @ MarinadeError::InvalidAdminAuthority
    )]
    pub state: Account<'info, State>,
    pub admin_authority: Signer<'info>,
}

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
        let min_fee_change = if let Some(min_fee) = min_fee {
            let old = self.state.liq_pool.lp_min_fee;
            self.state.liq_pool.lp_min_fee = min_fee;
            Some(FeeValueChange { old, new: min_fee })
        } else {
            None
        };

        let max_fee_change = if let Some(max_fee) = max_fee {
            let old = self.state.liq_pool.lp_max_fee;
            self.state.liq_pool.lp_max_fee = max_fee;
            Some(FeeValueChange { old, new: max_fee })
        } else {
            None
        };

        let liquidity_target_change = if let Some(liquidity_target) = liquidity_target {
            let old = self.state.liq_pool.lp_liquidity_target;
            self.state.liq_pool.lp_liquidity_target = liquidity_target;
            Some(U64ValueChange {
                old,
                new: liquidity_target,
            })
        } else {
            None
        };

        let treasury_cut_change = if let Some(treasury_cut) = treasury_cut {
            let old = self.state.liq_pool.treasury_cut;
            self.state.liq_pool.treasury_cut = treasury_cut;
            Some(FeeValueChange {
                old,
                new: treasury_cut,
            })
        } else {
            None
        };

        self.state.liq_pool.validate()?;

        emit!(ConfigLpEvent {
            state: self.state.key(),
            min_fee_change,
            max_fee_change,
            liquidity_target_change,
            treasury_cut_change
        });
        Ok(())
    }
}
