use anchor_lang::prelude::*;

use crate::{
    error::MarinadeError,
    events::{
        admin::{EmergencyPauseEvent, ResumeEvent},
        U64ValueChange,
    },
    State,
};

// this account struct is used for pause() and resume() instructions (see lib.rs)
#[derive(Accounts)]
pub struct EmergencyPause<'info> {
    #[account(
        mut,
        has_one = pause_authority @ MarinadeError::InvalidPauseAuthority
    )]
    pub state: Account<'info, State>,
    pub pause_authority: Signer<'info>,
}

impl<'info> EmergencyPause<'info> {
    pub fn pause(&mut self) -> Result<()> {
        let old_resume_at_epoch = self.state.resume_at_epoch;

        self.state.pause()?;

        emit!(EmergencyPauseEvent {
            state: self.state.key(),
            current_epoch: Clock::get()?.epoch,
            resume_at_epoch_change: U64ValueChange {
                old: old_resume_at_epoch,
                new: self.state.resume_at_epoch
            },
        });

        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        let old_resume_at_epoch = self.state.resume_at_epoch;

        self.state.resume()?;

        emit!(ResumeEvent {
            state: self.state.key(),
            current_epoch: Clock::get()?.epoch,
            resume_at_epoch_change: U64ValueChange {
                old: old_resume_at_epoch,
                new: self.state.resume_at_epoch
            },
        });

        Ok(())
    }
}
