use anchor_lang::{prelude::*, solana_program::stake};

use crate::{error::CommonError, FixForcedUnstake, checks::check_owner_program};

impl<'info> FixForcedUnstake<'info> {
    pub fn process(&mut self, stake_index: u32, validator_index: u32) -> ProgramResult {
        if self.state.fix_forced_unstake_upgraded_stakes != u32::MAX {
            msg!("Not upgraded state");
            return Err(ProgramError::Custom(9223));
        }
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;
        self.state.stake_system.check_stake_list(&self.stake_list)?;
        check_owner_program(&self.stake, &stake::program::ID, "stake")?;
        let mut stake = self
            .state
            .stake_system
            .get(&self.stake_list.data.as_ref().borrow(), stake_index)?;
        if self.stake.to_account_info().key != &stake.stake_account {
            msg!(
                "Stake account {} must match stake_list[{}] = {}. Maybe list layout was changed",
                self.stake.to_account_info().key,
                stake_index,
                &stake.stake_account
            );
            return Err(ProgramError::InvalidAccountData);
        }
        let delegation = if let Some(delegation) = self.stake.delegation() {
            delegation
        } else {
            msg!(
                "Stake {} must be delegated",
                self.stake.to_account_info().key
            );
            return Err(ProgramError::InvalidArgument);
        };

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        if delegation.voter_pubkey != validator.validator_account {
            msg!(
                "Invalid stake validator index. Need to point into validator {}",
                validator.validator_account
            );
            return Err(ProgramError::InvalidInstructionData);
        }

        if delegation.deactivation_epoch == std::u64::MAX || stake.state != 0 {
            msg!("Not forced unstake");
            return Err(ProgramError::InvalidAccountData);
        }
        // deactivate this stake for marinade too
        stake.state = 2; // We are unstaking
                         // we now consider amount no longer "active" for this specific validator
        validator.active_balance = validator
            .active_balance
            .checked_sub(stake.last_update_delegated_lamports)
            .ok_or(CommonError::CalculationFailure)?;
        // and in state totals,
        // move from total_active_balance -> total_cooling_down
        self.state.validator_system.total_active_balance = self
            .state
            .validator_system
            .total_active_balance
            .checked_sub(stake.last_update_delegated_lamports)
            .ok_or(CommonError::CalculationFailure)?;
        self.state.stake_system.delayed_unstake_cooling_down = self
            .state
            .stake_system
            .delayed_unstake_cooling_down
            .checked_add(stake.last_update_delegated_lamports)
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
