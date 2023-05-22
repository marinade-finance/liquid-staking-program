use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::stake_history;
use anchor_lang::solana_program::{program::invoke_signed, stake, stake::state::StakeState};
use anchor_spl::stake::{withdraw, Stake, StakeAccount, Withdraw};

use crate::{
    error::MarinadeError,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State,
};

#[derive(Accounts)]
pub struct MergeStakes<'info> {
    #[account(mut, has_one = operational_sol_account)]
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
        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        let mut destination_stake_info = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            destination_stake_index,
            self.destination_stake.to_account_info().key,
        )?;
        let destination_delegation = if let Some(delegation) = self.destination_stake.delegation() {
            delegation
        } else {
            msg!(
                "Destination stake {} must be delegated",
                self.destination_stake.to_account_info().key
            );
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        };
        if destination_delegation.deactivation_epoch != std::u64::MAX {
            msg!(
                "Destination stake {} must not be deactivating",
                self.destination_stake.to_account_info().key
            );
        }
        if destination_stake_info.last_update_delegated_lamports != destination_delegation.stake {
            msg!(
                "Destination stake {} is not updated",
                self.destination_stake.to_account_info().key
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }
        if destination_delegation.voter_pubkey != validator.validator_account {
            msg!(
                "Destination validator {} doesn't match {}",
                destination_delegation.voter_pubkey,
                validator.validator_account
            );
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        }
        // Source stake
        let source_stake_info = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            source_stake_index,
            self.source_stake.to_account_info().key,
        )?;
        let source_delegation = if let Some(delegation) = self.source_stake.delegation() {
            delegation
        } else {
            msg!(
                "Source stake {} must be delegated",
                self.source_stake.to_account_info().key
            );
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        };
        if source_delegation.deactivation_epoch != std::u64::MAX {
            msg!(
                "Source stake {} must not be deactivating",
                self.source_stake.to_account_info().key
            );
        }
        if source_stake_info.last_update_delegated_lamports != source_delegation.stake
            || self.source_stake.to_account_info().lamports()
                != source_delegation
                    .stake
                    .checked_add(self.source_stake.meta().unwrap().rent_exempt_reserve)
                    .ok_or(MarinadeError::CalculationFailure)?
        {
            msg!(
                "Source stake {} is not updated",
                self.source_stake.to_account_info().key
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }
        if source_delegation.voter_pubkey != validator.validator_account {
            msg!(
                "Source validator {} doesn't match {}",
                source_delegation.voter_pubkey,
                validator.validator_account
            );
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        }
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
        // reread stake after merging
        let result_stake: StakeState = self
            .destination_stake
            .to_account_info()
            .deserialize_data()
            .map_err(|err| ProgramError::BorshIoError(err.to_string()))?;
        let extra_delegated = result_stake
            .delegation()
            .unwrap()
            .stake
            .checked_sub(destination_stake_info.last_update_delegated_lamports)
            .ok_or(MarinadeError::CalculationFailure)?
            .checked_sub(source_stake_info.last_update_delegated_lamports)
            .ok_or(MarinadeError::CalculationFailure)?;
        let returned_stake_rent = self
            .source_stake
            .meta()
            .unwrap()
            .rent_exempt_reserve
            .checked_sub(extra_delegated)
            .ok_or(MarinadeError::CalculationFailure)?;
        validator.active_balance = validator
            .active_balance
            .checked_add(extra_delegated)
            .ok_or(MarinadeError::CalculationFailure)?;
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            validator_index,
            validator,
        )?;
        self.state.validator_system.total_active_balance = self
            .state
            .validator_system
            .total_active_balance
            .checked_add(extra_delegated)
            .ok_or(MarinadeError::CalculationFailure)?;

        destination_stake_info.last_update_delegated_lamports =
            result_stake.delegation().unwrap().stake;
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
        if extra_delegated > 0 {
            msg!(
                "Extra delegation of {} lamports. TODO: mint some mSOLs for admin in return",
                extra_delegated
            );
        }
        Ok(())
    }
}
