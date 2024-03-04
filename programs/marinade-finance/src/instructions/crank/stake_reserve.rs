use crate::{
    error::MarinadeError,
    events::crank::StakeReserveEvent,
    state::{
        stake_system::{StakeList, StakeSystem},
        validator_system::ValidatorList,
    },
    State, ID,
};
use anchor_lang::{
    prelude::*,
    system_program::{transfer, Transfer},
};
use anchor_lang::{
    solana_program::{
        log::sol_log_compute_units,
        program::{invoke, invoke_signed},
        stake::{
            self,
            state::{Authorized, Lockup, StakeState},
        },
        sysvar::stake_history,
    },
    system_program,
};
use anchor_spl::stake::{withdraw, Stake, StakeAccount, Withdraw};
use std::convert::TryFrom;

#[derive(Accounts)]
pub struct StakeReserve<'info> {
    #[account(mut)]
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
    /// CHECK: CPI
    #[account(mut)]
    pub validator_vote: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [
            &state.key().to_bytes(),
            State::RESERVE_SEED
        ],
        bump = state.reserve_bump_seed
    )]
    pub reserve_pda: SystemAccount<'info>,
    #[account(
        init,
        payer = rent_payer,
        space = std::mem::size_of::<StakeState>(),
        owner = stake::program::ID,
    )]
    pub stake_account: Account<'info, StakeAccount>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            StakeSystem::STAKE_DEPOSIT_SEED
        ],
        bump = state.stake_system.stake_deposit_bump_seed
    )]
    pub stake_deposit_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        owner = system_program::ID
    )]
    pub rent_payer: Signer<'info>,

    pub clock: Sysvar<'info, Clock>,
    pub epoch_schedule: Sysvar<'info, EpochSchedule>,
    pub rent: Sysvar<'info, Rent>,
    /// CHECK: have no CPU budget to parse
    #[account(address = stake_history::ID)]
    pub stake_history: UncheckedAccount<'info>,
    /// CHECK: CPI
    #[account(address = stake::config::ID)]
    pub stake_config: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    pub stake_program: Program<'info, Stake>,
}

impl<'info> StakeReserve<'info> {
    /// called by the bot
    /// Receives self.stake_account where to stake, normally an empty account (new keypair)
    /// stakes from available delta-stake in data.validator_index
    /// pub fn stake_reserve()
    pub fn process(&mut self, validator_index: u32) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);
        require!(self.state.delinquent_upgrader.is_done(), MarinadeError::DelinquentUpgraderIsNotDone);

        // record for event
        let total_active_balance = self.state.validator_system.total_active_balance;

        let staker = Pubkey::create_program_address(
            &[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_DEPOSIT_SEED,
                &[self.state.stake_system.stake_deposit_bump_seed],
            ],
            &ID,
        )
        .unwrap();

        let withdrawer = Pubkey::create_program_address(
            &[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_WITHDRAW_SEED,
                &[self.state.stake_system.stake_withdraw_bump_seed],
            ],
            &ID,
        )
        .unwrap();

        let reserve_balance = self.reserve_pda.lamports();
        let stake_delta = self.state.stake_delta(reserve_balance);
        if stake_delta <= 0 {
            if stake_delta < 0 {
                msg!(
                    "Must unstake {} instead of staking",
                    u64::try_from(-stake_delta).expect("Stake delta overflow")
                );
            } else {
                msg!("Noting to do");
            }
            self.return_unused_stake_account_rent()?;
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }
        let total_stake_delta = u64::try_from(stake_delta).expect("Stake delta overflow");
        let total_stake_target = total_active_balance.saturating_add(total_stake_delta);

        let mut validator = self
            .state
            .validator_system
            .get_checked(
                &self.validator_list.to_account_info().data.as_ref().borrow(),
                validator_index,
                self.validator_vote.key,
            )
            .map_err(|e| e.with_account_name("validator_vote"))?;
        // record for event
        let validator_active_balance = validator.active_balance;

        if validator.last_stake_delta_epoch == self.clock.epoch {
            // check if we have some extra stake runs allowed
            if self.state.stake_system.extra_stake_delta_runs == 0 {
                msg!(
                    "Double delta stake command for validator {} in epoch {}",
                    validator.validator_account,
                    self.clock.epoch
                );
                self.return_unused_stake_account_rent()?;
                return Ok(()); // Not an error. Don't fail other instructions in tx
            } else {
                // some extra runs allowed. Use one
                self.state.stake_system.extra_stake_delta_runs -= 1;
            }
        }

        let last_slot = self.epoch_schedule.get_last_slot_in_epoch(self.clock.epoch);

        require_gte!(
            self.clock.slot,
            last_slot.saturating_sub(self.state.stake_system.slots_for_stake_delta),
            MarinadeError::TooEarlyForStakeDelta
        );

        let validator_stake_target = self
            .state
            .validator_system
            .validator_stake_target(&validator, total_stake_target)?;

        //verify the validator is under-staked
        if validator_active_balance >= validator_stake_target {
            msg!(
                    "Validator {} has already reached stake target {}. Please stake into another validator",
                    validator.validator_account,
                    validator_stake_target
                );
            self.return_unused_stake_account_rent()?;
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }

        // compute stake_target
        // stake_target = validator_target - validator_actual_balance, at most total_stake_delta
        let stake_target = validator_stake_target
            .saturating_sub(validator_active_balance)
            .min(total_stake_delta);

        // if what's left in stake_delta after this operation is < state.min_stake, take all the remainder from stake_delta
        let stake_target = if total_stake_delta - stake_target < self.state.stake_system.min_stake {
            total_stake_delta
        } else {
            stake_target
        };

        // if the amount to stake is < stake_system.min_stake (e.g. less than 1 SOL)
        // we don't stake to avoid creating a stake account with less than 1 SOL
        if stake_target < self.state.stake_system.min_stake {
            msg!(
                "Resulting stake {} is lower than min stake allowed {}",
                stake_target,
                self.state.stake_system.min_stake
            );
            self.return_unused_stake_account_rent()?;
            return Ok(()); // Not an error. Don't fail other instructions in tx
        }

        // transfer SOL from reserve_pda to the stake-account
        sol_log_compute_units();
        msg!("Transfer to stake account");
        transfer(
            CpiContext::new_with_signer(
                self.system_program.to_account_info(),
                Transfer {
                    from: self.reserve_pda.to_account_info(),
                    to: self.stake_account.to_account_info(),
                },
                &[&[
                    &self.state.key().to_bytes(),
                    State::RESERVE_SEED,
                    &[self.state.reserve_bump_seed],
                ]],
            ),
            stake_target,
        )?;
        self.state.on_transfer_from_reserve(stake_target);

        sol_log_compute_units();
        msg!("Initialize stake");
        invoke(
            &stake::instruction::initialize(
                &self.stake_account.key(),
                &Authorized { staker, withdrawer },
                &Lockup::default(),
            ),
            &[
                self.stake_program.to_account_info(),
                self.stake_account.to_account_info(),
                self.rent.to_account_info(),
            ],
        )?;

        sol_log_compute_units();
        msg!("Delegate stake");
        invoke_signed(
            &stake::instruction::delegate_stake(
                &self.stake_account.key(),
                &staker,
                self.validator_vote.key,
            ),
            &[
                self.stake_program.to_account_info(),
                self.stake_account.to_account_info(),
                self.stake_deposit_authority.to_account_info(),
                self.validator_vote.to_account_info(),
                self.clock.to_account_info(),
                self.stake_history.to_account_info(),
                self.stake_config.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_DEPOSIT_SEED,
                &[self.state.stake_system.stake_deposit_bump_seed],
            ]],
        )?;

        self.state.stake_system.add(
            &mut self.stake_list.to_account_info().data.as_ref().borrow_mut(),
            &self.stake_account.key(),
            stake_target,
            &self.clock,
            false, // is_emergency_unstaking? no
            true, // is_active? yes
        )?;

        // update validator record and store in list
        validator.active_balance += stake_target;
        validator.last_stake_delta_epoch = self.clock.epoch;
        // Any stake-delta activity must activate stake delta mode
        self.state.stake_system.last_stake_delta_epoch = self.clock.epoch;
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
        // update also total_active_balance
        self.state.validator_system.total_active_balance += stake_target;

        emit!(StakeReserveEvent {
            state: self.state.key(),
            epoch: self.clock.epoch,
            stake_index: self.state.stake_system.stake_count() - 1,
            stake_account: self.stake_account.key(),
            validator_index,
            validator_vote: self.validator_vote.key(),
            amount: stake_target,
            total_stake_target,
            validator_stake_target,
            reserve_balance,
            total_active_balance,
            validator_active_balance,
            total_stake_delta,
        });
        Ok(())
    }

    pub fn return_unused_stake_account_rent(&self) -> Result<()> {
        // Return back the rent reserve of unused stake account in case of early return
        withdraw(
            CpiContext::new(
                self.stake_program.to_account_info(),
                Withdraw {
                    stake: self.stake_account.to_account_info(),
                    withdrawer: self.stake_account.to_account_info(),
                    to: self.rent_payer.to_account_info(),
                    clock: self.clock.to_account_info(),
                    stake_history: self.stake_history.to_account_info(),
                },
            ),
            self.stake_account.to_account_info().lamports(),
            None,
        )
    }
}
