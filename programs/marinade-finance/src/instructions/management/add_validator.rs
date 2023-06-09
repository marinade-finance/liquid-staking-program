use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;

use crate::state::validator_system::ValidatorRecord;
use crate::{error::MarinadeError, state::validator_system::ValidatorSystem, State};

#[derive(Accounts)]
pub struct AddValidator<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(address = state.validator_system.manager_authority)]
    pub manager_authority: Signer<'info>,
    /// CHECK: manual account processing
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
        constraint = validator_list.data.borrow().as_ref().get(0..8)
            == Some(ValidatorSystem::DISCRIMINATOR)
            @ MarinadeError::InvalidValidatorListDiscriminator,
    )]
    pub validator_list: UncheckedAccount<'info>,

    /// CHECK: todo
    pub validator_vote: UncheckedAccount<'info>,

    /// CHECK: no discriminator used
    /// by initializing this account we mark the validator as added
    #[account(
        init, // will ensure it is system account
        payer = rent_payer,
        space = 0,
        seeds = [
            &state.key().to_bytes(),
            ValidatorRecord::DUPLICATE_FLAG_SEED,
            &validator_vote.key().to_bytes(),
        ],
        bump,
    )]
    pub duplication_flag: UncheckedAccount<'info>,
    #[account(
        mut,
        owner = system_program::ID
    )]
    pub rent_payer: Signer<'info>,

    pub clock: Sysvar<'info, Clock>,
    pub rent: Sysvar<'info, Rent>,

    pub system_program: Program<'info, System>,
}

impl<'info> AddValidator<'info> {
    pub fn process(&mut self, score: u32) -> Result<()> {
        msg!("Add validator {}", self.validator_vote.key);

        let state_address = self.state.key();
        self.state.validator_system.add(
            &mut self.validator_list.data.borrow_mut(),
            *self.validator_vote.key,
            score,
            &state_address,
            self.duplication_flag.key,
        )?;

        Ok(())
    }
}
