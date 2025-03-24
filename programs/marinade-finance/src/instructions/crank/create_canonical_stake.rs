use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_lang::solana_program::sysvar::stake_history;
use anchor_lang::solana_program::{program::invoke_signed, stake};
use anchor_spl::stake::{Stake, StakeAccount};

use crate::events::crank::CreateCanonicalStakeEvent;
use crate::state::stake_system::StakeList;
use crate::state::validator_system::ValidatorList;
use crate::{error::MarinadeError, state::stake_system::StakeSystem, State};

#[derive(Accounts)]
pub struct CreateCanonicalStake<'info> {
    #[account(
        mut,
        has_one = operational_sol_account
    )]
    pub state: Box<Account<'info, State>>,
    #[account(
        mut,
        address = state.stake_system.stake_list.account,
    )]
    pub stake_list: Account<'info, StakeList>,
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
    )]
    pub validator_list: Account<'info, ValidatorList>,
    /// CHECK: PDA, created via split from source account
    #[account(mut)]
    pub canonical_stake: UncheckedAccount<'info>,
    #[account(mut)]
    pub source_stake: Box<Account<'info, StakeAccount>>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            StakeSystem::STAKE_DEPOSIT_SEED
        ],
        bump = state.stake_system.stake_deposit_bump_seed
    )]
    pub stake_deposit_authority: UncheckedAccount<'info>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            StakeSystem::STAKE_WITHDRAW_SEED
        ],
        bump = state.stake_system.stake_withdraw_bump_seed
    )]
    pub stake_withdraw_authority: UncheckedAccount<'info>,
    /// CHECK: not important
    #[account(mut)]
    pub operational_sol_account: UncheckedAccount<'info>,

    pub clock: Sysvar<'info, Clock>,
    /// CHECK: have no CPU budget to parse
    #[account(address = stake_history::ID)]
    pub stake_history: UncheckedAccount<'info>,

    pub stake_program: Program<'info, Stake>,
    pub system_program: Program<'info, System>,
}

impl<'info> CreateCanonicalStake<'info> {
    pub fn process(
        &mut self,
        source_stake_index: u32,
        validator_index: u32,
    ) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        let validator = self.state.validator_system.get(
            &self.validator_list.to_account_info().data.as_ref().borrow(),
            validator_index,
        )?;

        // record for event
        let validator_active_balance = validator.active_balance;
        let total_active_balance = self.state.validator_system.total_active_balance;
        let operational_sol_balance = self.operational_sol_account.lamports();

        // check canonical account non-existent
        require_keys_neq!(
            *self.canonical_stake.owner,
            *self.stake_program.key,
            MarinadeError::CanonicalStakeAccountAlreadyCreated
        );

        // check derivation
        let (canonical_stake_account, bump) = State::find_canonical_stake_address(&self.state.key(), &validator.validator_account);
        require_keys_eq!(
            *self.canonical_stake.key,
            canonical_stake_account,
            MarinadeError::InvalidCanonicalStakeAccountAddress
        );

        // Source stake
        let source_stake_info = self.state.stake_system.get_checked(
            &self.stake_list.to_account_info().data.as_ref().borrow(),
            source_stake_index,
            self.source_stake.to_account_info().key,
        )?;
        let source_delegation = if let Some(delegation) = self.source_stake.delegation() {
            delegation
        } else {
            return err!(MarinadeError::SourceStakeMustBeDelegated)
                .map_err(|e| e.with_account_name("source_stake"));
        };
        require_eq!(
            source_delegation.deactivation_epoch,
            std::u64::MAX,
            MarinadeError::SourceStakeMustNotBeDeactivating
        );
        require_eq!(
            source_stake_info.last_update_delegated_lamports,
            source_delegation.stake,
            MarinadeError::SourceStakeMustBeUpdated
        );

        require_eq!(
            self.source_stake.to_account_info().lamports(),
            source_delegation.stake + self.source_stake.meta().unwrap().rent_exempt_reserve,
            MarinadeError::SourceStakeMustBeUpdated
        );

        require_keys_eq!(
            source_delegation.voter_pubkey,
            validator.validator_account,
            MarinadeError::InvalidSourceStakeDelegation
        );

        let state_key = self.state.key();
        let canonical_stake_seeds = [
            state_key.as_ref(),
            validator.validator_account.as_ref(),
            State::CANONICAL_STAKE_SEED,
            &[bump],
        ];

        // extra SOL to withdraw
        if self.canonical_stake.lamports() > 0 {
            transfer(
                CpiContext::new_with_signer(
                    self.system_program.to_account_info(),
                    Transfer {
                        from: self.canonical_stake.to_account_info(),
                        to: self.operational_sol_account.to_account_info(),
                    },
                    &[&canonical_stake_seeds],
                ),
                self.canonical_stake.lamports(),
            )?;
        }

        // "split" returns three instructions: allocate, assign, split, so we
        // need to execute them one by one
        let split_instructions = stake::instruction::split(
            self.source_stake.to_account_info().key,
            self.stake_deposit_authority.to_account_info().key,
            self.source_stake.to_account_info().lamports(),
            self.canonical_stake.to_account_info().key,
        );
        let (allocate, assign, split) =
            if let [allocate, assign, split, ..] = &split_instructions[..] {
                (allocate, assign, split)
            } else {
                unreachable!()
            };
        invoke_signed(
            allocate,
            &[self.canonical_stake.to_account_info()],
            &[&canonical_stake_seeds],
        )?;
        invoke_signed(
            assign,
            &[self.canonical_stake.to_account_info()],
            &[&canonical_stake_seeds],
        )?;
        invoke_signed(
            split,
            &[
                self.source_stake.to_account_info(),
                self.canonical_stake.to_account_info(),
                self.stake_deposit_authority.to_account_info(),
            ],
            &[&[
                &self.state.key().as_ref(),
                StakeSystem::STAKE_DEPOSIT_SEED,
                &[self.state.stake_system.stake_deposit_bump_seed],
            ]],
        )?;

        // add new canonical account to Marinade stake-accounts list
        self.state.stake_system.add(
            &mut self.stake_list.to_account_info().data.as_ref().borrow_mut(),
            &self.canonical_stake.key(),
            source_delegation.stake,
            &self.clock,
            0, // is_emergency_unstaking
        )?;

        // Call this last because of index invalidation
        self.state.stake_system.remove(
            &mut self.stake_list.to_account_info().data.as_ref().borrow_mut(),
            source_stake_index,
        )?;
        emit!(CreateCanonicalStakeEvent {
            state: self.state.key(),
            epoch: self.clock.epoch,
            canonical_stake_account,
            source_stake_index,
            source_stake_account: source_stake_info.stake_account,
            last_update_source_stake_delegation: source_stake_info.last_update_delegated_lamports,
            validator_index,
            validator_vote: validator.validator_account,
            validator_active_balance,
            total_active_balance,
            operational_sol_balance,
        });
        Ok(())
    }
}

