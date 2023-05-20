use crate::{
    checks::{check_owner_program, check_stake_amount_and_validator},
    error::MarinadeError,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State,
};
use std::convert::TryFrom;

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::{invoke, invoke_signed},
    stake::program as stake_program,
    stake::{self, state::StakeState},
    system_instruction, system_program,
};
use anchor_spl::stake::{Stake, StakeAccount};

#[derive(Accounts)]
pub struct PartialUnstake<'info> {
    #[account(mut)]
    pub state: Box<Account<'info, State>>,
    #[account(address = state.validator_system.manager_authority)]
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
    // Readonly. For stake delta calculation
    #[account(
        seeds = [
            &state.key().to_bytes(),
            State::RESERVE_SEED
        ],
        bump = state.reserve_bump_seed
    )]
    pub reserve_pda: SystemAccount<'info>,
    #[account(mut)]
    pub split_stake_account: Signer<'info>,
    #[account(mut)]
    #[account(owner = system_program::ID)]
    pub split_stake_rent_payer: Signer<'info>,

    pub clock: Sysvar<'info, Clock>,
    pub rent: Sysvar<'info, Rent>,
    /// CHECK: have no CPU budget to parse
    pub stake_history: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    pub stake_program: Program<'info, Stake>,
}

impl<'info> PartialUnstake<'info> {
    pub fn process(
        &mut self,
        stake_index: u32,
        validator_index: u32,
        desired_unstake_amount: u64,
    ) -> Result<()> {
        assert!(
            desired_unstake_amount >= self.state.stake_system.min_stake,
            "desired_unstake_amount too low"
        );

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        let mut stake = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;

        // check the account is not already in emergency_unstake
        if stake.is_emergency_unstaking != 0 {
            return Err(crate::MarinadeError::StakeAccountIsEmergencyUnstaking.into());
        }

        // check amount currently_staked in this account
        // and that the account is delegated to the validator_index sent
        check_stake_amount_and_validator(
            &self.stake_account,
            stake.last_update_delegated_lamports,
            &validator.validator_account,
        )?;

        // compute total required stake delta (i128, must be negative)
        let total_stake_delta_i128 = self.state.stake_delta(self.reserve_pda.lamports());
        // compute total target stake (current total active stake +/- delta)
        let total_stake_target_i128 =
            self.state.validator_system.total_active_balance as i128 + total_stake_delta_i128;
        // convert to u64
        let total_stake_target =
            u64::try_from(total_stake_target_i128).expect("total_stake_target+stake_delta");
        // compute target for this particular validator (total_stake_target * score/total_score)
        let validator_stake_target = self
            .state
            .validator_system
            .validator_stake_target(&validator, total_stake_target)?;
        // if validator is already on-target (or the split will be lower than min_stake), exit now
        if validator.active_balance <= validator_stake_target + self.state.stake_system.min_stake {
            msg!(
                "Current validator {} stake {} is <= target {} +min_stake",
                validator.validator_account,
                validator.active_balance,
                validator_stake_target
            );
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }

        // compute how much we can unstake from this validator, and cap unstake amount to it
        let max_unstake_from_validator = validator.active_balance - validator_stake_target;
        let unstake_amount = if desired_unstake_amount > max_unstake_from_validator {
            max_unstake_from_validator
        } else {
            desired_unstake_amount
        };

        // compute how much this particular account will have after unstake
        let stake_account_after = stake
            .last_update_delegated_lamports
            .saturating_sub(unstake_amount);

        let unstaked_from_account = if stake_account_after < self.state.stake_system.min_stake {
            // unstake all if what will remain in the account is < min_stake
            msg!("Deactivate whole stake {}", stake.stake_account);
            // Do not check and set validator.last_stake_delta_epoch here because it is possible to run
            // multiple deactivate whole stake commands per epoch. Thats why limitation is applicable only for partial deactivation

            // deactivate stake account

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

            // mark as emergency_unstaking, so the SOL will be re-staked ASAP
            stake.is_emergency_unstaking = 1;

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

            // effective unstaked_from_account
            stake.last_update_delegated_lamports
        } else {
            // we must perform partial unstake of unstake_amount

            msg!(
                "Deactivate split {} ({} lamports) from stake {}",
                self.split_stake_account.key,
                unstake_amount,
                stake.stake_account
            );

            // add new account to Marinade stake-accounts list
            self.state.stake_system.add(
                &mut self.stake_list.data.as_ref().borrow_mut(),
                self.split_stake_account.key,
                unstake_amount,
                &self.clock,
                1, // is_emergency_unstaking
            )?;

            let stake_account_len = std::mem::size_of::<StakeState>();
            if self.split_stake_account.owner == &system_program::ID {
                // empty account
                invoke(
                    &system_instruction::create_account(
                        self.split_stake_rent_payer.key,
                        self.split_stake_account.key,
                        self.rent.minimum_balance(stake_account_len),
                        stake_account_len as u64,
                        &stake_program::ID,
                    ),
                    &[
                        self.system_program.to_account_info(),
                        self.split_stake_rent_payer.to_account_info(),
                        self.split_stake_account.to_account_info(),
                    ],
                )?;
            } else {
                // ready uninitialized stake (needed for testing because solana_program_test does not support system_instruction::create_account)
                check_owner_program(
                    &self.split_stake_account,
                    &stake::program::ID,
                    "split_stake_account",
                )?;
                if self.split_stake_account.data_len() < stake_account_len {
                    msg!(
                        "Split stake account {} must have at least {} bytes (got {})",
                        self.split_stake_account.key,
                        stake_account_len,
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

            // split & deactivate stake account
            let split_instruction = stake::instruction::split(
                self.stake_account.to_account_info().key,
                self.stake_deposit_authority.key,
                unstake_amount,
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

            // update amount accounted for this account
            stake.last_update_delegated_lamports -= unstake_amount;

            // effective unstaked_from_account
            unstake_amount
        };

        // we now consider amount no longer "active" for this specific validator
        validator.active_balance -= unstaked_from_account;
        // and in state totals,
        // move from total_active_balance -> total_cooling_down
        self.state.validator_system.total_active_balance -= unstaked_from_account;
        self.state.emergency_cooling_down += unstaked_from_account;

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
