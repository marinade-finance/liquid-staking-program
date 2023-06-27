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

use crate::events::user::DepositStakeAccountEvent;
use crate::{
    checks::check_owner_program,
    error::MarinadeError,
    require_lte,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State, ID,
};

#[derive(Accounts)]
pub struct DepositStakeAccount<'info> {
    #[account(
        mut,
        has_one = msol_mint
    )]
    pub state: Box<Account<'info, State>>,

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
    pub stake_authority: Signer<'info>,
    /// CHECK: manual account processing, only required if adding validator (if allowed)
    #[account(mut)]
    pub duplication_flag: UncheckedAccount<'info>,
    #[account(
        mut,
        owner = system_program::ID
    )]
    pub rent_payer: Signer<'info>,

    #[account(mut)]
    pub msol_mint: Account<'info, Mint>,
    /// user mSOL Token account to send the mSOL
    #[account(
        mut,
        token::mint = state.msol_mint
    )]
    pub mint_to: Box<Account<'info, TokenAccount>>,

    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            State::MSOL_MINT_AUTHORITY_SEED
        ],
        bump = state.msol_mint_authority_bump_seed
    )]
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
        // impossible to happen check outside bug (msol mint auth is a PDA)
        require_lte!(
            self.msol_mint.supply,
            self.state.msol_supply,
            MarinadeError::UnregisteredMsolMinted
        );

        let total_virtual_staked_lamports = self.state.total_virtual_staked_lamports();
        let msol_supply = self.state.msol_supply;

        let delegation = self.stake_account.delegation().ok_or_else(|| {
            error!(MarinadeError::RequiredDelegatedStake).with_account_name("stake_account")
        })?;

        // require stake is active (deactivation_epoch == u64::MAX)
        require_eq!(
            delegation.deactivation_epoch,
            std::u64::MAX,
            MarinadeError::RequiredActiveStake
        );

        require_gte!(
            self.clock.epoch,
            delegation
                .activation_epoch
                .checked_add(Self::WAIT_EPOCHS)
                .unwrap(),
            MarinadeError::DepositingNotActivatedStake
        );

        require_gte!(
            delegation.stake,
            self.state.stake_system.min_stake,
            MarinadeError::TooLowDelegationInDepositingStake
        );

        // Check that stake account has the right amount of lamports.
        // if there's extra the user should withdraw the extra and try again
        // (some times users send lamports to active stake accounts believing that will top up the account)
        require_eq!(
            self.stake_account.to_account_info().lamports(),
            delegation.stake + self.stake_account.meta().unwrap().rent_exempt_reserve,
            MarinadeError::WrongStakeBalance,
        );

        self.state.check_staking_cap(delegation.stake)?;

        let lockup = self.stake_account.lockup().unwrap();
        // Check Lockup
        if lockup.is_in_force(&self.clock, None) {
            msg!("Can not deposit stake account with lockup");
            return err!(MarinadeError::StakeAccountWithLockup)
                .map_err(|e| e.with_account_name("stake_account"));
        }

        let new_validator_active_balance = if validator_index == self.state.validator_system.validator_count() {
            if self.state.validator_system.auto_add_validator_enabled == 0 {
                return err!(MarinadeError::AutoAddValidatorIsNotEnabled);
            }
            check_owner_program(
                &self.duplication_flag,
                &system_program::ID,
                "duplication_flag",
            )?;
            if !self.rent.is_exempt(self.rent_payer.lamports(), 0) {
                return Err(Error::from(ProgramError::InsufficientFunds)
                    .with_source(source!())
                    .with_account_name("rent_payer")
                    .with_values((self.rent_payer.lamports(), self.rent.minimum_balance(0))));
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
            delegation.stake
        } else {
            let mut validator = self.state.validator_system.get_checked(
                &self.validator_list.data.as_ref().borrow(),
                validator_index,
                &delegation.voter_pubkey,
            )?;

            validator.active_balance = validator
                .active_balance
                .checked_add(delegation.stake)
                .ok_or(MarinadeError::CalculationFailure)?;
            self.state.validator_system.set(
                &mut self.validator_list.data.as_ref().borrow_mut(),
                validator_index,
                validator,
            )?;
            validator.active_balance
        };

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
            // Can not deposit stake already under marinade stake auth. old staker must be different than ours
            require_keys_neq!(
                old_staker,
                new_staker,
                MarinadeError::RedepositingMarinadeStake
            );

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

        let old_withdrawer = self.stake_account.meta().unwrap().authorized.withdrawer;
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
            // Can not deposit stake already under marinade stake auth. old_withdrawer must be different than ours
            require_keys_neq!(
                old_withdrawer,
                new_withdrawer,
                MarinadeError::RedepositingMarinadeStake
            );

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

        self.mint_to.reload()?;
        emit!(DepositStakeAccountEvent {
            state: self.state.key(),
            stake: self.stake_account.key(),
            stake_index: self.state.stake_system.stake_count() - 1,
            validator: delegation.voter_pubkey,
            validator_index,
            delegated: delegation.stake,
            withdrawer: old_withdrawer,
            msol_minted: msol_to_mint,
            new_user_msol_balance: self.mint_to.amount,
            new_validator_active_balance,
            new_total_active_balance: self.state.validator_system.total_active_balance,
            total_virtual_staked_lamports,
            msol_supply
        });
        Ok(())
    }
}
