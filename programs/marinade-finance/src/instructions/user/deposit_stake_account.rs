use anchor_lang::prelude::*;
use anchor_lang::solana_program::stake::instruction::LockupArgs;
use anchor_lang::solana_program::{
    program::{invoke, invoke_signed},
    stake,
    stake::state::StakeAuthorize,
    system_instruction, system_program,
};
use anchor_spl::stake::{Stake, StakeAccount};
use anchor_spl::token::{mint_to, Mint, MintTo, Token, TokenAccount};

use crate::checks::check_owner_program;
use crate::error::MarinadeError;
use crate::state::stake_system::StakeSystem;
use crate::State;
use crate::ID;

#[derive(Accounts)]
pub struct DepositStakeAccount<'info> {
    #[account(mut, has_one = msol_mint)]
    pub state: Box<Account<'info, State>>,

    /// CHECK: manual account processing
    #[account(mut)]
    pub validator_list: UncheckedAccount<'info>,
    /// CHECK: manual account processing
    #[account(mut)]
    pub stake_list: UncheckedAccount<'info>,

    #[account(mut)]
    pub stake_account: Box<Account<'info, StakeAccount>>,
    pub stake_authority: Signer<'info>,
    /// CHECK: manual account processing, only required if adding validator (if allowed)
    #[account(mut)]
    pub duplication_flag: UncheckedAccount<'info>,
    #[account(mut)]
    #[account(owner = system_program::ID)]
    pub rent_payer: Signer<'info>,

    #[account(mut)]
    pub msol_mint: Account<'info, Mint>,
    /// user mSOL Token account to send the mSOL
    #[account(mut, token::mint = state.msol_mint)]
    pub mint_to: Box<Account<'info, TokenAccount>>,

    /// CHECK: PDA
    #[account(seeds = [&state.key().to_bytes(),
            State::MSOL_MINT_AUTHORITY_SEED],
            bump = state.msol_mint_authority_bump_seed)]
    pub msol_mint_authority: UncheckedAccount<'info>,

    pub clock: Sysvar<'info, Clock>,
    pub rent: Sysvar<'info, Rent>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub stake_program: Program<'info, Stake>,
}

impl<'info> DepositStakeAccount<'info> {
    pub const WAIT_EPOCHS: u64 = 2;
    // fn deposit_stake_account()
    pub fn process(&mut self, validator_index: u32) -> Result<()> {
        self.state
            .validator_system
            .check_validator_list(&self.validator_list)?;
        self.state.stake_system.check_stake_list(&self.stake_list)?;

        // impossible to happen check (msol mint auth is a PDA)
        if self.msol_mint.supply > self.state.msol_supply {
            msg!(
                "Warning: mSOL minted {} lamports outside of marinade",
                self.msol_mint.supply - self.state.msol_supply
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }

        let delegation = self.stake_account.delegation().ok_or_else(|| {
            msg!(
                "Deposited stake {} must be delegated",
                self.stake_account.to_account_info().key
            );
            ProgramError::InvalidAccountData
        })?;

        if delegation.deactivation_epoch != std::u64::MAX {
            msg!(
                "Deposited stake {} must not be cooling down",
                self.stake_account.to_account_info().key
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }

        if self.clock.epoch
            < delegation
                .activation_epoch
                .checked_add(Self::WAIT_EPOCHS)
                .unwrap()
        {
            msg!(
                "Deposited stake {} is not activated yet. Wait for #{} epoch",
                self.stake_account.to_account_info().key,
                delegation
                    .activation_epoch
                    .checked_add(Self::WAIT_EPOCHS)
                    .unwrap()
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }

        if delegation.stake < self.state.stake_system.min_stake {
            msg!(
                "Deposited stake {} has low amount of lamports {}. Need at least {}",
                self.stake_account.to_account_info().key,
                delegation.stake,
                self.state.stake_system.min_stake
            );
            return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
        }

        if self.stake_account.to_account_info().lamports()
            > delegation.stake + self.stake_account.meta().unwrap().rent_exempt_reserve
        {
            msg!(
                "Stake account has {} extra lamports. Please withdraw it and try again",
                self.stake_account.to_account_info().lamports()
                    - (delegation.stake + self.stake_account.meta().unwrap().rent_exempt_reserve)
            );
            return Err(Error::from(ProgramError::Custom(6212)).with_source(source!()));
        }

        self.state.check_staking_cap(delegation.stake)?;

        let lockup = self.stake_account.lockup().unwrap();
        // Check Lockup
        if lockup.is_in_force(&self.clock, None) {
            msg!("Can not deposit stake account with lockup");
            return Err(MarinadeError::AccountWithLockup.into());
        }

        if validator_index == self.state.validator_system.validator_count() {
            if self.state.validator_system.auto_add_validator_enabled == 0 {
                return Err(MarinadeError::InvalidValidator.into());
            }
            check_owner_program(
                &self.duplication_flag,
                &system_program::ID,
                "duplication_flag",
            )?;
            if !self.rent.is_exempt(self.rent_payer.lamports(), 0) {
                msg!(
                    "Rent payer must have at least {} lamports",
                    self.rent.minimum_balance(0)
                );
                return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
            }
            // Add extra validator with 0 score
            let state_address = *self.state.to_account_info().key;
            self.state.validator_system.add_with_balance(
                &mut self.validator_list.data.as_ref().borrow_mut(),
                delegation.voter_pubkey,
                0,
                delegation.stake,
                &state_address,
                self.duplication_flag.key,
            )?;

            // Mark validator as added
            let validator_record = self.state.validator_system.get(
                &self.validator_list.data.as_ref().borrow(),
                self.state.validator_system.validator_count() - 1,
            )?;
            validator_record.with_duplication_flag_seeds(
                self.state.to_account_info().key,
                |seeds| {
                    invoke_signed(
                        &system_instruction::create_account(
                            self.rent_payer.key,
                            self.duplication_flag.key,
                            self.rent.minimum_balance(0),
                            0,
                            &ID,
                        ),
                        &[
                            self.system_program.to_account_info(),
                            self.rent_payer.to_account_info(),
                            self.duplication_flag.to_account_info(),
                        ],
                        &[seeds],
                    )
                },
            )?;
        } else {
            let mut validator = self
                .state
                .validator_system
                .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

            if delegation.voter_pubkey != validator.validator_account {
                msg!(
                "Deposited stake {} is delegated to {} but must be delegated to validator {}. Probably validator list is changed",
                self.stake_account.to_account_info().key, delegation.voter_pubkey, validator.validator_account
                );
                return Err(MarinadeError::InvalidValidator.into());
            }

            validator.active_balance = validator
                .active_balance
                .checked_add(delegation.stake)
                .ok_or(MarinadeError::CalculationFailure)?;
            self.state.validator_system.set(
                &mut self.validator_list.data.as_ref().borrow_mut(),
                validator_index,
                validator,
            )?;
        }

        {
            let new_staker = Pubkey::create_program_address(
                &[
                    &self.state.key().to_bytes(),
                    StakeSystem::STAKE_DEPOSIT_SEED,
                    &[self.state.stake_system.stake_deposit_bump_seed],
                ],
                &ID,
            )
            .unwrap();
            let old_staker = self.stake_account.meta().unwrap().authorized.staker;
            if old_staker == new_staker {
                msg!(
                    "Can not deposit stake {} already under marinade stake auth. Expected staker differs from {}",
                    self.stake_account.to_account_info().key,
                    new_staker
                );
                return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
            }

            // Clean old lockup
            if lockup.custodian != Pubkey::default() {
                invoke(
                    &stake::instruction::set_lockup(
                        &self.stake_account.key(),
                        &LockupArgs {
                            unix_timestamp: Some(0),
                            epoch: Some(0),
                            custodian: Some(Pubkey::default()),
                        },
                        self.stake_authority.key,
                    ),
                    &[
                        self.stake_program.to_account_info(),
                        self.stake_account.to_account_info(),
                        self.stake_authority.to_account_info(),
                    ],
                )?;
            }

            invoke(
                &stake::instruction::authorize(
                    self.stake_account.to_account_info().key,
                    self.stake_authority.key,
                    &new_staker,
                    StakeAuthorize::Staker,
                    None,
                ),
                &[
                    self.stake_program.to_account_info(),
                    self.stake_account.to_account_info(),
                    self.clock.to_account_info(),
                    self.stake_authority.to_account_info(),
                ],
            )?;
        }

        {
            let new_withdrawer = Pubkey::create_program_address(
                &[
                    &self.state.key().to_bytes(),
                    StakeSystem::STAKE_WITHDRAW_SEED,
                    &[self.state.stake_system.stake_withdraw_bump_seed],
                ],
                &ID,
            )
            .unwrap();
            let old_withdrawer = self.stake_account.meta().unwrap().authorized.withdrawer;
            if old_withdrawer == new_withdrawer {
                msg!(
                    "Can not deposit stake {} already under marinade withdraw auth. Expected withdrawer differs from {}",
                    self.stake_account.to_account_info().key,
                    new_withdrawer
                );
                return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
            }

            invoke(
                &stake::instruction::authorize(
                    self.stake_account.to_account_info().key,
                    self.stake_authority.key,
                    &new_withdrawer,
                    StakeAuthorize::Withdrawer,
                    None,
                ),
                &[
                    self.stake_program.to_account_info(),
                    self.stake_account.to_account_info(),
                    self.clock.to_account_info(),
                    self.stake_authority.to_account_info(),
                ],
            )?;
        }

        self.state.stake_system.add(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            self.stake_account.to_account_info().key,
            delegation.stake,
            &self.clock,
            0, // is_emergency_unstaking? no
        )?;

        let msol_to_mint = self.state.calc_msol_from_lamports(delegation.stake)?;

        mint_to(
            CpiContext::new_with_signer(
                self.token_program.to_account_info(),
                MintTo {
                    mint: self.msol_mint.to_account_info(),
                    to: self.mint_to.to_account_info(),
                    authority: self.msol_mint_authority.to_account_info(),
                },
                &[&[
                    &self.state.key().to_bytes(),
                    State::MSOL_MINT_AUTHORITY_SEED,
                    &[self.state.msol_mint_authority_bump_seed],
                ]],
            ),
            msol_to_mint,
        )?;
        self.state.on_msol_mint(msol_to_mint);

        self.state.validator_system.total_active_balance = self
            .state
            .validator_system
            .total_active_balance
            .checked_add(delegation.stake)
            .ok_or(MarinadeError::CalculationFailure)?;

        Ok(())
    }
}
