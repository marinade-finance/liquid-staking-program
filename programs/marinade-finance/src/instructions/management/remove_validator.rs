use anchor_lang::prelude::*;

use crate::{error::MarinadeError, state::validator_system::{ValidatorSystem, ValidatorRecord}, State, ID};

#[derive(Accounts)]
#[instruction(index: u32, validator_vote: Pubkey)]
pub struct RemoveValidator<'info> {
    #[account(
        mut,
        has_one = operational_sol_account
    )]
    pub state: Account<'info, State>,
    #[account(
        address = state.validator_system.manager_authority
            @ MarinadeError::InvalidValidatorManager
    )]
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
    /// CHECK: manual account processing
    #[account(
        mut,
        owner = ID,
        rent_exempt = enforce,
        seeds = [
            &state.key().to_bytes(),
            ValidatorRecord::DUPLICATE_FLAG_SEED,
            &validator_vote.to_bytes(),
        ],
        bump,
    )]
    pub duplication_flag: UncheckedAccount<'info>,
    /// CHECK: not important
    #[account(mut)]
    pub operational_sol_account: UncheckedAccount<'info>,
}

impl<'info> RemoveValidator<'info> {
    pub fn process(&mut self, index: u32, validator_vote: Pubkey) -> Result<()> {
        let validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.borrow(), index)?;
        if validator.validator_account != validator_vote {
            msg!("Removing validator index is wrong");
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        }
        if self.duplication_flag.key
            != &validator.duplication_flag_address(self.state.to_account_info().key)
        {
            msg!(
                "Invalid duplication flag {}. Expected {}",
                self.duplication_flag.key,
                validator.duplication_flag_address(self.state.to_account_info().key)
            );
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        }

        self.state.validator_system.remove(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            index,
            validator,
        )?;

        let rent_return = self.duplication_flag.lamports();
        **self.duplication_flag.try_borrow_mut_lamports()? = 0;
        **self.operational_sol_account.try_borrow_mut_lamports()? += rent_return;
        Ok(())
    }
}
