use anchor_lang::prelude::*;

use crate::RemoveValidator;

impl<'info> RemoveValidator<'info> {
    pub fn process(&mut self, index: u32, validator_vote: Pubkey) -> ProgramResult {
        self.state
            .validator_system
            .check_validator_manager_authority(self.manager_authority.key)?;
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;

        self.state
            .check_operational_sol_account(self.operational_sol_account.key)?;
        let validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.borrow(), index)?;
        if validator.validator_account != validator_vote {
            msg!("Removing validator index is wrong");
            return Err(ProgramError::InvalidArgument);
        }
        if self.duplication_flag.key
            != &validator.duplication_flag_address(self.state.to_account_info().key)
        {
            msg!(
                "Invalid duplication flag {}. Expected {}",
                self.duplication_flag.key,
                validator.duplication_flag_address(self.state.to_account_info().key)
            );
            return Err(ProgramError::InvalidArgument);
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
