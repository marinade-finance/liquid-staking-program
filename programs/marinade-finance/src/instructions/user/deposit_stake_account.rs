use anchor_lang::prelude::*;
use anchor_lang::solana_program::stake::instruction::LockupArgs;
use anchor_lang::solana_program::{
    program::invoke, stake, stake::state::StakeAuthorize, system_program,
};
use anchor_spl::stake::{Stake, StakeAccount};
use anchor_spl::token::{mint_to, Mint, MintTo, Token, TokenAccount};

use crate::events::user::DepositStakeAccountEvent;
use crate::state::stake_system::StakeList;
use crate::state::validator_system::ValidatorList;
use crate::{error::MarinadeError, require_lte, state::stake_system::StakeSystem, State, ID};

#[derive(Accounts)]
pub struct DepositStakeAccount<'info> {
    #[account(
        mut,
        has_one = msol_mint
    )]
    pub state: Box<Account<'info, State>>,

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
    pub const WAIT_EPOCHS: u64 = 0; // Accepting fresh/redelegated accounts also because those are mergeable anyways
    pub fn process(&mut self, validator_index: u32) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        // impossible to happen check outside bug (msol mint auth is a PDA)
        require_lte!(
            self.msol_mint.supply,
            self.state.msol_supply,
            MarinadeError::UnregisteredMsolMinted
        );

        // record values for event log
        let user_msol_balance = self.mint_to.amount;
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

        // require the stake to have been created for at least WAIT_EPOCHS = 0 (activation_epoch field contains creation epoch)
        require_gte!(
            self.clock.epoch,
            delegation.activation_epoch + Self::WAIT_EPOCHS,
            MarinadeError::DepositingNotActivatedStake
        );

        // require the stake amount is at least min_stake
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

        let mut validator = self.state.validator_system.get_checked(
            &self.validator_list.to_account_info().data.as_ref().borrow(),
            validator_index,
            &delegation.voter_pubkey,
        )?;
        // record balance for event log
        let validator_active_balance = validator.active_balance;
        // update validator.active_balance
        validator.active_balance += delegation.stake;
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
            &mut self.stake_list.to_account_info().data.as_ref().borrow_mut(),
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

        // record current total_active_balance for the event log
        let total_active_balance = self.state.validator_system.total_active_balance;
        // update total_active_balance
        self.state.validator_system.total_active_balance += delegation.stake;

        emit!(DepositStakeAccountEvent {
            state: self.state.key(),
            stake: self.stake_account.key(),
            delegated: delegation.stake,
            withdrawer: old_withdrawer,
            stake_index: self.state.stake_system.stake_count() - 1,
            validator: delegation.voter_pubkey,
            validator_index,
            validator_active_balance,
            total_active_balance,
            user_msol_balance,
            msol_minted: msol_to_mint,
            total_virtual_staked_lamports,
            msol_supply
        });
        Ok(())
    }
}
