//get staking rewards & update mSOL price

use std::ops::{Deref, DerefMut};

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::invoke_signed, system_instruction, sysvar::stake_history,
};
use anchor_spl::stake::{withdraw, Stake, StakeAccount, Withdraw};
use anchor_spl::token::{mint_to, Mint, MintTo, Token};

use crate::{
    error::MarinadeError,
    state::{
        stake_system::{StakeRecord, StakeSystem},
        validator_system::ValidatorSystem,
    },
    State,
};

#[derive(Accounts)]
pub struct UpdateCommon<'info> {
    #[account(
        mut,
        has_one = treasury_msol_account,
        has_one = msol_mint
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
    #[account(mut)]
    pub stake_account: Box<Account<'info, StakeAccount>>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            StakeSystem::STAKE_WITHDRAW_SEED
        ],
        bump = state.stake_system.stake_withdraw_bump_seed
    )]
    pub stake_withdraw_authority: UncheckedAccount<'info>, // for getting non delegated SOLs
    #[account(
        mut,
        seeds = [
            &state.key().to_bytes(),
            State::RESERVE_SEED
        ],
        bump = state.reserve_bump_seed
    )]
    pub reserve_pda: SystemAccount<'info>, // all non delegated SOLs (if some attacker transfers it to stake) are sent to reserve_pda

    #[account(mut)]
    pub msol_mint: Box<Account<'info, Mint>>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            State::MSOL_MINT_AUTHORITY_SEED
        ],
        bump = state.msol_mint_authority_bump_seed
    )]
    pub msol_mint_authority: UncheckedAccount<'info>,
    /// CHECK: in code
    #[account(mut)]
    pub treasury_msol_account: UncheckedAccount<'info>, //receives 1% from staking rewards protocol fee

    pub clock: Sysvar<'info, Clock>,
    /// CHECK: have no CPU budget to parse
    #[account(address = stake_history::ID)]
    pub stake_history: UncheckedAccount<'info>,

    pub stake_program: Program<'info, Stake>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct UpdateActive<'info> {
    pub common: UpdateCommon<'info>,
    /// CHECK: manual account processing
    #[account(
        mut,
        address = common.state.validator_system.validator_list.account,
        constraint = validator_list.data.borrow().as_ref().get(0..8)
            == Some(ValidatorSystem::DISCRIMINATOR)
            @ MarinadeError::InvalidValidatorListDiscriminator,
    )]
    pub validator_list: UncheckedAccount<'info>,
}

impl<'info> Deref for UpdateActive<'info> {
    type Target = UpdateCommon<'info>;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

impl<'info> DerefMut for UpdateActive<'info> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.common
    }
}

#[derive(Accounts)]
pub struct UpdateDeactivated<'info> {
    pub common: UpdateCommon<'info>,

    /// CHECK: not important
    #[account(mut, address = common.state.operational_sol_account)]
    pub operational_sol_account: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

impl<'info> Deref for UpdateDeactivated<'info> {
    type Target = UpdateCommon<'info>;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

impl<'info> DerefMut for UpdateDeactivated<'info> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.common
    }
}

struct BeginOutput {
    stake: StakeRecord,
    is_treasury_msol_ready_for_transfer: bool,
}

impl<'info> UpdateCommon<'info> {
    fn begin(&mut self, stake_index: u32) -> Result<BeginOutput> {
        let is_treasury_msol_ready_for_transfer = self
            .state
            .check_treasury_msol_account(&self.treasury_msol_account)?;

        let virtual_reserve_balance = self
            .state
            .available_reserve_balance
            .checked_add(self.state.rent_exempt_for_token_acc)
            .expect("reserve balance overflow");

        // impossible to happen check outside bug
        if self.reserve_pda.lamports() < virtual_reserve_balance {
            msg!(
                "Warning: Reserve must have {} lamports but got {}",
                virtual_reserve_balance,
                self.reserve_pda.lamports()
            );
        }
        // Update reserve balance
        self.state.available_reserve_balance = self
            .reserve_pda
            .lamports()
            .saturating_sub(self.state.rent_exempt_for_token_acc);
        // Update mSOL supply
        // impossible to happen check outside bug (msol mint auth is a PDA)
        if self.msol_mint.supply > self.state.msol_supply {
            msg!(
                "Warning: mSOL minted {} lamports outside of marinade",
                self.msol_mint.supply - self.state.msol_supply
            );
            self.state.staking_sol_cap = 0;
        }
        self.state.msol_supply = self.msol_mint.supply;

        let stake = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;
        /*if stake.last_update_epoch == self.clock.epoch {
            msg!("Double update for stake {}", stake.stake_account);
            return Ok(()); // Not error. Maybe parallel update artifact
        }*/

        Ok(BeginOutput {
            stake,
            is_treasury_msol_ready_for_transfer,
        })
    }

    pub fn withdraw_to_reserve(&mut self, amount: u64) -> Result<()> {
        if amount > 0 {
            // Move unstaked + rewards for restaking
            withdraw(
                CpiContext::new_with_signer(
                    self.stake_program.to_account_info(),
                    Withdraw {
                        stake: self.stake_account.to_account_info(),
                        withdrawer: self.stake_withdraw_authority.to_account_info(),
                        to: self.reserve_pda.to_account_info(),
                        clock: self.clock.to_account_info(),
                        stake_history: self.stake_history.to_account_info(),
                    },
                    &[&[
                        &self.state.key().to_bytes(),
                        StakeSystem::STAKE_WITHDRAW_SEED,
                        &[self.state.stake_system.stake_withdraw_bump_seed],
                    ]],
                ),
                amount,
                None,
            )?;
            self.state.on_transfer_to_reserve(amount);
        }
        Ok(())
    }

    pub fn mint_to_treasury(&mut self, msol_lamports: u64) -> Result<()> {
        if msol_lamports > 0 {
            mint_to(
                CpiContext::new_with_signer(
                    self.token_program.to_account_info(),
                    MintTo {
                        mint: self.msol_mint.to_account_info(),
                        to: self.treasury_msol_account.to_account_info(),
                        authority: self.msol_mint_authority.to_account_info(),
                    },
                    &[&[
                        &self.state.key().to_bytes(),
                        State::MSOL_MINT_AUTHORITY_SEED,
                        &[self.state.msol_mint_authority_bump_seed],
                    ]],
                ),
                msol_lamports,
            )?;
            self.state.on_msol_mint(msol_lamports);
        }
        Ok(())
    }
}

impl<'info> UpdateActive<'info> {
    /// Compute rewards for a single stake account
    /// take 1% protocol fee for treasury & add the rest to validator_system.total_balance
    /// update mSOL price accordingly
    /// Future optional expansion: Partial: If the stake-account is a fully-deactivated stake account ready to withdraw,
    /// (cool-down period is complete) delete-withdraw the stake-account, send SOL to reserve-account
    //
    // fn update_active()
    pub fn process(&mut self, stake_index: u32, validator_index: u32) -> Result<()> {
        let BeginOutput {
            mut stake,
            is_treasury_msol_ready_for_transfer,
        } = self.begin(stake_index)?;

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        let delegation = self.stake_account.delegation().ok_or_else(|| {
            msg!("Undelegated stake under marinade control!");
            ProgramError::InvalidAccountData
        })?;

        if delegation.voter_pubkey != validator.validator_account {
            msg!(
                "Invalid stake validator index. Need to point into validator {}",
                validator.validator_account
            );
            return Err(Error::from(ProgramError::InvalidInstructionData).with_source(source!()));
        }
        if delegation.deactivation_epoch != std::u64::MAX {
            // is deactivated or deactivating
            msg!(
                "Cooling down stake {}. Please use UpdateCoolingDown",
                self.stake_account.to_account_info().key
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }
        // current lamports amount, to compare with previous
        let delegated_lamports = delegation.stake;

        // we don't consider self.stake_account.meta().unwrap().rent_exempt_reserve as part of the stake
        // the reserve lamports are paid by the marinade-program/bot and return to marinade-program/bot once the account is deleted
        let stake_balance_without_rent = self.stake_account.to_account_info().lamports()
            - self.stake_account.meta().unwrap().rent_exempt_reserve;
        // move all sent by hacker extra SOLs to reserve
        // and mint 100% mSOL to treasury to make admins decide what to do with this (maybe return to sender)
        let extra_lamports = stake_balance_without_rent.saturating_sub(delegated_lamports);
        msg!("Extra lamports in stake balance: {}", extra_lamports);
        self.withdraw_to_reserve(extra_lamports)?;
        if is_treasury_msol_ready_for_transfer {
            let msol_amount = self.state.calc_msol_from_lamports(extra_lamports)?;
            self.mint_to_treasury(msol_amount)?;
        }

        msg!("current staked lamports {}", delegated_lamports);
        if delegated_lamports >= stake.last_update_delegated_lamports {
            // re-delegated by solana rewards
            let rewards = delegated_lamports - stake.last_update_delegated_lamports;
            msg!("Staking rewards: {}", rewards);

            if is_treasury_msol_ready_for_transfer {
                // apply 1% protocol fee on staking rewards (do this before updating validators' balance, so it's 1% at old, lower, price)
                let protocol_rewards_fee = self.state.reward_fee.apply(rewards);
                msg!("protocol_rewards_fee {}", protocol_rewards_fee);
                // compute mSOL amount for protocol_rewards_fee
                let fee_as_msol_amount =
                    self.state.calc_msol_from_lamports(protocol_rewards_fee)?;
                self.mint_to_treasury(fee_as_msol_amount)?;
            }

            // validator active balance is updated with rewards
            validator.active_balance += rewards;
            // validator_system.total_active_balance is updated with re-delegated rewards (this impacts price-calculation)
            self.state.validator_system.total_active_balance += rewards;
        } else {
            //slashed
            let slashed = stake.last_update_delegated_lamports - delegated_lamports;
            msg!("slashed {}", slashed);
            //validator balance is updated with slashed
            validator.active_balance = validator.active_balance.saturating_sub(slashed);
            self.state.validator_system.total_active_balance = self
                .state
                .validator_system
                .total_active_balance
                .saturating_sub(slashed);
        }

        // mark stake-account as visited
        stake.last_update_epoch = self.clock.epoch;
        stake.last_update_delegated_lamports = delegated_lamports;

        //update validator-list
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            validator_index,
            validator,
        )?;

        // self.state.stake_system.updated_during_last_epoch += 1;*/
        // set new mSOL price
        self.state.msol_price = self
            .state
            .calc_lamports_from_msol_amount(State::PRICE_DENOMINATOR)?; // store binary-denominated mSOL price
        self.state.stake_system.set(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            stake_index,
            stake,
        )?;

        assert_eq!(
            self.state.available_reserve_balance + self.state.rent_exempt_for_token_acc,
            self.reserve_pda.lamports()
        );
        Ok(())
    }
}

impl<'info> UpdateDeactivated<'info> {
    /// Compute rewards for a single deactivated stake-account
    /// take 1% protocol fee for treasury & add the rest to validator_system.total_balance
    /// update mSOL price accordingly
    /// Optional Future Expansion: Partial: If the stake-account is a fully-deactivated stake account ready to withdraw,
    /// (cool-down period is complete) delete-withdraw the stake-account, send SOL to reserve-account
    pub fn process(&mut self, stake_index: u32) -> Result<()> {
        let BeginOutput {
            stake,
            is_treasury_msol_ready_for_transfer,
        } = self.begin(stake_index)?;

        let delegation = self
            .stake_account
            .delegation()
            .expect("Undelegated stake under control");
        if delegation.deactivation_epoch == std::u64::MAX {
            msg!(
                "Stake {} is active",
                self.stake_account.to_account_info().key
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }
        // current lamports amount, to compare with previous
        let delegated_lamports = delegation.stake;
        let rent = self.stake_account.meta().unwrap().rent_exempt_reserve;
        let stake_balance_without_rent = self.stake_account.to_account_info().lamports() - rent;

        // move all sent by hacker extra SOLs to reserve
        // and mint 100% mSOL to treasury to make admins decide what to do with this (maybe return to sender)
        let extra_lamports = stake_balance_without_rent.saturating_sub(delegated_lamports);
        msg!("Extra lamports in stake balance: {}", extra_lamports);
        if is_treasury_msol_ready_for_transfer {
            let msol_amount = self.state.calc_msol_from_lamports(extra_lamports)?;
            self.mint_to_treasury(msol_amount)?;
        }

        if delegated_lamports >= stake.last_update_delegated_lamports {
            // if there were rewards, mint treasury fee
            let rewards = delegated_lamports - stake.last_update_delegated_lamports;
            msg!("Staking rewards: {}", rewards);

            if is_treasury_msol_ready_for_transfer {
                // apply 1% protocol fee on staking rewards (do this before updating validators' balance, so it's 1% at old, lower, price)
                let protocol_rewards_fee = self.state.reward_fee.apply(rewards);
                msg!("protocol_rewards_fee {}", protocol_rewards_fee);
                // compute mSOL amount for protocol_rewards_fee
                let fee_as_msol_amount =
                    self.state.calc_msol_from_lamports(protocol_rewards_fee)?;
                self.mint_to_treasury(fee_as_msol_amount)?;
            }
        } else {
            let slashed = stake.last_update_delegated_lamports - delegated_lamports;
            msg!("Slashed {}", slashed);
        }

        // withdraw all to reserve (the stake account will be marked for deletion by the system)
        self.common
            .withdraw_to_reserve(self.stake_account.to_account_info().lamports())?;
        // but send the rent-exempt lamports part to operational_sol_account for the future recreation of this slot's account
        invoke_signed(
            &system_instruction::transfer(
                self.reserve_pda.key,
                self.operational_sol_account.key,
                rent,
            ),
            &[
                self.system_program.to_account_info(),
                self.reserve_pda.to_account_info(),
                self.operational_sol_account.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                State::RESERVE_SEED,
                &[self.state.reserve_bump_seed],
            ]],
        )?;
        self.state.on_transfer_from_reserve(rent)?;

        if stake.is_emergency_unstaking == 0 {
            // remove from delayed_unstake_cooling_down (amount is now in the reserve, is no longer cooling-down)
            self.state.stake_system.delayed_unstake_cooling_down = self
                .state
                .stake_system
                .delayed_unstake_cooling_down
                .checked_sub(stake.last_update_delegated_lamports)
                .ok_or(MarinadeError::CalculationFailure)?;
        } else {
            // remove from emergency_cooling_down (amount is now in the reserve, is no longer cooling-down)
            self.state.emergency_cooling_down = self
                .state
                .emergency_cooling_down
                .checked_sub(stake.last_update_delegated_lamports)
                .ok_or(MarinadeError::CalculationFailure)?;
        }

        // We update mSOL price in case we receive "extra deactivating rewards" after the start of Delayed-unstake.
        // Those rewards went into reserve_pda, are part of mSOL price (benefit all stakers) and even might be re-staked
        // set new mSOL price
        self.state.msol_price = self
            .state
            .calc_lamports_from_msol_amount(State::PRICE_DENOMINATOR)?; // store binary-denominated mSOL price

        //remove deleted stake-account from our list
        self.common.state.stake_system.remove(
            &mut self.common.stake_list.data.as_ref().borrow_mut(),
            stake_index,
        )?;

        Ok(())
    }
}
