use crate::{
    checks::check_stake_amount_and_validator,
    error::MarinadeError,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State,
};
use std::convert::TryFrom;

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
    #[account(
        address = state.validator_system.manager_authority
            @ MarinadeError::InvalidValidatorManager
    )]
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
        validator_index: u32,
        dest_validator_index: u32,
        desired_redelegate_amount: u64,
    ) -> Result<()> {
        assert!(
            desired_redelegate_amount >= self.state.stake_system.min_stake,
            "desired_redelegate_amount too low"
        );
        assert!(
            validator_index != dest_validator_index,
            "validator indexes are the same"
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
        let max_redelegate_from_validator = validator.active_balance - validator_stake_target;
        let re_delegate_amount = if desired_redelegate_amount > max_redelegate_from_validator {
            max_redelegate_from_validator
        } else {
            desired_redelegate_amount
        };
        // compute how much this particular account will have after split
        let stake_account_after = stake
            .last_update_delegated_lamports
            .saturating_sub(re_delegate_amount);

        // get dest validator from index
        let mut dest_validator = self.state.validator_system.get(
            &self.validator_list.data.as_ref().borrow(),
            dest_validator_index,
        )?;
        // verify dest_validator_account matches
        if self.dest_validator_account.key() != dest_validator.validator_account {
            return err!(MarinadeError::IncorrectDestValidatorAccountOrIndex);
        }

        // compute dest validator target
        let dest_validator_stake_target = self
            .state
            .validator_system
            .validator_stake_target(&dest_validator, total_stake_target)?;
        // if added stake will put the validator over target, reject
        if dest_validator.active_balance + re_delegate_amount > dest_validator_stake_target {
            msg!(
                "Dest validator {} stake {} + amount {} is > target {}",
                dest_validator.validator_account,
                dest_validator.active_balance,
                re_delegate_amount,
                dest_validator_stake_target
            );
            return err!(MarinadeError::RedelegateOverTarget);
        }

        // select if we redelegate all or if we split first
        // (do not leave less than min_stake in the account)
        let (source_account, redelegate_amount_from_source_account) =
            if stake_account_after < self.state.stake_system.min_stake {
                // redelegate all if what will remain in the account is < min_stake
                msg!("ReDelegate whole stake {}", stake.stake_account);

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

                // mark as emergency_unstaking, the account will be cooling down
                stake.is_emergency_unstaking = 1;
                // all lamports will be moved to the re-delegated account
                let amount_to_redelegate = stake.last_update_delegated_lamports;
                // this account will enter redelegate-deactivating mode, all lamports will be sent to the other account
                // so we set last_update_delegated_lamports = 0 because all lamports are gone
                // after completing deactivation, whatever is there minus rent is considered last rewards for the account
                stake.last_update_delegated_lamports = 0; // TODO: is is 0 lamports or rent-exempt?

                // account to redelegate is the whole source account
                (self.stake_account.to_account_info(), amount_to_redelegate)
                //
                //
            } else {
                // not whole account,
                // we need to split first
                msg!(
                    "Split {} lamports from stake {} to {}",
                    re_delegate_amount,
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
                    1, // is_emergency_unstaking
                )?;

                // split stake account
                let split_instruction = stake::instruction::split(
                    self.stake_account.to_account_info().key,
                    &self.stake_deposit_authority.key(),
                    re_delegate_amount,
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
                stake.last_update_delegated_lamports -= re_delegate_amount;

                // account to redelegate is the splitted account
                (
                    self.split_stake_account.to_account_info(),
                    re_delegate_amount,
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
                source_account,
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
            redelegate_amount_from_source_account,
            &self.clock,
            0, // is_emergency_unstaking
        )?;

        // we now consider amount no longer "active" for this specific validator
        validator.active_balance -= redelegate_amount_from_source_account;
        // it moved to dest-validator
        dest_validator.active_balance += redelegate_amount_from_source_account;

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
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            dest_validator_index,
            dest_validator,
        )?;

        Ok(())
    }
}
