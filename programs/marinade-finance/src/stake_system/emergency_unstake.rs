use crate::{checks::check_owner_program, stake_system::StakeSystemHelpers};

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::invoke_signed,
    stake::{self},
};

use crate::{checks::check_address, EmergencyUnstake};

impl<'info> EmergencyUnstake<'info> {
    pub fn process(&mut self, stake_index: u32, validator_index: u32) -> ProgramResult {
        self.state
            .validator_system
            .check_validator_manager_authority(self.validator_manager_authority.key)?;
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;
        self.state.stake_system.check_stake_list(&self.stake_list)?;
        self.state
            .check_stake_deposit_authority(self.stake_deposit_authority.key)?;
        check_owner_program(&self.stake_account, &stake::program::ID, "stake_account")?;
        self.state
            .check_stake_deposit_authority(self.stake_deposit_authority.key)?;
        check_address(self.stake_program.key, &stake::program::ID, "stake_program")?;

        let mut stake = self
            .state
            .stake_system
            .get(&self.stake_list.data.as_ref().borrow(), stake_index)?;
        if self.stake_account.to_account_info().key != &stake.stake_account {
            msg!(
                "Stake account {} must match stake_list[{}] = {}. Maybe list layout was changed",
                self.stake_account.to_account_info().key,
                stake_index,
                &stake.stake_account
            );
            return Err(ProgramError::InvalidAccountData);
        }

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        // One more level of protection: need to run setScore(0) before this. I don't know is it really a good idea
        if validator.score != 0 {
            msg!("Emergency unstake validator must have 0 score");
            return Err(ProgramError::InvalidAccountData);
        }

        let unstake_amount = stake.last_update_delegated_lamports;
        msg!("Deactivate whole stake {}", stake.stake_account);
        self.state.with_stake_deposit_authority_seeds(|seeds| {
            invoke_signed(
                &stake::instruction::deactivate_stake(
                    self.stake_account.to_account_info().key,
                    self.stake_deposit_authority.key,
                ),
                &[
                    self.stake_program.clone(),
                    self.stake_account.to_account_info(),
                    self.clock.to_account_info(),
                    self.stake_deposit_authority.clone(),
                ],
                &[seeds],
            )
        })?;

        stake.is_emergency_unstaking = 1;

        // we now consider amount no longer "active" for this specific validator
        validator.active_balance = validator.active_balance.saturating_sub(unstake_amount);
        // and in state totals,
        // move from total_active_balance -> total_cooling_down
        self.state.validator_system.total_active_balance = self
            .state
            .validator_system
            .total_active_balance
            .saturating_sub(unstake_amount);
        self.state.emergency_cooling_down = self
            .state
            .emergency_cooling_down
            .checked_add(unstake_amount)
            .expect("Cooling down overflow");

        // update stake-list & validator-list
        self.state.stake_system.set(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            stake_index,
            stake,
        )?;

        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            validator_index,
            validator,
        )?;

        Ok(())
    }
}
