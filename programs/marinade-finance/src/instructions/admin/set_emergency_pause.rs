use anchor_lang::{prelude::*};

use crate::{
    error::MarinadeError,
    events::{admin::SetEmergencyPauseEvent},
    State,
};

#[derive(Accounts)]
pub struct SetEmergencyPause<'info> {
    #[account(
        mut,
        has_one = pause_authority @ MarinadeError::InvalidPauseAuthority
    )]
    pub state: Account<'info, State>,
    pub pause_authority: Signer<'info>,
}

impl<'info> SetEmergencyPause<'info> {

    pub fn process(&mut self, value: bool) -> Result<()> {

        let old_resume_at_epoch = self.state.resume_at_epoch;
        if value {
            // pause request
            self.state.pause()
        }
        else {
            // resume request
            self.state.resume()
        }?;

        emit!(SetEmergencyPauseEvent {
            state: self.state.key(),
            old_resume_at_epoch,
            value,
            new_resume_at_epoch: self.state.resume_at_epoch
        });

        Ok(())
    }
}
