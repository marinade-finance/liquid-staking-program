use anchor_lang::{prelude::*};

use crate::{
    error::MarinadeError,
    events::{admin::EmergencyPauseEvent},
    State,
};

#[derive(Accounts)]
pub struct EmergencyPause<'info> {
    #[account(
        mut,
        has_one = pause_authority @ MarinadeError::InvalidPauseAuthority
    )]
    pub state: Account<'info, State>,
    pub pause_authority: Signer<'info>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct EmergencyPauseData {
    pub set_pause: bool
}

impl<'info> EmergencyPause<'info> {

    pub fn process(&mut self, data: EmergencyPauseData) -> Result<()> {

        let resume_at_epoch = self.state.resume_at_epoch;
        if data.set_pause {
            // pause request
            self.state.pause()
        }
        else {
            // resume request
            self.state.resume()
        }?;

        emit!(EmergencyPauseEvent {
            state: self.state.key(),
            resume_at_epoch,
            set_pause: data.set_pause,
            result_resume_at_epoch: self.state.resume_at_epoch
        });

        Ok(())
    }
}
