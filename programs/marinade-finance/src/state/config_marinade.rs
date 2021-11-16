use crate::{CommonError, ConfigMarinade, ConfigMarinadeParams, MAX_REWARD_FEE};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::native_token::LAMPORTS_PER_SOL;

impl<'info> ConfigMarinade<'info> {
    const MIN_WITHDRAW_CAP: u64 = LAMPORTS_PER_SOL / 10;
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
    ) -> ProgramResult {
        self.state.check_admin_authority(self.admin_authority.key)?;
        if let Some(rewards_fee) = rewards_fee {
            rewards_fee.check_max(MAX_REWARD_FEE)?;
            self.state.reward_fee = rewards_fee;
        }
        if let Some(slots_for_stake_delta) = slots_for_stake_delta {
            const MIN_UPDATE_WINDOW: u64 = 3_000; //min value is 3_000 => half an hour
            if slots_for_stake_delta < MIN_UPDATE_WINDOW {
                return Err(CommonError::NumberTooLow.into());
            };
            self.state.stake_system.slots_for_stake_delta = slots_for_stake_delta;
        }
        if let Some(min_stake) = min_stake {
            let min_accepted = 5 * self.state.rent_exempt_for_token_acc;
            if min_stake < min_accepted {
                return Err(CommonError::NumberTooLow.into());
            };
            self.state.stake_system.min_stake = min_stake;
        }
        if let Some(min_deposit) = min_deposit {
            // It is not dangerous to skip value checks because it is deposit only action
            // We can use u64::MAX to stop accepting deposits
            // or 0 to accept 1 lamport
            self.state.min_deposit = min_deposit;
        }
        if let Some(min_withdraw) = min_withdraw {
            if min_withdraw > Self::MIN_WITHDRAW_CAP {
                return Err(CommonError::NumberTooHigh.into());
            }
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
