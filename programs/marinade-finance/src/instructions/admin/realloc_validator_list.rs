use anchor_lang::{prelude::*, system_program, Discriminator};

use crate::{error::MarinadeError, state::validator_system::ValidatorList, State, events::admin::ReallocValidatorListEvent};

#[derive(Accounts)]
#[instruction(capacity: u32)]
pub struct ReallocValidatorList<'info> {
    #[account(
        mut,
        has_one = admin_authority @ MarinadeError::InvalidAdminAuthority,
    )]
    pub state: Account<'info, State>,
    pub admin_authority: Signer<'info>,
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
        realloc = ValidatorList::DISCRIMINATOR.len() 
            + (state.validator_system.validator_record_size() * capacity) as usize,
        realloc::payer = rent_funds,
        realloc::zero = false,
    )]
    pub validator_list: Account<'info, ValidatorList>,

    #[account(
        mut,
        owner = system_program::ID,
    )]
    pub rent_funds: Signer<'info>,

    pub system_program: Program<'info, System>,
}

impl<'info> ReallocValidatorList<'info> {
    pub fn process(&mut self, capacity: u32) -> Result<()> {
        require_gte!(capacity, self.state.validator_system.validator_count(), MarinadeError::ShrinkingListWithDeletingContents);
        emit!(ReallocValidatorListEvent {
            state: self.state.key(),
            count: self.state.validator_system.validator_count(),
            new_capacity: capacity
        });
        Ok(())
    }
}
