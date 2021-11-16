use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::invoke_signed,
    stake::{self, state::StakeState},
};

use crate::{
    checks::{check_address, check_owner_program},
    error::CommonError,
    stake_system::StakeSystemHelpers,
    MergeStakes,
};

impl<'info> MergeStakes<'info> {
    pub fn process(
        &mut self,
        destination_stake_index: u32,
        source_stake_index: u32,
        validator_index: u32,
    ) -> ProgramResult {
        self.state.stake_system.check_stake_list(&self.stake_list)?;
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;
        check_owner_program(
            &self.destination_stake,
            &stake::program::ID,
            "destination_stake",
        )?;
        check_owner_program(&self.source_stake, &stake::program::ID, "source_stake")?;
        self.state
            .check_stake_deposit_authority(self.stake_deposit_authority.to_account_info().key)?;
        self.state
            .check_stake_withdraw_authority(self.stake_withdraw_authority.to_account_info().key)?;

        self.state
            .check_operational_sol_account(self.operational_sol_account.key)?;

        check_address(self.stake_program.key, &stake::program::ID, "stake_program")?;
        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        let mut destination_stake_info = self.state.stake_system.get(
            &self.stake_list.data.as_ref().borrow(),
            destination_stake_index,
        )?;
        let destination_delegation = if let Some(delegation) = self.destination_stake.delegation() {
            delegation
        } else {
            msg!(
                "Destination stake {} must be delegated",
                self.destination_stake.to_account_info().key
            );
            return Err(ProgramError::InvalidArgument);
        };
        if destination_delegation.deactivation_epoch != std::u64::MAX {
            msg!(
                "Destination stake {} must not be deactivating",
                self.destination_stake.to_account_info().key
            );
        }
        if destination_stake_info.last_update_delegated_lamports != destination_delegation.stake {
            msg!(
                "Destination stake {} is not updated",
                self.destination_stake.to_account_info().key
            );
            return Err(ProgramError::InvalidAccountData);
        }
        if destination_delegation.voter_pubkey != validator.validator_account {
            msg!(
                "Destination validator {} doesn't match {}",
                destination_delegation.voter_pubkey,
                validator.validator_account
            );
            return Err(ProgramError::InvalidArgument);
        }
        // Source stake
        let source_stake_info = self
            .state
            .stake_system
            .get(&self.stake_list.data.as_ref().borrow(), source_stake_index)?;
        let source_delegation = if let Some(delegation) = self.source_stake.delegation() {
            delegation
        } else {
            msg!(
                "Source stake {} must be delegated",
                self.source_stake.to_account_info().key
            );
            return Err(ProgramError::InvalidArgument);
        };
        if source_delegation.deactivation_epoch != std::u64::MAX {
            msg!(
                "Source stake {} must not be deactivating",
                self.source_stake.to_account_info().key
            );
        }
        if source_stake_info.last_update_delegated_lamports != source_delegation.stake
            || self.source_stake.to_account_info().lamports()
                != source_delegation
                    .stake
                    .checked_add(self.source_stake.meta().unwrap().rent_exempt_reserve)
                    .ok_or(CommonError::CalculationFailure)?
        {
            msg!(
                "Source stake {} is not updated",
                self.source_stake.to_account_info().key
            );
            return Err(ProgramError::InvalidAccountData);
        }
        if source_delegation.voter_pubkey != validator.validator_account {
            msg!(
                "Source validator {} doesn't match {}",
                source_delegation.voter_pubkey,
                validator.validator_account
            );
            return Err(ProgramError::InvalidArgument);
        }
        self.state.with_stake_deposit_authority_seeds(|seeds| {
            invoke_signed(
                &stake::instruction::merge(
                    self.destination_stake.to_account_info().key,
                    self.source_stake.to_account_info().key,
                    self.stake_deposit_authority.to_account_info().key,
                )[0],
                &[
                    self.stake_program.clone(),
                    self.destination_stake.to_account_info(),
                    self.source_stake.to_account_info(),
                    self.clock.to_account_info(),
                    self.stake_history.to_account_info(),
                    self.stake_deposit_authority.to_account_info(),
                ],
                &[seeds],
            )
        })?;
        // reread stake after merging
        let result_stake: StakeState = self
            .destination_stake
            .to_account_info()
            .deserialize_data()
            .map_err(|err| ProgramError::BorshIoError(err.to_string()))?;
        let extra_delegated = result_stake
            .delegation()
            .unwrap()
            .stake
            .checked_sub(destination_stake_info.last_update_delegated_lamports)
            .ok_or(CommonError::CalculationFailure)?
            .checked_sub(source_stake_info.last_update_delegated_lamports)
            .ok_or(CommonError::CalculationFailure)?;
        let returned_stake_rent = self
            .source_stake
            .meta()
            .unwrap()
            .rent_exempt_reserve
            .checked_sub(extra_delegated)
            .ok_or(CommonError::CalculationFailure)?;
        validator.active_balance = validator
            .active_balance
            .checked_add(extra_delegated)
            .ok_or(CommonError::CalculationFailure)?;
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            validator_index,
            validator,
        )?;
        self.state.validator_system.total_active_balance = self
            .state
            .validator_system
            .total_active_balance
            .checked_add(extra_delegated)
            .ok_or(CommonError::CalculationFailure)?;

        destination_stake_info.last_update_delegated_lamports =
            result_stake.delegation().unwrap().stake;
        self.state.stake_system.set(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            destination_stake_index,
            destination_stake_info,
        )?;
        // Call this last because of index invalidation
        self.state.stake_system.remove(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            source_stake_index,
        )?;
        if returned_stake_rent > 0 {
            self.state.with_stake_withdraw_authority_seeds(|seeds| {
                // withdraw the rent-exempt lamports part of merged stake to operational_sol_account for the future recreation of this slot's account
                invoke_signed(
                    &stake::instruction::withdraw(
                        self.destination_stake.to_account_info().key,
                        self.stake_withdraw_authority.key,
                        self.operational_sol_account.key,
                        returned_stake_rent,
                        None,
                    ),
                    &[
                        self.stake_program.clone(),
                        self.destination_stake.to_account_info(),
                        self.operational_sol_account.clone(),
                        self.clock.to_account_info(),
                        self.stake_history.to_account_info(),
                        self.stake_withdraw_authority.clone(),
                    ],
                    &[seeds],
                )
            })?;
        }
        if extra_delegated > 0 {
            msg!(
                "Extra delegation of {} lamports. TODO: mint some mSOLs for admin in return",
                extra_delegated
            );
        }
        Ok(())
    }
}
