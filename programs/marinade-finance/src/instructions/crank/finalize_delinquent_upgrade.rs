use anchor_lang::prelude::*;

use crate::{
    error::MarinadeError,
    state::{delinquent_upgrader::DelinquentUpgraderState, validator_system::ValidatorList},
    State,
};

#[derive(Accounts)]
pub struct FinalizeDelinquentUpgrade<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
    )]
    pub validator_list: Account<'info, ValidatorList>,
}

impl<'info> FinalizeDelinquentUpgrade<'info> {
    pub fn process(&mut self, mut max_validators: u32) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        let (visited_count, delinquent_balance_left) =
            if let DelinquentUpgraderState::IteratingValidators {
                mut visited_count,
                mut delinquent_balance_left,
            } = &self.state.delinquent_upgrader
            {
                while visited_count < self.state.validator_system.validator_count()
                    && max_validators > 0
                {
                    let mut validator = self.state.validator_system.get(
                        &self.validator_list.to_account_info().data.as_ref().borrow(),
                        visited_count,
                    )?;
                    delinquent_balance_left -=
                        validator.active_balance - validator.delinquent_upgrader_active_balance;
                    validator.active_balance = validator.delinquent_upgrader_active_balance;
                    validator.delinquent_upgrader_active_balance = 0;
                    self.state.validator_system.set(
                        &mut self
                            .validator_list
                            .to_account_info()
                            .data
                            .as_ref()
                            .borrow_mut(),
                        visited_count,
                        validator,
                    )?;
                    visited_count += 1;
                    max_validators -= 1;
                }
                (visited_count, delinquent_balance_left)
            } else {
                return err!(MarinadeError::UpgradingInvariantViolation);
            };

        if visited_count == self.state.validator_system.validator_count() {
            require_eq!(
                delinquent_balance_left,
                0,
                MarinadeError::UpgradingInvariantViolation
            );
            self.state.delinquent_upgrader = DelinquentUpgraderState::Done;
        } else {
            self.state.delinquent_upgrader = DelinquentUpgraderState::IteratingValidators {
                visited_count,
                delinquent_balance_left,
            };
        }
        Ok(())
    }
}
