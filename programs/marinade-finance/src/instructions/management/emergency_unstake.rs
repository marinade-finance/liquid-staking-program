use crate::{
    checks::check_stake_amount_and_validator,
    error::MarinadeError,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State,
};

use anchor_lang::prelude::*;
use anchor_spl::stake::{deactivate_stake, DeactivateStake, Stake, StakeAccount};

#[derive(Accounts)]
pub struct EmergencyUnstake<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(
        address = state.validator_system.manager_authority
            @ MarinadeError::InvalidValidatorManager
    )]
    pub validator_manager_authority: Signer<'info>,
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
        address = state.stake_system.stake_list.account,
        constraint = stake_list.data.borrow().as_ref().get(0..8)
            == Some(StakeSystem::DISCRIMINATOR)
            @ MarinadeError::InvalidStakeListDiscriminator,
    )]
    pub stake_list: UncheckedAccount<'info>,
    #[account(mut)]
    pub stake_account: Account<'info, StakeAccount>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            StakeSystem::STAKE_DEPOSIT_SEED
        ],
        bump = state.stake_system.stake_deposit_bump_seed
    )]
    pub stake_deposit_authority: UncheckedAccount<'info>,

    pub clock: Sysvar<'info, Clock>,

    pub stake_program: Program<'info, Stake>,
}

impl<'info> EmergencyUnstake<'info> {
    pub fn process(&mut self, stake_index: u32, validator_index: u32) -> Result<()> {
        self.state.check_not_paused()?;
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
        require_eq!(
            validator.score,
            0,
            MarinadeError::EmergencyUnstakingFromNonZeroScoredValidator
        );

        // check that the account is delegated to the right validator
        check_stake_amount_and_validator(
            &self.stake_account,
            stake.last_update_delegated_lamports,
            &validator.validator_account,
        )?;

        let unstake_amount = stake.last_update_delegated_lamports;
        msg!("Deactivate whole stake {}", stake.stake_account);
        deactivate_stake(CpiContext::new_with_signer(
            self.stake_program.to_account_info(),
            DeactivateStake {
                stake: self.stake_account.to_account_info(),
                staker: self.stake_deposit_authority.to_account_info(),
                clock: self.clock.to_account_info(),
            },
            &[&[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_DEPOSIT_SEED,
                &[self.state.stake_system.stake_deposit_bump_seed],
            ]],
        ))?;

        // check the account is not already in emergency_unstake
        if stake.is_emergency_unstaking != 0 {
            return err!(MarinadeError::StakeAccountIsEmergencyUnstaking);
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
