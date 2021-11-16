use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction, system_program};

use crate::{
    checks::{check_address, check_owner_program},
    AddValidator, ID,
};
//use super::{ValidatorRecord, ValidatorSystem};

impl<'info> AddValidator<'info> {
    pub fn process(&mut self, score: u32) -> ProgramResult {
        self.state
            .validator_system
            .check_validator_manager_authority(self.manager_authority.key)?;
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;
        check_owner_program(
            &self.duplication_flag,
            &system_program::ID,
            "duplication_flag",
        )?;
        check_owner_program(&self.rent_payer, &system_program::ID, "rent_payer")?;
        if !self.rent.is_exempt(self.rent_payer.lamports(), 0) {
            msg!(
                "Rent payer must have at least {} lamports",
                self.rent.minimum_balance(0)
            );
            return Err(ProgramError::InsufficientFunds);
        }
        check_address(
            self.system_program.key,
            &system_program::ID,
            "system_program",
        )?;

        msg!("Add validator {}", self.validator_vote.key);

        let state_address = *self.state.to_account_info().key;
        self.state.validator_system.add(
            &mut self.validator_list.data.borrow_mut(),
            *self.validator_vote.key,
            score,
            &state_address,
            self.duplication_flag.key,
        )?;

        // Mark validator as added
        let validator_record = self.state.validator_system.get(
            &self.validator_list.data.borrow(),
            self.state.validator_system.validator_count() - 1,
        )?;
        validator_record.with_duplication_flag_seeds(
            self.state.to_account_info().key,
            |seeds| {
                invoke_signed(
                    &system_instruction::create_account(
                        self.rent_payer.key,
                        self.duplication_flag.key,
                        self.rent.minimum_balance(0),
                        0,
                        &ID,
                    ),
                    &[
                        self.system_program.clone(),
                        self.rent_payer.clone(),
                        self.duplication_flag.clone(),
                    ],
                    &[seeds],
                )
            },
        )?;

        Ok(())
    }
}
