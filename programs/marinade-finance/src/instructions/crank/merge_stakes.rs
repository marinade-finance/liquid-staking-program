use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::stake_history;
use anchor_lang::solana_program::{program::invoke_signed, stake};
use anchor_spl::stake::{withdraw, Stake, StakeAccount, Withdraw};

use crate::events::crank::MergeStakesEvent;
use crate::{
    error::MarinadeError,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State,
};

#[derive(Accounts)]
pub struct MergeStakes<'info> {
    #[account(
        mut,
        has_one = operational_sol_account
    )]
    pub state: Box<Account<'info, State>>,
    /// CHECK: manual account processing
    #[account(
        mut,
        address = state.stake_system.stake_list.account,
        constraint = stake_list.data.borrow().as_ref().get(0..8)
            == Some(StakeSystem::DISCRIMINATOR)
            @ MarinadeError::InvalidStakeListDiscriminator,
    )]
    pub stake_list: UncheckedAccount<'info>,
    /// CHECK: manual account processing
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
        constraint = validator_list.data.borrow().as_ref().get(0..8)
            == Some(ValidatorSystem::DISCRIMINATOR)
            @ MarinadeError::InvalidValidatorListDiscriminator,
    )]
    pub validator_list: UncheckedAccount<'info>,
    #[account(mut)]
    pub destination_stake: Box<Account<'info, StakeAccount>>,
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
}

impl<'info> MergeStakes<'info> {
    pub fn process(
        &mut self,
        destination_stake_index: u32,
        source_stake_index: u32,
        validator_index: u32,
    ) -> Result<()> {

        self.state.check_paused()?;
        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        // record for event
        let validator_active_balance = validator.active_balance;
        let total_active_balance = self.state.validator_system.total_active_balance;
        let operational_sol_balance = self.operational_sol_account.lamports();

        let mut destination_stake_info = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            destination_stake_index,
            self.destination_stake.to_account_info().key,
        )?;
        let last_update_destination_stake_delegation =
            destination_stake_info.last_update_delegated_lamports;
        let destination_delegation = if let Some(delegation) = self.destination_stake.delegation() {
            delegation
        } else {
            return err!(MarinadeError::DestinationStakeMustBeDelegated)
                .map_err(|e| e.with_account_name("destination_stake"));
        };
        require_eq!(
            destination_delegation.deactivation_epoch,
            std::u64::MAX,
            MarinadeError::DestinationStakeMustNotBeDeactivating
        );
        require_eq!(
            destination_stake_info.last_update_delegated_lamports,
            destination_delegation.stake,
            MarinadeError::DestinationStakeMustBeUpdated
        );

        require_keys_eq!(
            destination_delegation.voter_pubkey,
            validator.validator_account,
            MarinadeError::InvalidDestinationStakeDelegation
        );

        // Source stake
        let source_stake_info = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
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
            source_delegation
                .stake
                .checked_add(self.source_stake.meta().unwrap().rent_exempt_reserve)
                .ok_or(MarinadeError::CalculationFailure)?,
            MarinadeError::SourceStakeMustBeUpdated
        );

        require_keys_eq!(
            source_delegation.voter_pubkey,
            validator.validator_account,
            MarinadeError::InvalidSourceStakeDelegation
        );
        invoke_signed(
            &stake::instruction::merge(
                self.destination_stake.to_account_info().key,
                self.source_stake.to_account_info().key,
                self.stake_deposit_authority.to_account_info().key,
            )[0],
            &[
                self.stake_program.to_account_info(),
                self.destination_stake.to_account_info(),
                self.source_stake.to_account_info(),
                self.clock.to_account_info(),
                self.stake_history.to_account_info(),
                self.stake_deposit_authority.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_DEPOSIT_SEED,
                &[self.state.stake_system.stake_deposit_bump_seed],
            ]],
        )?;
        // reread stake after merging to properly compute extra_delegated
        self.destination_stake.reload()?;
        // extra_delegated = dest.delegation.stake after merge - (dest.last_update_delegated_lamports + source.last_update_delegated_lamports)
        let extra_delegated = self
            .destination_stake
            .delegation()
            .unwrap()
            .stake
            .checked_sub(destination_stake_info.last_update_delegated_lamports)
            .ok_or(MarinadeError::CalculationFailure)?
            .checked_sub(source_stake_info.last_update_delegated_lamports)
            .ok_or(MarinadeError::CalculationFailure)?;
        // Note: if the merge is invoked with 2 activating accounts, or a new account -> activating account,
        // the source account *rent-lamports* are added to the destination account on top of the delegation (extra-delegated).
        // This is not normal operation for the bot, but this instruction is permissionless so anyone can call any time,
        // and so we should consider the case.
        // In normal cases (the bot merging to active accounts) the *rent-lamports* go to dest account *native lamports*, 
        // so the destination account will have double the rent-exempt lamports
        let returned_stake_rent = self
            .source_stake
            .meta()
            .unwrap()
            .rent_exempt_reserve
            .checked_sub(extra_delegated)
            .ok_or(MarinadeError::CalculationFailure)?;
        // update validator.active_balance
        validator.active_balance += extra_delegated;
        // store in list
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            validator_index,
            validator,
        )?;
        // update also total_active_balance
        self.state.validator_system.total_active_balance += extra_delegated;

        destination_stake_info.last_update_delegated_lamports =
            self.destination_stake.delegation().unwrap().stake;
        self.state.stake_system.set(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            destination_stake_index,
            destination_stake_info,
        )?;
        // Call this last because of index invalidation
        self.state.stake_system.remove(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            source_stake_index,
        )?;
        if returned_stake_rent > 0 {
            // withdraw the rent-exempt lamports part of merged stake to operational_sol_account for the future recreation of this slot's account
            withdraw(
                CpiContext::new_with_signer(
                    self.stake_program.to_account_info(),
                    Withdraw {
                        stake: self.destination_stake.to_account_info(),
                        withdrawer: self.stake_withdraw_authority.to_account_info(),
                        to: self.operational_sol_account.to_account_info(),
                        clock: self.clock.to_account_info(),
                        stake_history: self.stake_history.to_account_info(),
                    },
                    &[&[
                        &self.state.key().to_bytes(),
                        StakeSystem::STAKE_WITHDRAW_SEED,
                        &[self.state.stake_system.stake_withdraw_bump_seed],
                    ]],
                ),
                returned_stake_rent,
                None,
            )?;
        }
        emit!(MergeStakesEvent {
            state: self.state.key(),
            epoch: self.clock.epoch,
            destination_stake_index,
            destination_stake_account: destination_stake_info.stake_account,
            last_update_destination_stake_delegation,
            source_stake_index,
            source_stake_account: source_stake_info.stake_account,
            last_update_source_stake_delegation: source_stake_info.last_update_delegated_lamports,
            validator_index,
            validator_vote: validator.validator_account,
            extra_delegated,
            returned_stake_rent,
            validator_active_balance,
            total_active_balance,
            operational_sol_balance,
        });
        Ok(())
    }
}
