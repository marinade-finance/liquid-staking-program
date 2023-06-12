use crate::state::stake_system::StakeSystem;
use crate::state::Fee;
use crate::{MarinadeError, State, require_lte};
use anchor_lang::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct ConfigMarinadeParams {
    pub rewards_fee: Option<Fee>,
    pub slots_for_stake_delta: Option<u64>,
    pub min_stake: Option<u64>,
    pub min_deposit: Option<u64>,
    pub min_withdraw: Option<u64>,
    pub staking_sol_cap: Option<u64>,
    pub liquidity_sol_cap: Option<u64>,
    pub auto_add_validator_enabled: Option<bool>,
}

#[derive(Accounts)]
pub struct ConfigMarinade<'info> {
    #[account(
        mut,
        has_one = admin_authority @ MarinadeError::InvalidAdminAuthority
    )]
    pub state: Account<'info, State>,
    pub admin_authority: Signer<'info>,
}

impl<'info> ConfigMarinade<'info> {
    // fn config_marinade()
    pub fn process(
        &mut self,
        ConfigMarinadeParams {
            rewards_fee,
            slots_for_stake_delta,
            min_stake,
            min_deposit,
            min_withdraw,
            staking_sol_cap,
            liquidity_sol_cap,
            auto_add_validator_enabled,
        }: ConfigMarinadeParams,
    ) -> Result<()> {
        if let Some(rewards_fee) = rewards_fee {
            require_lte!(
                rewards_fee,
                State::MAX_REWARD_FEE,
                MarinadeError::RewardsFeeIsTooHigh
            );
            self.state.reward_fee = rewards_fee;
        }
        if let Some(slots_for_stake_delta) = slots_for_stake_delta {
            require_gte!(
                slots_for_stake_delta,
                StakeSystem::MIN_UPDATE_WINDOW,
                MarinadeError::UpdateWindowIsTooLow
            );
            self.state.stake_system.slots_for_stake_delta = slots_for_stake_delta;
        }
        if let Some(min_stake) = min_stake {
            require_gte!(
                min_stake,
                5 * self.state.rent_exempt_for_token_acc,
                MarinadeError::MinStakeIsTooLow
            );
            self.state.stake_system.min_stake = min_stake;
        }
        if let Some(min_deposit) = min_deposit {
            // It is not dangerous to skip value checks because it is deposit only action
            // We can use u64::MAX to stop accepting deposits
            // or 0 to accept 1 lamport
            self.state.min_deposit = min_deposit;
        }
        if let Some(min_withdraw) = min_withdraw {
            require_lte!(
                min_withdraw,
                State::MAX_WITHDRAW_ATOM,
                MarinadeError::MinWithdrawIsTooHigh
            );
            self.state.min_withdraw = min_withdraw;
        }
        if let Some(staking_sol_cap) = staking_sol_cap {
            self.state.staking_sol_cap = staking_sol_cap;
        }
        if let Some(liquidity_sol_cap) = liquidity_sol_cap {
            self.state.liq_pool.liquidity_sol_cap = liquidity_sol_cap;
        }
        if let Some(auto_add_validator_enabled) = auto_add_validator_enabled {
            self.state.validator_system.auto_add_validator_enabled =
                if auto_add_validator_enabled { 1 } else { 0 };
        }

        Ok(())
    }
}
