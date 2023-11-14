use anchor_lang::prelude::*;

use crate::{
    error::MarinadeError,
    events::{management::SetValidatorScoreEvent, U32ValueChange},
    state::validator_system::ValidatorList,
    State,
};

#[derive(Accounts)]
pub struct SetValidatorScore<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(
        address = state.validator_system.manager_authority
            @ MarinadeError::InvalidValidatorManager
    )]
    pub manager_authority: Signer<'info>,
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
    )]
    pub validator_list: Account<'info, ValidatorList>,
}

impl<'info> SetValidatorScore<'info> {
    pub fn process(&mut self, index: u32, validator_vote: Pubkey, score: u32) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        let mut validator = self.state.validator_system.get_checked(
            &self.validator_list.to_account_info().data.borrow(),
            index,
            &validator_vote,
        )?;

        self.state.validator_system.total_validator_score -= validator.score;
        let score_change = {
            let old = validator.score;
            validator.score = score;
            U32ValueChange { old, new: score }
        };
        self.state.validator_system.total_validator_score += score;
        self.state.validator_system.set(
            &mut self.validator_list.to_account_info().data.borrow_mut(),
            index,
            validator,
        )?;

        emit!(SetValidatorScoreEvent {
            state: self.state.key(),
            validator: validator_vote,
            index,
            score_change,
        });

        Ok(())
    }
}
