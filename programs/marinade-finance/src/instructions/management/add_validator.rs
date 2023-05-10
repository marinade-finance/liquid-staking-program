use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction, system_program};

use crate::State;
use crate::ID;

#[derive(Accounts)]
pub struct AddValidator<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(address = state.validator_system.manager_authority)]
    pub manager_authority: Signer<'info>,
    /// CHECK: manual account processing
    #[account(mut)]
    pub validator_list: UncheckedAccount<'info>,

    /// CHECK: todo
    pub validator_vote: UncheckedAccount<'info>,

    #[account(mut)]
    pub duplication_flag: SystemAccount<'info>,
    #[account(mut)]
    #[account(owner = system_program::ID)]
    pub rent_payer: Signer<'info>,

    pub clock: Sysvar<'info, Clock>,
    pub rent: Sysvar<'info, Rent>,

    pub system_program: Program<'info, System>,
}

impl<'info> AddValidator<'info> {
    pub fn process(&mut self, score: u32) -> Result<()> {
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;
        if !self.rent.is_exempt(self.rent_payer.lamports(), 0) {
            msg!(
                "Rent payer must have at least {} lamports",
                self.rent.minimum_balance(0)
            );
            return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
        }

        msg!("Add validator {}", self.validator_vote.key);

        let state_address = *self.state.to_account_info().key;
        self.state.validator_system.add(
            &mut self.validator_list.data.borrow_mut(),
            *self.validator_vote.key,
            score,
            &state_address,
            self.duplication_flag.key,
        )?;

        // Mark validator as added
        let validator_record = self.state.validator_system.get(
            &self.validator_list.data.borrow(),
            self.state.validator_system.validator_count() - 1,
        )?;
        validator_record.with_duplication_flag_seeds(
            self.state.to_account_info().key,
            |seeds| {
                invoke_signed(
                    &system_instruction::create_account(
                        self.rent_payer.key,
                        self.duplication_flag.key,
                        self.rent.minimum_balance(0),
                        0,
                        &ID,
                    ),
                    &[
                        self.system_program.to_account_info(),
                        self.rent_payer.to_account_info(),
                        self.duplication_flag.to_account_info(),
                    ],
                    &[seeds],
                )
            },
        )?;

        Ok(())
    }
}
