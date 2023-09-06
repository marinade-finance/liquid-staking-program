use anchor_lang::{prelude::*, system_program};

use crate::{
    error::MarinadeError,
    events::management::RemoveValidatorEvent,
    state::validator_system::{ValidatorList, ValidatorRecord},
    State, ID,
};

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
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
    )]
    pub validator_list: Account<'info, ValidatorList>,
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
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        let validator = self.state.validator_system.get_checked(
            &self.validator_list.to_account_info().data.borrow(),
            index,
            &validator_vote,
        )?;

        require_keys_eq!(
            self.duplication_flag.key(),
            validator.duplication_flag_address(self.state.to_account_info().key),
            MarinadeError::WrongValidatorDuplicationFlag
        );

        self.state.validator_system.remove(
            &mut self
                .validator_list
                .to_account_info()
                .data
                .as_ref()
                .borrow_mut(),
            index,
            validator,
        )?;

        // record for event, then remove all flag-account lamports to remove flag
        let operational_sol_balance = self.operational_sol_account.lamports();
        let rent_return = self.duplication_flag.lamports();
        **self.duplication_flag.try_borrow_mut_lamports()? = 0;
        **self.operational_sol_account.try_borrow_mut_lamports()? += rent_return;
        // Prevent from reviving in the same tx
        self.duplication_flag.assign(&system_program::ID);

        emit!(RemoveValidatorEvent {
            state: self.state.key(),
            validator: validator_vote,
            index,
            operational_sol_balance,
        });

        Ok(())
    }
}
