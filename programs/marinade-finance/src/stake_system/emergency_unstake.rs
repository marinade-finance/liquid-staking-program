use crate::{checks::check_owner_program, stake_system::StakeSystemHelpers};

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::{invoke, invoke_signed},
    stake::{self, state::StakeState},
    stake::program as stake_program,
    system_instruction, system_program,
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
        assert!(stake.is_emergency_unstaking==0,"already emergency unstaking");

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        // check currently_staked in this account
        {
            let currently_staked = if let Some(delegation) = self.stake_account.delegation() {
                if delegation.voter_pubkey != validator.validator_account {
                    msg!(
                        "Invalid stake validator index. Need to point into validator {}",
                        validator.validator_account
                    );
                    return Err(ProgramError::InvalidInstructionData);
                }
                delegation.stake
            } else {
                msg!(
                    "Stake {} must be delegated!",
                    self.stake_account.to_account_info().key
                );
                return Err(ProgramError::InvalidAccountData);
            };

            if stake.last_update_delegated_lamports != currently_staked {
                msg!("Deactivation of not updated stake {}", stake.stake_account);
                // Not error, update it after deactivation
                // return Err(ProgramError::InvalidAccountData);
            }
        }


        // Allow partial emergency unstake
        // compute target for this particular validator (total_stake_target * score/total_score)
        let total_stake_target = self
            .state
            .validator_system
            .total_active_balance;

        let validator_stake_target = self
            .state
            .validator_system
            .validator_stake_target(&validator, total_stake_target)?;

        // compute how much we should unstake from this validator
        if validator.active_balance <= validator_stake_target {
            msg!(
                "Validator {} has already reached unstake target {}",
                validator.validator_account,
                validator_stake_target
            );
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }
        let unstake_from_validator = validator.active_balance - validator_stake_target;
        msg!(
            "manual unstake {} from_validator {}",
            unstake_from_validator,
            &validator.validator_account
        );

        // compute how much this particular account should have
        let stake_account_target = stake.last_update_delegated_lamports.saturating_sub(unstake_from_validator);

        let unstake_amount = if stake_account_target < 2 * self.state.stake_system.min_stake {
            // unstake all if what will remain in the account is < twice min_stake
            msg!("Deactivate whole stake {}", stake.stake_account);
            // Do not check and set validator.last_stake_delta_epoch here because it is possible to run
            // multiple deactivate whole stake commands per epoch. Thats why limitation is applicable only for partial deactivation
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
    
            // Return rent reserve of unused split stake account if it is not empty
            if self.split_stake_account.owner == &stake::program::ID {
                let correct =
                    match bincode::deserialize(&self.split_stake_account.data.as_ref().borrow()) {
                        Ok(StakeState::Uninitialized) => true,
                        _ => {
                            msg!(
                                "Split stake {} rent return problem",
                                self.split_stake_account.key
                            );
                            false
                        }
                    };
                if correct {
                    invoke(
                        &stake::instruction::withdraw(
                            self.split_stake_account.key,
                            self.split_stake_account.key,
                            self.split_stake_rent_payer.key,
                            self.split_stake_account.lamports(),
                            None,
                        ),
                        &[
                            self.stake_program.clone(),
                            self.split_stake_account.clone(),
                            self.split_stake_rent_payer.clone(),
                            self.clock.to_account_info(),
                            self.stake_history.to_account_info(),
                        ],
                    )?;
                }
            }

            stake.last_update_delegated_lamports

        } else {
            // we must perform partial unstake
            assert!(stake_account_target < stake.last_update_delegated_lamports);
            let split_amount = stake.last_update_delegated_lamports - stake_account_target;

            msg!(
                "Deactivate split {} ({} lamports) from stake {}",
                self.split_stake_account.key,
                split_amount,
                stake.stake_account
            );

            self.state.stake_system.add(
                &mut self.stake_list.data.as_ref().borrow_mut(),
                self.split_stake_account.key,
                split_amount,
                &self.clock,
                1 // is_emergency_unstaking
            )?;

            let stake_accout_len = std::mem::size_of::<StakeState>();
            if self.split_stake_account.owner == &system_program::ID {
                // empty account
                invoke(
                    &system_instruction::create_account(
                        self.split_stake_rent_payer.key,
                        self.split_stake_account.key,
                        self.rent.minimum_balance(stake_accout_len),
                        stake_accout_len as u64,
                        &stake_program::ID,
                    ),
                    &[
                        self.system_program.clone(),
                        self.split_stake_rent_payer.clone(),
                        self.split_stake_account.clone(),
                    ],
                )?;
            } else {
                // ready uninitialized stake (needed for testing because solana_program_test does not support system_instruction::create_account)
                check_owner_program(
                    &self.split_stake_account,
                    &stake::program::ID,
                    "split_stake_account",
                )?;
                if self.split_stake_account.data_len() < stake_accout_len {
                    msg!(
                        "Split stake account {} must have at least {} bytes (got {})",
                        self.split_stake_account.key,
                        stake_accout_len,
                        self.split_stake_account.data_len()
                    );
                    return Err(ProgramError::InvalidAccountData);
                }
                if !self.rent.is_exempt(
                    self.split_stake_account.lamports(),
                    self.split_stake_account.data_len(),
                ) {
                    msg!(
                        "Split stake account {} must be rent-exempt",
                        self.split_stake_account.key
                    );
                    return Err(ProgramError::InsufficientFunds);
                }
                match bincode::deserialize(&self.split_stake_account.data.as_ref().borrow())
                    .map_err(|err| ProgramError::BorshIoError(err.to_string()))?
                {
                    StakeState::Uninitialized => (),
                    _ => {
                        msg!(
                            "Split stake {} must be uninitialized",
                            self.split_stake_account.key
                        );
                        return Err(ProgramError::InvalidAccountData);
                    }
                }
            }

            self.state.with_stake_deposit_authority_seeds(|seeds| {
                let split_instruction = stake::instruction::split(
                    self.stake_account.to_account_info().key,
                    self.stake_deposit_authority.key,
                    split_amount,
                    self.split_stake_account.key,
                )
                .last()
                .unwrap()
                .clone();
                invoke_signed(
                    &split_instruction,
                    &[
                        self.stake_program.clone(),
                        self.stake_account.to_account_info(),
                        self.split_stake_account.to_account_info(),
                        self.stake_deposit_authority.clone(),
                    ],
                    &[seeds],
                )?;

                invoke_signed(
                    &stake::instruction::deactivate_stake(
                        self.split_stake_account.to_account_info().key,
                        self.stake_deposit_authority.key,
                    ),
                    &[
                        self.stake_program.clone(),
                        self.split_stake_account.to_account_info(),
                        self.clock.to_account_info(),
                        self.stake_deposit_authority.clone(),
                    ],
                    &[seeds],
                )
            })?;

            stake.last_update_delegated_lamports -= split_amount;
            split_amount
        };

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
