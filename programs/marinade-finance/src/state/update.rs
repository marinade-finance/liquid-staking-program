//get staking rewards & update mSOL price

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    program::invoke_signed, stake, system_instruction, system_program,
};
use anchor_spl::token::{mint_to, MintTo};

use crate::{
    checks::check_address,
    stake_system::{StakeRecord, StakeSystemHelpers},
    state::StateHelpers,
    State,
    UpdateActive,
    UpdateCommon,
    // UpdateCoolingDown,
    UpdateDeactivated,
};

struct BeginOutput {
    stake: StakeRecord,
    is_treasury_msol_ready_for_transfer: bool,
}

impl<'info> UpdateCommon<'info> {
    fn begin(&mut self, stake_index: u32) -> Result<BeginOutput, ProgramError> {
        /*
        self.state
            .validator_system
            .check_validator_list(self.validator_list.key)?;*/
        self.state.stake_system.check_stake_list(&self.stake_list)?;
        self.state
            .check_msol_mint(self.msol_mint.to_account_info().key)?;
        self.state
            .check_msol_mint_authority(self.msol_mint_authority.key)?;
        let is_treasury_msol_ready_for_transfer = self
            .state
            .check_treasury_msol_account(&self.treasury_msol_account)?;
        self.state
            .check_stake_withdraw_authority(self.stake_withdraw_authority.key)?;
        self.state.check_reserve_address(self.reserve_pda.key)?;
        check_address(self.stake_program.key, &stake::program::ID, "stake_program")?;
        check_address(self.token_program.key, &spl_token::ID, "token_program")?;

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

        let stake = self
            .state
            .stake_system
            .get(&self.stake_list.data.as_ref().borrow(), stake_index)?;
        /*if stake.last_update_epoch == self.clock.epoch {
            msg!("Double update for stake {}", stake.stake_account);
            return Ok(()); // Not error. Maybe parallel update artifact
        }*/
        check_address(
            self.stake_account.to_account_info().key,
            &stake.stake_account,
            "stake_account",
        )?;

        Ok(BeginOutput {
            stake,
            is_treasury_msol_ready_for_transfer,
        })
    }

    pub fn withdraw_to_reserve(&mut self, amount: u64) -> ProgramResult {
        if amount > 0 {
            self.state.with_stake_withdraw_authority_seeds(|seeds| {
                // Move unstaked + rewards for restaking
                invoke_signed(
                    &stake::instruction::withdraw(
                        self.stake_account.to_account_info().key,
                        self.stake_withdraw_authority.key,
                        self.reserve_pda.key,
                        amount,
                        None,
                    ),
                    &[
                        self.stake_program.clone(),
                        self.stake_account.to_account_info(),
                        self.reserve_pda.clone(),
                        self.clock.to_account_info(),
                        self.stake_history.clone(),
                        self.stake_withdraw_authority.clone(),
                    ],
                    &[seeds],
                )
            })?;
            self.state.on_transfer_to_reserve(amount);
        }
        Ok(())
    }

    pub fn mint_to_treasury(&mut self, msol_lamports: u64) -> ProgramResult {
        if msol_lamports > 0 {
            self.state.with_msol_mint_authority_seeds(|seeds| {
                mint_to(
                    CpiContext::new_with_signer(
                        self.token_program.clone(),
                        MintTo {
                            mint: self.msol_mint.to_account_info(),
                            to: self.treasury_msol_account.to_account_info(),
                            authority: self.msol_mint_authority.clone(),
                        },
                        &[seeds],
                    ),
                    msol_lamports,
                )
            })?;
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
    pub fn process(&mut self, stake_index: u32, validator_index: u32) -> ProgramResult {
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
            return Err(ProgramError::InvalidInstructionData);
        }
        if delegation.deactivation_epoch != std::u64::MAX {
            // is deactivated or deactivating
            msg!(
                "Cooling down stake {}. Please use UpdateCoolingDown",
                self.stake_account.to_account_info().key
            );
            return Err(ProgramError::InvalidAccountData);
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
    pub fn process(&mut self, stake_index: u32) -> ProgramResult {
        let BeginOutput {
            stake,
            is_treasury_msol_ready_for_transfer,
        } = self.begin(stake_index)?;

        check_address(
            self.system_program.to_account_info().key,
            &system_program::ID,
            "system_program",
        )?;
        self.state
            .check_operational_sol_account(self.operational_sol_account.key)?;

        let delegation = self
            .stake_account
            .delegation()
            .expect("Undelegated stake under control");
        if delegation.deactivation_epoch == std::u64::MAX {
            msg!(
                "Stake {} is active",
                self.stake_account.to_account_info().key
            );
            return Err(ProgramError::InvalidAccountData);
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
        self.state.with_reserve_seeds(|seeds| {
            invoke_signed(
                &system_instruction::transfer(
                    self.reserve_pda.key,
                    self.operational_sol_account.key,
                    rent,
                ),
                &[
                    self.system_program.clone(),
                    self.reserve_pda.clone(),
                    self.operational_sol_account.clone(),
                ],
                &[seeds],
            )
        })?;
        self.state.on_transfer_from_reserve(rent);

        if stake.is_emergency_unstaking == 0 {
            // remove from delayed_unstake_cooling_down (amount is now in the reserve, is no longer cooling-down)
            self.state.stake_system.delayed_unstake_cooling_down = self
                .state
                .stake_system
                .delayed_unstake_cooling_down
                .saturating_sub(stake.last_update_delegated_lamports);
        } else {
            // remove from emergency_cooling_down (amount is now in the reserve, is no longer cooling-down)
            self.state.emergency_cooling_down = self
                .state
                .emergency_cooling_down
                .saturating_sub(stake.last_update_delegated_lamports);
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
