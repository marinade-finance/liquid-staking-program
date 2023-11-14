use crate::{
    checks::check_stake_amount_and_validator,
    error::MarinadeError,
    state::{
        stake_system::{StakeList, StakeSystem},
        validator_system::ValidatorList,
    },
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
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
    )]
    pub validator_list: Account<'info, ValidatorList>,
    #[account(
        mut,
        address = state.stake_system.stake_list.account,
    )]
    pub stake_list: Account<'info, StakeList>,
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
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        let mut stake = self.state.stake_system.get_checked(
            &self.stake_list.to_account_info().data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;

        let mut validator = self.state.validator_system.get(
            &self.validator_list.to_account_info().data.as_ref().borrow(),
            validator_index,
        )?;

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
        self.state.on_stake_moved(unstake_amount, &self.clock)?;
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
        require_eq!(
            stake.is_emergency_unstaking,
            0,
            MarinadeError::StakeAccountIsEmergencyUnstaking
        );
        stake.is_emergency_unstaking = 1;

        // we now consider amount no longer "active" for this specific validator
        validator.active_balance -= unstake_amount;
        // and in state totals,
        // move from total_active_balance -> total_cooling_down
        self.state.validator_system.total_active_balance -= unstake_amount;
        self.state.emergency_cooling_down += unstake_amount;

        // update stake-list & validator-list
        self.state.stake_system.set(
            &mut self.stake_list.to_account_info().data.as_ref().borrow_mut(),
            stake_index,
            stake,
        )?;

        self.state.validator_system.set(
            &mut self
                .validator_list
                .to_account_info()
                .data
                .as_ref()
                .borrow_mut(),
            validator_index,
            validator,
        )?;

        Ok(())
    }
}
