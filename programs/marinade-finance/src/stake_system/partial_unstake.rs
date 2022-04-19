use crate::{
    checks::{check_owner_program, check_stake_matches_validator},
    stake_system::StakeSystemHelpers,
};

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::{invoke, invoke_signed},
    stake::program as stake_program,
    stake::{self, state::StakeState},
    system_instruction, system_program,
};

use crate::{checks::check_address, PartialUnstake};

impl<'info> PartialUnstake<'info> {
    pub fn process(
        &mut self,
        stake_index: u32,
        validator_index: u32,
        unstake_amount: u64,
    ) -> ProgramResult {
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
        assert!(
            stake.is_emergency_unstaking == 0,
            "already emergency unstaking"
        );

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        // check that the account is delegated to the right validator
        check_stake_matches_validator(&self.stake_account.inner, &validator.validator_account)?;

        msg!(
            "partial unstake {} from account {} current stake {}",
            unstake_amount,
            stake.stake_account,
            stake.last_update_delegated_lamports
        );

        // only allow partial unstake (avoid code duplication with emergency_unstake)
        assert!(
            stake.last_update_delegated_lamports
                >= unstake_amount + 2 * self.state.stake_system.min_stake,
            "not enough stake left. Use emergency_unstake for full unstake"
        );

        // perform partial unstake
        msg!(
            "Deactivate split {} ({} lamports) from stake {}",
            self.split_stake_account.key,
            unstake_amount,
            stake.stake_account
        );

        self.state.stake_system.add(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            self.split_stake_account.key,
            unstake_amount,
            &self.clock,
            1, // is_emergency_unstaking
        )?;

        let stake_account_len = std::mem::size_of::<StakeState>();
        if self.split_stake_account.owner == &system_program::ID {
            // empty account
            invoke(
                &system_instruction::create_account(
                    self.split_stake_rent_payer.key,
                    self.split_stake_account.key,
                    self.rent.minimum_balance(stake_account_len),
                    stake_account_len as u64,
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
            if self.split_stake_account.data_len() < stake_account_len {
                msg!(
                    "Split stake account {} must have at least {} bytes (got {})",
                    self.split_stake_account.key,
                    stake_account_len,
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

            self.state.with_stake_deposit_authority_seeds(|seeds| {
                let split_instruction = stake::instruction::split(
                    self.stake_account.to_account_info().key,
                    self.stake_deposit_authority.key,
                    unstake_amount,
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

            stake.last_update_delegated_lamports -= unstake_amount;
        };

        // we now consider amount no longer "active" for this specific validator
        validator.active_balance -= unstake_amount;
        // and in state totals,
        // move from total_active_balance -> total_cooling_down
        self.state.validator_system.total_active_balance -= unstake_amount;
        self.state.emergency_cooling_down += unstake_amount;

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
