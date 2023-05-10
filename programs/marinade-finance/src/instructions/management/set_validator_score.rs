use anchor_lang::prelude::*;

use crate::{error::MarinadeError, State};

#[derive(Accounts)]
pub struct SetValidatorScore<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(address = state.validator_system.manager_authority)]
    pub manager_authority: Signer<'info>,
    /// CHECK: manual account processing
    #[account(mut)]
    pub validator_list: UncheckedAccount<'info>,
}

impl<'info> SetValidatorScore<'info> {
    pub fn process(&mut self, index: u32, validator_vote: Pubkey, score: u32) -> Result<()> {
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
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        }

        self.state.validator_system.total_validator_score = self
            .state
            .validator_system
            .total_validator_score
            .checked_sub(validator.score)
            .ok_or(MarinadeError::CalculationFailure)?;
        validator.score = score;
        self.state.validator_system.total_validator_score = self
            .state
            .validator_system
            .total_validator_score
            .checked_add(score)
            .ok_or(MarinadeError::CalculationFailure)?;
        self.state.validator_system.set(
            &mut self.validator_list.data.borrow_mut(),
            index,
            validator,
        )?;

        Ok(())
    }
}
