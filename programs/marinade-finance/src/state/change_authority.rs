use anchor_lang::prelude::*;

use crate::{ChangeAuthority, ChangeAuthorityData};

impl<'info> ChangeAuthority<'info> {
    pub fn process(&mut self, data: ChangeAuthorityData) -> ProgramResult {
        self.state.check_admin_authority(self.admin_authority.key)?;

        if let Some(admin) = data.admin {
            self.state.admin_authority = admin;
        }

        if let Some(validator_manager) = data.validator_manager {
            self.state.validator_system.manager_authority = validator_manager;
        }

        if let Some(operational_sol_account) = data.operational_sol_account {
            self.state.operational_sol_account = operational_sol_account;
        }

        if let Some(treasury_msol_account) = data.treasury_msol_account {
            self.state.treasury_msol_account = treasury_msol_account;
        }

        Ok(())
    }
}
