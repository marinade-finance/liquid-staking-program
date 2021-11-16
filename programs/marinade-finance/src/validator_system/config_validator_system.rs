use anchor_lang::prelude::*;

impl<'info> crate::ConfigValidatorSystem<'info> {
    pub fn process(&mut self, extra_runs: u32) -> ProgramResult {
        self.state
            .validator_system
            .check_validator_manager_authority(self.manager_authority.key)?;
        self.state.stake_system.extra_stake_delta_runs = extra_runs; // TODO: think about is it stake or validator thing?
        Ok(())
    }
}
