use crate::{
    error::MarinadeError,
    events::crank::{DeactivateStakeEvent, SplitStakeAccountInfo},
    require_lt,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State,
};
use std::convert::TryFrom;

use anchor_lang::solana_program::{
    program::invoke_signed, stake, stake::state::StakeState, system_program,
};
use anchor_lang::{prelude::*, solana_program::sysvar::stake_history};
use anchor_spl::stake::{
    deactivate_stake as solana_deactivate_stake, withdraw,
    DeactivateStake as SolanaDeactivateStake, Stake, StakeAccount, Withdraw,
};

use crate::checks::check_stake_amount_and_validator;

#[derive(Accounts)]
pub struct DeactivateStake<'info> {
    #[account(mut)]
    pub state: Box<Account<'info, State>>,
    // Readonly. For stake delta calculation
    #[account(
        seeds = [
            &state.key().to_bytes(),
            State::RESERVE_SEED
        ],
        bump = state.reserve_bump_seed
    )]
    pub reserve_pda: SystemAccount<'info>,
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
    pub stake_account: Box<Account<'info, StakeAccount>>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            StakeSystem::STAKE_DEPOSIT_SEED
        ],
        bump = state.stake_system.stake_deposit_bump_seed
    )]
    pub stake_deposit_authority: UncheckedAccount<'info>,
    #[account(
        init,
        payer = split_stake_rent_payer,
        space = std::mem::size_of::<StakeState>(),
        owner = stake::program::ID,
    )]
    pub split_stake_account: Account<'info, StakeAccount>,
    #[account(
        mut,
        owner = system_program::ID
    )]
    pub split_stake_rent_payer: Signer<'info>,

    pub clock: Sysvar<'info, Clock>,
    pub rent: Sysvar<'info, Rent>,
    pub epoch_schedule: Sysvar<'info, EpochSchedule>,
    /// CHECK: have no CPU budget to parse
    #[account(address = stake_history::ID)]
    pub stake_history: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    pub stake_program: Program<'info, Stake>,
}

impl<'info> DeactivateStake<'info> {
    //
    // fn deactivate_stake()
    //
    pub fn process(&mut self, stake_index: u32, validator_index: u32) -> Result<()> {
        self.state.check_paused()?;
        let mut stake = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;
        let last_update_stake_delegation = stake.last_update_delegated_lamports;

        // check the account is not already in emergency_unstake
        if stake.is_emergency_unstaking != 0 {
            return err!(MarinadeError::StakeAccountIsEmergencyUnstaking);
        }

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        // check that we're in the last slots of the epoch (stake-delta window)
        require_gte!(
            self.clock.slot,
            self.epoch_schedule
                .get_last_slot_in_epoch(self.clock.epoch)
                .saturating_sub(self.state.stake_system.slots_for_stake_delta),
            MarinadeError::TooEarlyForStakeDelta
        );

        // compute total required stake delta (i128, must be negative)
        let total_stake_delta_i128 = self.state.stake_delta(self.reserve_pda.lamports());
        msg!("total_stake_delta_i128 {}", total_stake_delta_i128);
        require_lt!(
            total_stake_delta_i128,
            0,
            MarinadeError::UnstakingOnPositiveDelta
        );
        // convert to u64
        let total_unstake_delta =
            u64::try_from(-total_stake_delta_i128).expect("Unstake delta overflow");
        // compute total target stake (current total active stake minus delta)
        let total_active_balance = self.state.validator_system.total_active_balance; // record for event
        let total_stake_target = total_active_balance.saturating_sub(total_unstake_delta);

        // check currently_staked in this account & validator vote-key
        check_stake_amount_and_validator(
            &self.stake_account,
            stake.last_update_delegated_lamports,
            &validator.validator_account,
        )?;

        // compute target for this particular validator (total_stake_target * score/total_score)
        let validator_stake_target = self
            .state
            .validator_system
            .validator_stake_target(&validator, total_stake_target)?;

        // compute how much we should unstake from this validator
        let validator_active_balance = validator.active_balance; // record for event
        if validator_active_balance <= validator_stake_target {
            msg!(
                "Validator {} has already reached unstake target {}",
                validator.validator_account,
                validator_stake_target
            );
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }
        let unstake_from_validator = validator_active_balance - validator_stake_target;
        msg!(
            "unstake {} from_validator {}",
            unstake_from_validator,
            &validator.validator_account
        );

        // compute how much this particular account should have
        // making sure we are not trying to unstake more than total_unstake_delta
        let stake_account_target = stake.last_update_delegated_lamports.saturating_sub(
            if unstake_from_validator > total_unstake_delta {
                total_unstake_delta
            } else {
                unstake_from_validator
            },
        );

        let (unstaked_amount, deactivate_whole_stake) =
            if stake_account_target < 2 * self.state.stake_system.min_stake {
                // unstake all if what will remain in the account is < twice min_stake
                msg!("Deactivate whole stake {}", stake.stake_account);
                // Do not check and set validator.last_stake_delta_epoch here because it is possible to run
                // multiple deactivate whole stake commands per epoch. Thats why limitation is applicable only for partial deactivation

                solana_deactivate_stake(CpiContext::new_with_signer(
                    self.stake_program.to_account_info(),
                    SolanaDeactivateStake {
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

                // Return back the rent reserve of unused split stake account
                withdraw(
                    CpiContext::new(
                        self.stake_program.to_account_info(),
                        Withdraw {
                            stake: self.split_stake_account.to_account_info(),
                            withdrawer: self.split_stake_account.to_account_info(),
                            to: self.split_stake_rent_payer.to_account_info(),
                            clock: self.clock.to_account_info(),
                            stake_history: self.stake_history.to_account_info(),
                        },
                    ),
                    self.split_stake_account.to_account_info().lamports(),
                    None,
                )?;

                (stake.last_update_delegated_lamports, true)
            } else {
                // we must perform partial unstake
                // Update validator.last_stake_delta_epoch for split-stakes only because probably we need to unstake multiple whole stakes for the same validator
                if validator.last_stake_delta_epoch == self.clock.epoch {
                    // note: we don't consume self.state.extra_stake_delta_runs
                    // for unstake operations. Once delta stake is initiated
                    // only one unstake per validator is allowed (this maximizes mSOL price increase)
                    msg!(
                        "Double delta stake command for validator {} in epoch {}",
                        validator.validator_account,
                        self.clock.epoch
                    );
                    return Ok(()); // Not an error. Don't fail other instructions in tx
                }
                validator.last_stake_delta_epoch = self.clock.epoch;

                // because previous if's
                // here stake_account_target is < last_update_delegated_lamports,
                // and stake.last_update_delegated_lamports - stake_account_target > 2*min_stake
                // assert anyway in case some bug is introduced in the code above
                let split_amount = stake.last_update_delegated_lamports - stake_account_target;
                assert!(
                    stake_account_target < stake.last_update_delegated_lamports
                        && split_amount <= total_unstake_delta
                );

                msg!(
                    "Deactivate split {} ({} lamports) from stake {}",
                    self.split_stake_account.key(),
                    split_amount,
                    stake.stake_account
                );

                self.state.stake_system.add(
                    &mut self.stake_list.data.as_ref().borrow_mut(),
                    &self.split_stake_account.key(),
                    split_amount,
                    &self.clock,
                    0, // is_emergency_unstaking? no
                )?;

                let split_instruction = stake::instruction::split(
                    self.stake_account.to_account_info().key,
                    self.stake_deposit_authority.key,
                    split_amount,
                    &self.split_stake_account.key(),
                )
                .last()
                .unwrap()
                .clone();
                invoke_signed(
                    &split_instruction,
                    &[
                        self.stake_program.to_account_info(),
                        self.stake_account.to_account_info(),
                        self.split_stake_account.to_account_info(),
                        self.stake_deposit_authority.to_account_info(),
                    ],
                    &[&[
                        &self.state.key().to_bytes(),
                        StakeSystem::STAKE_DEPOSIT_SEED,
                        &[self.state.stake_system.stake_deposit_bump_seed],
                    ]],
                )?;

                solana_deactivate_stake(CpiContext::new_with_signer(
                    self.stake_program.to_account_info(),
                    SolanaDeactivateStake {
                        stake: self.split_stake_account.to_account_info(),
                        staker: self.stake_deposit_authority.to_account_info(),
                        clock: self.clock.to_account_info(),
                    },
                    &[&[
                        &self.state.key().to_bytes(),
                        StakeSystem::STAKE_DEPOSIT_SEED,
                        &[self.state.stake_system.stake_deposit_bump_seed],
                    ]],
                ))?;

                stake.last_update_delegated_lamports -= split_amount;
                (split_amount, false)
            };
        // we now consider amount no longer "active" for this specific validator
        validator.active_balance = validator
            .active_balance
            .checked_sub(unstaked_amount)
            .ok_or(MarinadeError::CalculationFailure)?;
        // Any stake-delta activity must activate stake delta mode
        self.state.stake_system.last_stake_delta_epoch = self.clock.epoch;
        // and in state totals,
        // move from total_active_balance -> total_cooling_down
        self.state.validator_system.total_active_balance = self
            .state
            .validator_system
            .total_active_balance
            .checked_sub(unstaked_amount)
            .ok_or(MarinadeError::CalculationFailure)?;
        // record for event and update
        let delayed_unstake_cooling_down = self.state.stake_system.delayed_unstake_cooling_down;
        self.state.stake_system.delayed_unstake_cooling_down += unstaked_amount;

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

        emit!(DeactivateStakeEvent {
            state: self.state.key(),
            epoch: self.clock.epoch,
            stake_index,
            stake_account: self.stake_account.key(),
            last_update_stake_delegation,
            split_stake_account: if deactivate_whole_stake {
                None
            } else {
                Some(SplitStakeAccountInfo {
                    account: self.split_stake_account.key(),
                    index: self.state.stake_system.stake_count() - 1,
                })
            },
            validator_index,
            validator_vote: validator.validator_account,
            total_stake_target,
            validator_stake_target,
            total_active_balance,
            delayed_unstake_cooling_down,
            validator_active_balance,
            total_unstake_delta,
            unstaked_amount,
        });

        Ok(())
    }
}
