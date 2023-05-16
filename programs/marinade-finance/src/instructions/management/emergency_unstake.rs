use crate::{
    checks::check_stake_amount_and_validator,
    error::MarinadeError,
    state::stake_system::StakeSystem,
    State,
};

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, stake};
use anchor_spl::stake::{Stake, StakeAccount};

#[derive(Accounts)]
pub struct EmergencyUnstake<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(address = state.validator_system.manager_authority)]
    pub validator_manager_authority: Signer<'info>,
    /// CHECK: manual account processing
    #[account(mut)]
    pub validator_list: UncheckedAccount<'info>,
    /// CHECK: manual account processing
    #[account(mut)]
    pub stake_list: UncheckedAccount<'info>,
    #[account(mut)]
    pub stake_account: Account<'info, StakeAccount>,
    /// CHECK: PDA
    #[account(seeds = [&state.key().to_bytes(),
            StakeSystem::STAKE_DEPOSIT_SEED],
            bump = state.stake_system.stake_deposit_bump_seed)]
    pub stake_deposit_authority: UncheckedAccount<'info>,

    pub clock: Sysvar<'info, Clock>,

    pub stake_program: Program<'info, Stake>,
}

impl<'info> EmergencyUnstake<'info> {
    pub fn process(&mut self, stake_index: u32, validator_index: u32) -> Result<()> {
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;
        self.state.stake_system.check_stake_list(&self.stake_list)?;

        let mut stake = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        // One more level of protection: need to run setScore(0) before this. I don't know is it really a good idea
        if validator.score != 0 {
            msg!("Emergency unstake validator must have 0 score");
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }

        // check that the account is delegated to the right validator
        check_stake_amount_and_validator(
            &self.stake_account,
            stake.last_update_delegated_lamports,
            &validator.validator_account,
        )?;

        let unstake_amount = stake.last_update_delegated_lamports;
        msg!("Deactivate whole stake {}", stake.stake_account);
        invoke_signed(
            &stake::instruction::deactivate_stake(
                self.stake_account.to_account_info().key,
                self.stake_deposit_authority.key,
            ),
            &[
                self.stake_program.to_account_info(),
                self.stake_account.to_account_info(),
                self.clock.to_account_info(),
                self.stake_deposit_authority.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_DEPOSIT_SEED,
                &[self.state.stake_system.stake_deposit_bump_seed],
            ]],
        )?;

        // check the account is not already in emergency_unstake
        if stake.is_emergency_unstaking != 0 {
            return Err(crate::MarinadeError::StakeAccountIsEmergencyUnstaking.into());
        }
        stake.is_emergency_unstaking = 1;

        // we now consider amount no longer "active" for this specific validator
        validator.active_balance = validator
            .active_balance
            .checked_sub(unstake_amount)
            .ok_or(MarinadeError::CalculationFailure)?;
        // and in state totals,
        // move from total_active_balance -> total_cooling_down
        self.state.validator_system.total_active_balance = self
            .state
            .validator_system
            .total_active_balance
            .checked_sub(unstake_amount)
            .ok_or(MarinadeError::CalculationFailure)?;
        self.state.emergency_cooling_down = self
            .state
            .emergency_cooling_down
            .checked_add(unstake_amount)
            .expect("Cooling down overflow");

        // update stake-list & validator-list
        self.state.stake_system.set(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            stake_index,
            stake,
        )?;

        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            validator_index,
            validator,
        )?;

        Ok(())
    }
}
