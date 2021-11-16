use anchor_lang::prelude::*;

use crate::{error::CommonError, SetValidatorScore};

impl<'info> SetValidatorScore<'info> {
    pub fn process(&mut self, index: u32, validator_vote: Pubkey, score: u32) -> ProgramResult {
        self.state
            .validator_system
            .check_validator_manager_authority(self.manager_authority.key)?;
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.borrow(), index)?;
        if validator.validator_account != validator_vote {
            msg!(
                "Wrong validator {}. Validator #{} must be {}",
                validator_vote,
                index,
                validator.validator_account
            );
            return Err(ProgramError::InvalidArgument);
        }

        self.state.validator_system.total_validator_score = self
            .state
            .validator_system
            .total_validator_score
            .checked_sub(validator.score)
            .ok_or(CommonError::CalculationFailure)?;
        validator.score = score;
        self.state.validator_system.total_validator_score = self
            .state
            .validator_system
            .total_validator_score
            .checked_add(score)
            .ok_or(CommonError::CalculationFailure)?;
        self.state.validator_system.set(
            &mut self.validator_list.data.borrow_mut(),
            index,
            validator,
        )?;

        Ok(())
    }
}
