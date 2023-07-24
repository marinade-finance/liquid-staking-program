use crate::events::{
    admin::ConfigMarinadeEvent, BoolValueChange, FeeCentsValueChange, FeeValueChange,
    U64ValueChange,
};
use crate::{
    require_lte,
    state::{stake_system::StakeSystem, Fee, FeeCents},
    MarinadeError, State,
};
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
    pub withdraw_stake_account_enabled: Option<bool>,
    pub delayed_unstake_fee: Option<FeeCents>,
    pub withdraw_stake_account_fee: Option<FeeCents>,
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
            withdraw_stake_account_enabled,
            delayed_unstake_fee,
            withdraw_stake_account_fee,
        }: ConfigMarinadeParams,
    ) -> Result<()> {
        let rewards_fee_change = if let Some(rewards_fee) = rewards_fee {
            require_lte!(
                rewards_fee,
                State::MAX_REWARD_FEE,
                MarinadeError::RewardsFeeIsTooHigh
            );
            let old = self.state.reward_fee;
            self.state.reward_fee = rewards_fee;
            Some(FeeValueChange {
                old,
                new: rewards_fee,
            })
        } else {
            None
        };

        let slots_for_stake_delta_change =
            if let Some(slots_for_stake_delta) = slots_for_stake_delta {
                require_gte!(
                    slots_for_stake_delta,
                    StakeSystem::MIN_UPDATE_WINDOW,
                    MarinadeError::UpdateWindowIsTooLow
                );
                let old = self.state.stake_system.slots_for_stake_delta;
                self.state.stake_system.slots_for_stake_delta = slots_for_stake_delta;
                Some(U64ValueChange {
                    old,
                    new: slots_for_stake_delta,
                })
            } else {
                None
            };

        let min_stake_change = if let Some(min_stake) = min_stake {
            require_gte!(
                min_stake,
                5 * self.state.rent_exempt_for_token_acc,
                MarinadeError::MinStakeIsTooLow
            );
            let old = self.state.stake_system.min_stake;
            self.state.stake_system.min_stake = min_stake;
            Some(U64ValueChange {
                old,
                new: min_stake,
            })
        } else {
            None
        };

        let min_deposit_change = if let Some(min_deposit) = min_deposit {
            // It is not dangerous to skip value checks because it is deposit only action
            // We can use u64::MAX to stop accepting deposits
            // or 0 to accept 1 lamport
            let old = self.state.min_deposit;
            self.state.min_deposit = min_deposit;
            Some(U64ValueChange {
                old,
                new: min_deposit,
            })
        } else {
            None
        };

        let min_withdraw_change = if let Some(min_withdraw) = min_withdraw {
            require_lte!(
                min_withdraw,
                State::MAX_WITHDRAW_ATOM,
                MarinadeError::MinWithdrawIsTooHigh
            );
            let old = self.state.min_withdraw;
            self.state.min_withdraw = min_withdraw;
            Some(U64ValueChange {
                old,
                new: min_withdraw,
            })
        } else {
            None
        };

        let staking_sol_cap_change = if let Some(staking_sol_cap) = staking_sol_cap {
            let old = self.state.staking_sol_cap;
            self.state.staking_sol_cap = staking_sol_cap;
            Some(U64ValueChange {
                old,
                new: staking_sol_cap,
            })
        } else {
            None
        };

        let liquidity_sol_cap_change = if let Some(liquidity_sol_cap) = liquidity_sol_cap {
            let old = self.state.liq_pool.liquidity_sol_cap;
            self.state.liq_pool.liquidity_sol_cap = liquidity_sol_cap;
            Some(U64ValueChange {
                old,
                new: liquidity_sol_cap,
            })
        } else {
            None
        };

        let auto_add_validator_enabled_change =
            if let Some(auto_add_validator_enabled) = auto_add_validator_enabled {
                let old = self.state.validator_system.auto_add_validator_enabled != 0;
                self.state.validator_system.auto_add_validator_enabled =
                    if auto_add_validator_enabled { 1 } else { 0 };
                Some(BoolValueChange {
                    old,
                    new: auto_add_validator_enabled,
                })
            } else {
                None
            };

        let withdraw_stake_account_enabled_change =
            if let Some(withdraw_stake_account_enabled) = withdraw_stake_account_enabled {
                let old = self.state.withdraw_stake_account_enabled;
                self.state.withdraw_stake_account_enabled = withdraw_stake_account_enabled;
                Some(BoolValueChange {
                    old,
                    new: withdraw_stake_account_enabled,
                })
            } else {
                None
            };

        let delayed_unstake_fee_change = if let Some(delayed_unstake_fee) = delayed_unstake_fee {
            require_lte!(
                delayed_unstake_fee,
                State::MAX_DELAYED_UNSTAKE_FEE,
                MarinadeError::RewardsFeeIsTooHigh
            );
            let old = self.state.delayed_unstake_fee;
            self.state.delayed_unstake_fee = delayed_unstake_fee;
            Some(FeeCentsValueChange {
                old,
                new: delayed_unstake_fee,
            })
        } else {
            None
        };

        let withdraw_stake_account_fee_change =
            if let Some(withdraw_stake_account_fee) = withdraw_stake_account_fee {
                require_lte!(
                    withdraw_stake_account_fee,
                    State::MAX_WITHDRAW_STAKE_ACCOUNT_FEE,
                    MarinadeError::RewardsFeeIsTooHigh
                );
                let old = self.state.withdraw_stake_account_fee;
                self.state.withdraw_stake_account_fee = withdraw_stake_account_fee;
                Some(FeeCentsValueChange {
                    old,
                    new: withdraw_stake_account_fee,
                })
            } else {
                None
            };

        emit!(ConfigMarinadeEvent {
            state: self.state.key(),
            rewards_fee_change,
            slots_for_stake_delta_change,
            min_stake_change,
            min_deposit_change,
            min_withdraw_change,
            staking_sol_cap_change,
            liquidity_sol_cap_change,
            auto_add_validator_enabled_change,
            withdraw_stake_account_enabled_change,
            delayed_unstake_fee_change,
            withdraw_stake_account_fee_change,
        });

        Ok(())
    }
}
