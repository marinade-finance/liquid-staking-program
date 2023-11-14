use anchor_lang::prelude::*;

use crate::{
    error::MarinadeError,
    events::{admin::ChangeAuthorityEvent, PubkeyValueChange},
    State,
};

#[derive(Accounts)]
pub struct ChangeAuthority<'info> {
    #[account(
        mut,
        has_one = admin_authority @ MarinadeError::InvalidAdminAuthority
    )]
    pub state: Account<'info, State>,
    pub admin_authority: Signer<'info>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct ChangeAuthorityData {
    pub admin: Option<Pubkey>,
    pub validator_manager: Option<Pubkey>,
    pub operational_sol_account: Option<Pubkey>,
    pub treasury_msol_account: Option<Pubkey>,
    pub pause_authority: Option<Pubkey>,
}

impl<'info> ChangeAuthority<'info> {
    pub fn process(&mut self, data: ChangeAuthorityData) -> Result<()> {
        let admin_change = if let Some(admin) = data.admin {
            let old = self.state.admin_authority;
            self.state.admin_authority = admin;
            Some(PubkeyValueChange { old, new: admin })
        } else {
            None
        };

        let validator_manager_change = if let Some(validator_manager) = data.validator_manager {
            let old = self.state.validator_system.manager_authority;
            self.state.validator_system.manager_authority = validator_manager;
            Some(PubkeyValueChange {
                old,
                new: validator_manager,
            })
        } else {
            None
        };

        let operational_sol_account_change =
            if let Some(operational_sol_account) = data.operational_sol_account {
                let old = self.state.operational_sol_account;
                self.state.operational_sol_account = operational_sol_account;
                Some(PubkeyValueChange {
                    old,
                    new: operational_sol_account,
                })
            } else {
                None
            };

        let treasury_msol_account_change =
            if let Some(treasury_msol_account) = data.treasury_msol_account {
                let old = self.state.treasury_msol_account;
                self.state.treasury_msol_account = treasury_msol_account;
                Some(PubkeyValueChange {
                    old,
                    new: treasury_msol_account,
                })
            } else {
                None
            };

        let pause_authority_change = if let Some(pause_authority) = data.pause_authority {
            let old = self.state.pause_authority;
            self.state.pause_authority = pause_authority;
            Some(PubkeyValueChange {
                old,
                new: pause_authority,
            })
        } else {
            None
        };

        emit!(ChangeAuthorityEvent {
            state: self.state.key(),
            admin_change,
            validator_manager_change,
            operational_sol_account_change,
            treasury_msol_account_change,
            pause_authority_change
        });

        Ok(())
    }
}
