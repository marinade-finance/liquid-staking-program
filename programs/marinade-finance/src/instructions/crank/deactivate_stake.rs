use crate::{
    checks::check_owner_program,
    error::MarinadeError,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State,
};
use std::convert::TryFrom;

use anchor_lang::{prelude::*, solana_program::sysvar::stake_history};
use anchor_lang::solana_program::{
    program::{invoke, invoke_signed},
    stake::program as stake_program,
    stake::{self, state::StakeState},
    system_instruction, system_program,
};
use anchor_spl::stake::{Stake, StakeAccount};

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
    #[account(mut)]
    pub split_stake_account: Signer<'info>,
    #[account(mut, owner = system_program::ID)]
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
        let mut stake = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;
        // check the account is not already in emergency_unstake
        if stake.is_emergency_unstaking != 0 {
            return Err(crate::MarinadeError::StakeAccountIsEmergencyUnstaking.into());
        }

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        // check that we're in the last slots of the epoch (stake-delta window)
        if self.clock.slot
            < self
                .epoch_schedule
                .get_last_slot_in_epoch(self.clock.epoch)
                .saturating_sub(self.state.stake_system.slots_for_stake_delta)
        {
            msg!(
                "Stake delta is available only last {} slots of epoch",
                self.state.stake_system.slots_for_stake_delta
            );
            return Err(Error::from(ProgramError::Custom(332)).with_source(source!()));
        }

        // compute total required stake delta (i128, must be negative)
        let total_stake_delta_i128 = self.state.stake_delta(self.reserve_pda.lamports());
        msg!("total_stake_delta_i128 {}", total_stake_delta_i128);
        if total_stake_delta_i128 >= 0 {
            msg!("Must stake {} instead of unstaking", total_stake_delta_i128);
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }
        // convert to u64
        let total_unstake_delta =
            u64::try_from(-total_stake_delta_i128).expect("Unstake delta overflow");
        // compute total target stake (current total active stake minus delta)
        let total_stake_target = self
            .state
            .validator_system
            .total_active_balance
            .saturating_sub(total_unstake_delta);

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
        if validator.active_balance <= validator_stake_target {
            msg!(
                "Validator {} has already reached unstake target {}",
                validator.validator_account,
                validator_stake_target
            );
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }
        let unstake_from_validator = validator.active_balance - validator_stake_target;
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

        let unstaked_amount = if stake_account_target < 2 * self.state.stake_system.min_stake {
            // unstake all if what will remain in the account is < twice min_stake
            msg!("Deactivate whole stake {}", stake.stake_account);
            // Do not check and set validator.last_stake_delta_epoch here because it is possible to run
            // multiple deactivate whole stake commands per epoch. Thats why limitation is applicable only for partial deactivation

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

            // Return rent reserve of unused split stake account if it is not empty
            if self.split_stake_account.owner == &stake::program::ID {
                let correct =
                    match bincode::deserialize(&self.split_stake_account.data.as_ref().borrow()) {
                        Ok(StakeState::Uninitialized) => true,
                        _ => {
                            msg!(
                                "Split stake {} rent return problem",
                                self.split_stake_account.key
                            );
                            false
                        }
                    };
                if correct {
                    invoke(
                        &stake::instruction::withdraw(
                            self.split_stake_account.key,
                            self.split_stake_account.key,
                            self.split_stake_rent_payer.key,
                            self.split_stake_account.lamports(),
                            None,
                        ),
                        &[
                            self.stake_program.to_account_info(),
                            self.split_stake_account.to_account_info(),
                            self.split_stake_rent_payer.to_account_info(),
                            self.clock.to_account_info(),
                            self.stake_history.to_account_info(),
                        ],
                    )?;
                }
            }

            stake.last_update_delegated_lamports
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
                self.split_stake_account.key,
                split_amount,
                stake.stake_account
            );

            self.state.stake_system.add(
                &mut self.stake_list.data.as_ref().borrow_mut(),
                self.split_stake_account.key,
                split_amount,
                &self.clock,
                0, // is_emergency_unstaking? no
            )?;

            let stake_accout_len = std::mem::size_of::<StakeState>();
            if self.split_stake_account.owner == &system_program::ID {
                // empty account
                invoke(
                    &system_instruction::create_account(
                        self.split_stake_rent_payer.key,
                        self.split_stake_account.key,
                        self.rent.minimum_balance(stake_accout_len),
                        stake_accout_len as u64,
                        &stake_program::ID,
                    ),
                    &[
                        self.system_program.to_account_info(),
                        self.split_stake_rent_payer.to_account_info(),
                        self.split_stake_account.to_account_info(),
                    ],
                )?;
            } else {
                // ready unitialized stake (needed for testing because solana_program_test does not support system_instruction::create_account)
                check_owner_program(
                    &self.split_stake_account,
                    &stake::program::ID,
                    "split_stake_account",
                )?;
                if self.split_stake_account.data_len() < stake_accout_len {
                    msg!(
                        "Split stake account {} must have at least {} bytes (got {})",
                        self.split_stake_account.key,
                        stake_accout_len,
                        self.split_stake_account.data_len()
                    );
                    return Err(
                        Error::from(ProgramError::InvalidAccountData).with_source(source!())
                    );
                }
                if !self.rent.is_exempt(
                    self.split_stake_account.lamports(),
                    self.split_stake_account.data_len(),
                ) {
                    msg!(
                        "Split stake account {} must be rent-exempt",
                        self.split_stake_account.key
                    );
                    return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
                }
                match bincode::deserialize(&self.split_stake_account.data.as_ref().borrow())
                    .map_err(|err| ProgramError::BorshIoError(err.to_string()))?
                {
                    StakeState::Uninitialized => (),
                    _ => {
                        msg!(
                            "Split stake {} must be uninitialized",
                            self.split_stake_account.key
                        );
                        return Err(
                            Error::from(ProgramError::InvalidAccountData).with_source(source!())
                        );
                    }
                }
            }

            let split_instruction = stake::instruction::split(
                self.stake_account.to_account_info().key,
                self.stake_deposit_authority.key,
                split_amount,
                self.split_stake_account.key,
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

            invoke_signed(
                &stake::instruction::deactivate_stake(
                    self.split_stake_account.to_account_info().key,
                    self.stake_deposit_authority.key,
                ),
                &[
                    self.stake_program.to_account_info(),
                    self.split_stake_account.to_account_info(),
                    self.clock.to_account_info(),
                    self.stake_deposit_authority.to_account_info(),
                ],
                &[&[
                    &self.state.key().to_bytes(),
                    StakeSystem::STAKE_DEPOSIT_SEED,
                    &[self.state.stake_system.stake_deposit_bump_seed],
                ]],
            )?;

            stake.last_update_delegated_lamports -= split_amount;
            split_amount
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
        self.state.stake_system.delayed_unstake_cooling_down = self
            .state
            .stake_system
            .delayed_unstake_cooling_down
            .checked_add(unstaked_amount)
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
