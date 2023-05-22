use anchor_lang::prelude::*;

use crate::{State, MarinadeError};

#[derive(Accounts)]
pub struct ConfigValidatorSystem<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(
        address = state.validator_system.manager_authority
            @ MarinadeError::InvalidValidatorManager
    )]
    pub manager_authority: Signer<'info>,
}

impl<'info> ConfigValidatorSystem<'info> {
    pub fn process(&mut self, extra_runs: u32) -> Result<()> {
        self.state.stake_system.extra_stake_delta_runs = extra_runs; // TODO: think about is it stake or validator thing?
        Ok(())
    }
}
