use crate::{
    checks::{check_address, check_owner_program},
    error::CommonError,
    stake_system::StakeSystemHelpers,
    stake_wrapper::StakeWrapper,
    state::StateHelpers,
    StakeReserve,
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    log::sol_log_compute_units,
    program::{invoke, invoke_signed},
    stake::{
        self,
        state::{Authorized, Lockup, StakeState},
    },
    system_instruction, system_program,
    sysvar::stake_history,
};
use std::convert::TryFrom;
use std::ops::Deref;

impl<'info> StakeReserve<'info> {
    fn check_stake_history(&self) -> ProgramResult {
        if !stake_history::check_id(self.stake_history.key) {
            msg!(
                "Stake history sysvar must be {}. Got {}",
                stake_history::ID,
                self.stake_history.key
            );
            return Err(ProgramError::InvalidArgument);
        }
        Ok(())
    }

    ///
    /// called by the bot
    /// Receives self.stake_account where to stake, normally an empty account (new keypair)
    /// stakes from available delta-stake in data.validator_index
    /// pub fn stake_reserve()
    pub fn process(&mut self, validator_index: u32) -> ProgramResult {
        sol_log_compute_units();
        msg!("Stake reserve");
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;
        self.state.stake_system.check_stake_list(&self.stake_list)?;
        self.state.check_reserve_address(self.reserve_pda.key)?;
        self.check_stake_history()?;
        self.state
            .check_stake_deposit_authority(self.stake_deposit_authority.key)?;
        check_owner_program(&self.stake_account, &stake::program::ID, "stake")?;
        match StakeWrapper::deref(&self.stake_account) {
            StakeState::Uninitialized => (),
            _ => {
                msg!("Stake {} must be uninitialized", self.stake_account.key());
                return Err(ProgramError::InvalidAccountData);
            }
        }
        if self.stake_account.to_account_info().lamports()
            != StakeState::get_rent_exempt_reserve(&self.rent)
        {
            msg!(
                "Stake {} must have balance {} but has {} lamports",
                self.stake_account.key(),
                StakeState::get_rent_exempt_reserve(&self.rent),
                self.stake_account.to_account_info().lamports()
            );
            return Err(ProgramError::InvalidAccountData);
        }

        check_address(self.stake_config.key, &stake::config::ID, "stake_config")?;
        check_address(
            self.system_program.key,
            &system_program::ID,
            "system_program",
        )?;
        check_address(self.stake_program.key, &stake::program::ID, "stake_program")?;

        let staker = self.state.stake_deposit_authority();
        let withdrawer = self.state.stake_withdraw_authority();

        let stake_delta = self.state.stake_delta(self.reserve_pda.lamports());
        if stake_delta <= 0 {
            if stake_delta < 0 {
                msg!(
                    "Must unstake {} instead of staking",
                    u64::try_from(-stake_delta).expect("Stake delta overflow")
                );
            } else {
                msg!("Noting to do");
            }
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }
        let stake_delta = u64::try_from(stake_delta).expect("Stake delta overflow");
        let total_stake_target = self
            .state
            .validator_system
            .total_active_balance
            .saturating_add(stake_delta);

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        check_address(
            &self.validator_vote.key,
            &validator.validator_account,
            "validator_vote",
        )?;

        if validator.last_stake_delta_epoch == self.clock.epoch {
            // check if we have some extra stake runs allowed
            if self.state.stake_system.extra_stake_delta_runs == 0 {
                msg!(
                    "Double delta stake command for validator {} in epoch {}",
                    validator.validator_account,
                    self.clock.epoch
                );
                return Ok(()); // Not an error. Don't fail other instructions in tx
            } else {
                // some extra runs allowed. Use one
                self.state.stake_system.extra_stake_delta_runs -= 1;
            }
        } else {
            // first stake in this epoch
            validator.last_stake_delta_epoch = self.clock.epoch;
        }

        let last_slot = self.epoch_schedule.get_last_slot_in_epoch(self.clock.epoch);

        if self.clock.slot < last_slot.saturating_sub(self.state.stake_system.slots_for_stake_delta)
        {
            msg!(
                "Stake delta is available only last {} slots of epoch",
                self.state.stake_system.slots_for_stake_delta
            );
            return Err(ProgramError::Custom(332));
        }

        let validator_stake_target = self
            .state
            .validator_system
            .validator_stake_target(&validator, total_stake_target)?;

        //verify the validator is under-staked
        if validator.active_balance >= validator_stake_target {
            msg!(
                    "Validator {} has already reached stake target {}. Please stake into another validator",
                    validator.validator_account,
                    validator_stake_target
                );
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }

        // compute stake_target
        // stake_target = target_validator_balance - validator.balance, at least self.state.min_stake and at most delta_stake
        let stake_target = validator_stake_target
            .saturating_sub(validator.active_balance)
            .max(self.state.stake_system.min_stake)
            .min(stake_delta);

        // if what's left after this stake is < state.min_stake, take all the remainder
        let stake_target = if stake_delta - stake_target < self.state.stake_system.min_stake {
            stake_delta
        } else {
            stake_target
        };

        // transfer SOL from reserve_pda to the stake-account
        self.state.with_reserve_seeds(|seeds| {
            sol_log_compute_units();
            msg!("Transfer to stake account");
            invoke_signed(
                &system_instruction::transfer(
                    self.reserve_pda.key,
                    &self.stake_account.key(),
                    stake_target,
                ),
                &[
                    self.system_program.clone(),
                    self.reserve_pda.clone(),
                    self.stake_account.to_account_info(),
                ],
                &[seeds],
            )
        })?;
        self.state.on_transfer_from_reserve(stake_target);

        sol_log_compute_units();
        msg!("Initialize stake");
        invoke(
            &stake::instruction::initialize(
                &self.stake_account.key(),
                &Authorized { staker, withdrawer },
                &Lockup::default(),
            ),
            &[
                self.stake_program.clone(),
                self.stake_account.to_account_info(),
                self.rent.to_account_info(),
            ],
        )?;

        self.state.with_stake_deposit_authority_seeds(|seeds| {
            sol_log_compute_units();
            msg!("Delegate stake");
            invoke_signed(
                &stake::instruction::delegate_stake(
                    &self.stake_account.key(),
                    &staker,
                    self.validator_vote.key,
                ),
                &[
                    self.stake_program.clone(),
                    self.stake_account.to_account_info(),
                    self.stake_deposit_authority.clone(),
                    self.validator_vote.clone(),
                    self.clock.to_account_info(),
                    self.stake_history.clone(),
                    self.stake_config.to_account_info(),
                ],
                &[seeds],
            )
        })?;

        self.state.stake_system.add(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            &self.stake_account.key(),
            stake_target,
            &self.clock,
        )?;

        // self.state.epoch_stake_orders -= amount;
        validator.active_balance = validator
            .active_balance
            .checked_add(stake_target)
            .ok_or(CommonError::CalculationFailure)?;
        validator.last_stake_delta_epoch = self.clock.epoch;
        // Any stake-delta activity must activate stake delta mode
        self.state.stake_system.last_stake_delta_epoch = self.clock.epoch;
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            validator_index,
            validator,
        )?;
        self.state.validator_system.total_active_balance = self
            .state
            .validator_system
            .total_active_balance
            .checked_add(stake_target)
            .ok_or(CommonError::CalculationFailure)?;
        Ok(())
    }
}
