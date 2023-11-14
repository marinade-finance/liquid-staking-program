use anchor_lang::{prelude::*, system_program, Discriminator};

use crate::{
    error::MarinadeError, events::admin::ReallocStakeListEvent, state::stake_system::StakeList,
    State,
};

#[derive(Accounts)]
#[instruction(capacity: u32)]
pub struct ReallocStakeList<'info> {
    #[account(
        mut,
        has_one = admin_authority @ MarinadeError::InvalidAdminAuthority,
    )]
    pub state: Account<'info, State>,
    pub admin_authority: Signer<'info>,
    #[account(
        mut,
        address = state.stake_system.stake_list.account,
        realloc = StakeList::DISCRIMINATOR.len()
            + (state.stake_system.stake_record_size() * capacity) as usize,
        realloc::payer = rent_funds,
        realloc::zero = false,
    )]
    pub stake_list: Account<'info, StakeList>,

    #[account(
        mut,
        owner = system_program::ID,
    )]
    pub rent_funds: Signer<'info>,

    pub system_program: Program<'info, System>,
}

impl<'info> ReallocStakeList<'info> {
    pub fn process(&mut self, capacity: u32) -> Result<()> {
        require_gte!(
            capacity,
            self.state.stake_system.stake_count(),
            MarinadeError::ShrinkingListWithDeletingContents
        );
        emit!(ReallocStakeListEvent {
            state: self.state.key(),
            count: self.state.stake_system.stake_count(),
            new_capacity: capacity
        });
        Ok(())
    }
}
