use crate::{
    checks::check_stake_amount_and_validator,
    error::MarinadeError,
    events::crank::{RedelegateEvent, SplitStakeAccountInfo},
    state::{
        stake_system::{StakeRecord, StakeSystem},
        validator_system::ValidatorSystem,
    },
    State,
};
use std::{cmp::min, convert::TryFrom};

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::invoke_signed,
    stake::{self, state::StakeState},
    system_program,
};
use anchor_spl::stake::{withdraw, Stake, StakeAccount, Withdraw};

#[derive(Accounts)]
pub struct ReDelegate<'info> {
    #[account(mut)]
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

    /// CHECK: compared to value stored in list
    pub dest_validator_account: UncheckedAccount<'info>,
    // new stake account to make the reDelegation
    #[account(
        init,
        payer = split_stake_rent_payer,
        space = std::mem::size_of::<StakeState>(),
        owner = stake::program::ID,
    )]
    pub redelegate_stake_account: Account<'info, StakeAccount>,

    pub clock: Sysvar<'info, Clock>,
    /// CHECK: have no CPU budget to parse
    pub stake_history: UncheckedAccount<'info>,
    /// CHECK: have no CPU budget to parse
    pub stake_config: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    pub stake_program: Program<'info, Stake>,
}

impl<'info> ReDelegate<'info> {
    /// Notes on Solana re-delegation of stake account:
    /// 1. In order to redelegate you need to create a new stake account
    /// 2. The old stake account remains in the system until the end of the epoch, cooling down
    /// 3. The new stake account exists at the same time, warming up
    /// 4. It means that the same delegation.stake "exists" in two accounts at the same time, one cooling down, the other warming up
    ///
    /// What we do here:
    /// 1. Check if we need to split the account (partial redelegate) or we are re-delegating the entire stake account
    /// 2. if needed, split the account
    /// 3. re-delegate the stake-account or the splitted account, into the new stake account + new validator
    ///
    pub fn process(
        &mut self,
        stake_index: u32,
        source_validator_index: u32,
        dest_validator_index: u32,
    ) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        require_neq!(
            source_validator_index,
            dest_validator_index,
            MarinadeError::SourceAndDestValidatorsAreTheSame
        );

        // only allow redelegation in the stake/unstake window at the end of the epoch
        {
            let last_slot = EpochSchedule::get()
                .unwrap()
                .get_last_slot_in_epoch(self.clock.epoch);
            require_gte!(
                self.clock.slot,
                last_slot.saturating_sub(self.state.stake_system.slots_for_stake_delta),
                MarinadeError::TooEarlyForStakeDelta
            );
        }

        let mut stake = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;
        let last_update_delegation = stake.last_update_delegated_lamports;

        // check the account is not already in emergency_unstake
        require_eq!(
            stake.is_emergency_unstaking,
            0,
            MarinadeError::StakeAccountIsEmergencyUnstaking
        );

        let mut source_validator = self.state.validator_system.get(
            &self.validator_list.data.as_ref().borrow(),
            source_validator_index,
        )?;
        let source_validator_balance = source_validator.active_balance;

        // check amount currently_staked matched observation (stake is updated)
        // and that the account is delegated to the validator_index sent
        check_stake_amount_and_validator(
            &self.stake_account,
            stake.last_update_delegated_lamports,
            &source_validator.validator_account,
        )?;

        // compute total required stake delta (i128, can be positive or negative)
        let total_stake_delta_i128 = self.state.stake_delta(self.reserve_pda.lamports());
        // compute total target stake (current total active stake +/- delta)
        let total_stake_target_i128 =
            self.state.validator_system.total_active_balance as i128 + total_stake_delta_i128;
        // convert to u64
        let total_stake_target =
            u64::try_from(total_stake_target_i128).expect("total_stake_target+stake_delta");

        // compute target for this particular validator (total_stake_target * score/total_score)
        let source_validator_stake_target = self
            .state
            .validator_system
            .validator_stake_target(&source_validator, total_stake_target)?;
        // if validator is already on-target (or the split will be lower than min_stake), exit now
        if source_validator.active_balance
            < source_validator_stake_target + self.state.stake_system.min_stake
        {
            msg!(
                "Source validator {} stake {} is <= target {} +min_stake",
                source_validator.validator_account,
                source_validator.active_balance,
                source_validator_stake_target
            );
            // return rent for unused accounts
            self.return_rent_unused_stake_account(self.split_stake_account.to_account_info())?;
            self.return_rent_unused_stake_account(self.redelegate_stake_account.to_account_info())?;
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }

        // compute how much we can unstake from this validator, and cap redelegate amount to it
        // can't remove more than needed to reach target, and can't remove more than what's in the account
        let max_redelegate_from_source_account = min(
            source_validator.active_balance - source_validator_stake_target,
            stake.last_update_delegated_lamports,
        );

        // get dest validator from index
        let mut dest_validator = self
            .state
            .validator_system
            .get_checked(
                &self.validator_list.data.as_ref().borrow(),
                dest_validator_index,
                &self.dest_validator_account.key(),
            )
            .map_err(|e| e.with_account_name("dest_validator_account"))?;
        let dest_validator_balance = dest_validator.active_balance;

        // compute dest validator target
        let dest_validator_stake_target = self
            .state
            .validator_system
            .validator_stake_target(&dest_validator, total_stake_target)?;
        // verify: dest validator must be under target
        if dest_validator.active_balance + self.state.stake_system.min_stake
            > dest_validator_stake_target
        {
            msg!(
                "Dest validator {} stake+min_stake {} is > target {}",
                dest_validator.validator_account,
                dest_validator.active_balance,
                dest_validator_stake_target
            );
            // return rent for unused accounts
            self.return_rent_unused_stake_account(self.split_stake_account.to_account_info())?;
            self.return_rent_unused_stake_account(self.redelegate_stake_account.to_account_info())?;
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }

        // compute how much we can stake in dest-validator, and cap redelegate amount to it
        // can't send more than needed to reach target
        let max_space_dest_validator = dest_validator_stake_target - dest_validator.active_balance;
        let redelegate_amount_theoretical =
            min(max_space_dest_validator, max_redelegate_from_source_account);

        // compute how much this particular account will have after split
        // do not use saturating_sub because an underflow here means bug and should panic
        let stake_account_after =
            stake.last_update_delegated_lamports - redelegate_amount_theoretical;
        // select if we redelegate all or if we split first
        // (do not leave less than min_stake in the account)
        let (source_account, redelegate_amount_effective) =
            if stake_account_after < self.state.stake_system.min_stake {
                // redelegate all if what will remain in the account is < min_stake
                msg!("ReDelegate whole stake {}", stake.stake_account);

                // Return back the rent reserve of unused split stake account
                self.return_rent_unused_stake_account(self.split_stake_account.to_account_info())?;

                // TODO: deprecate "is_emergency_unstaking"
                stake.is_emergency_unstaking = 0;
                // all lamports will be moved to the re-delegated account
                let amount_to_redelegate_whole_account = stake.last_update_delegated_lamports;
                // this account will enter redelegate-deactivating mode, all lamports will be sent to the other account
                // so we set last_update_delegated_lamports = 0 because all lamports are gone
                // after completing deactivation, whatever is there minus rent is considered last rewards for the account
                stake.last_update_delegated_lamports = 0;

                // account to redelegate is the whole source account
                (
                    self.stake_account.to_account_info(),
                    amount_to_redelegate_whole_account,
                )
                //
                //
            } else {
                // not whole account,
                // we need to split first
                self.split_stake_for_redelegation(&mut stake, redelegate_amount_theoretical)?;
                // account to redelegate is the splitted account
                (
                    self.split_stake_account.to_account_info(),
                    redelegate_amount_theoretical,
                )
            };

        // redelegate account to dest validator
        let redelegate_instruction = &stake::instruction::redelegate(
            &source_account.key(),
            &self.stake_deposit_authority.key(),
            &self.dest_validator_account.key(),
            &self.redelegate_stake_account.key(),
        )
        .last()
        .unwrap()
        .clone();

        invoke_signed(
            redelegate_instruction,
            &[
                source_account.clone(),
                self.dest_validator_account.to_account_info(),
                self.redelegate_stake_account.to_account_info(),
                self.stake_config.to_account_info(),
                self.stake_deposit_authority.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_DEPOSIT_SEED,
                &[self.state.stake_system.stake_deposit_bump_seed],
            ]],
        )?;

        // add new warming-up re-delegated account to Marinade stake-accounts list
        // warn - the lamports are accounted here, and no longer in the source account
        self.state.stake_system.add(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            &self.redelegate_stake_account.key(),
            redelegate_amount_effective,
            &self.clock,
            0, // is_emergency_unstaking
        )?;

        // we now consider amount no longer "active" for this specific validator
        source_validator.active_balance -= redelegate_amount_effective;
        // it moved to dest-validator
        dest_validator.active_balance += redelegate_amount_effective;

        // update stake-list & validator-list
        self.state.stake_system.set(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            stake_index,
            stake,
        )?;
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            source_validator_index,
            source_validator,
        )?;
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            dest_validator_index,
            dest_validator,
        )?;

        emit!(RedelegateEvent {
            state: self.state.key(),
            epoch: self.clock.epoch,
            stake_index,
            stake_account: self.stake_account.key(),
            last_update_delegation,
            source_validator_index,
            source_validator_vote: source_validator.validator_account,
            source_validator_score: source_validator.score,
            source_validator_balance,
            source_validator_stake_target,
            dest_validator_index,
            dest_validator_vote: dest_validator.validator_account,
            dest_validator_score: dest_validator.score,
            dest_validator_balance,
            dest_validator_stake_target,
            redelegate_amount: redelegate_amount_effective,
            split_stake_account: if source_account.key() == self.split_stake_account.key() {
                Some(SplitStakeAccountInfo {
                    account: self.split_stake_account.key(),
                    index: self.state.stake_system.stake_count() - 2,
                })
            } else {
                None
            },
            redelegate_stake_index: self.state.stake_system.stake_count() - 1,
            redelegate_stake_account: self.redelegate_stake_account.key(),
        });

        Ok(())
    }

    pub fn return_rent_unused_stake_account(
        &self,
        unused_stake_account: AccountInfo<'info>,
    ) -> Result<()> {
        // Return back the rent reserve of unused stake account (split or redelegate reserve)
        withdraw(
            CpiContext::new(
                self.stake_program.to_account_info(),
                Withdraw {
                    stake: unused_stake_account.clone(),
                    withdrawer: unused_stake_account.clone(),
                    to: self.split_stake_rent_payer.to_account_info(),
                    clock: self.clock.to_account_info(),
                    stake_history: self.stake_history.to_account_info(),
                },
            ),
            unused_stake_account.lamports(),
            None,
        )
    }

    #[inline] // separated for readability
    pub fn split_stake_for_redelegation(
        &mut self,
        stake: &mut StakeRecord,
        amount: u64,
    ) -> Result<()> {
        msg!(
            "Split {} lamports from stake {} to {}",
            amount,
            stake.stake_account,
            self.split_stake_account.key(),
        );

        // add the split account as new account to Marinade stake-accounts list
        self.state.stake_system.add(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            &self.split_stake_account.key(),
            0, // this account will be deactivating,
            // all lamports will be moved to the re-delegated account,
            // but even with no lamports, we expect the redelegate-deactivating account to provide rewards at the end of the epoch.
            // After completing deactivation, whatever is there minus rent is considered last rewards for the account
            &self.clock,
            // TODO: deprecate "is_emergency_unstaking"
            0,
        )?;

        // split stake account
        let split_instruction = stake::instruction::split(
            self.stake_account.to_account_info().key,
            &self.stake_deposit_authority.key(),
            amount,
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

        // update amount accounted for source stake account
        stake.last_update_delegated_lamports -= amount;

        Ok(())
    }
}
