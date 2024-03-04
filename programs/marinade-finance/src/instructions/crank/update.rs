//get staking rewards & update mSOL price

use std::ops::{Deref, DerefMut};

use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::stake_history;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_spl::stake::{withdraw, Stake, StakeAccount, Withdraw};
use anchor_spl::token::{mint_to, Mint, MintTo, Token};

use crate::events::crank::{UpdateActiveEvent, UpdateDeactivatedEvent};
use crate::events::U64ValueChange;
use crate::require_lte;
use crate::state::delinquent_upgrader::DelinquentUpgraderState;
use crate::state::stake_system::{StakeList, StakeStatus};
use crate::state::validator_system::{ValidatorList, ValidatorRecord};
use crate::{
    error::MarinadeError,
    state::stake_system::{StakeRecord, StakeSystem},
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
    #[account(
        mut,
        address = state.stake_system.stake_list.account,
    )]
    pub stake_list: Account<'info, StakeList>,
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

    #[account(
        mut,
        address = state.validator_system.validator_list.account,
    )]
    pub validator_list: Account<'info, ValidatorList>,
}

#[derive(Accounts)]
pub struct UpdateActive<'info> {
    pub common: UpdateCommon<'info>,
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
    #[account(
        mut,
        address = common.state.operational_sol_account
    )]
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
    validator: ValidatorRecord,
    is_treasury_msol_ready_for_transfer: bool,
}

impl<'info> UpdateCommon<'info> {
    fn begin(&mut self, stake_index: u32, validator_index: u32) -> Result<BeginOutput> {
        let is_treasury_msol_ready_for_transfer = self
            .state
            .get_treasury_msol_balance(&self.treasury_msol_account)
            .is_some();

        let virtual_reserve_balance =
            self.state.available_reserve_balance + self.state.rent_exempt_for_token_acc;

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
            &self.stake_list.to_account_info().data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;
        /*if stake.last_update_epoch == self.clock.epoch {
            msg!("Double update for stake {}", stake.stake_account);
            return Ok(()); // Not error. Maybe parallel update artifact
        }*/
        let validator = self.state.validator_system.get_checked(
            &self.validator_list.to_account_info().data.as_ref().borrow(),
            validator_index,
            &self
                .stake_account
                .delegation()
                .ok_or(error!(MarinadeError::StakeNotDelegated))?
                .voter_pubkey,
        )?;

        Ok(BeginOutput {
            stake,
            validator,
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

    #[inline]
    pub fn update_msol_price(&mut self) -> Result<U64ValueChange> {
        // price is computed as:
        // total_active_balance + total_cooling_down + reserve - circulating_ticket_balance
        // DIVIDED by msol_supply
        let old = self.state.msol_price;
        self.state.msol_price = self.state.msol_to_sol(State::PRICE_DENOMINATOR)?; // store binary-denominated mSOL price
        Ok(U64ValueChange {
            old,
            new: self.state.msol_price,
        })
    }

    // returns fees in msol
    pub fn mint_protocol_fees(&mut self, lamports_incoming: u64) -> Result<u64> {
        // apply x% protocol fee on staking rewards (do this before updating validators' balance, so it's 1% at old, lower, price)
        let protocol_rewards_fee = self.state.reward_fee.apply(lamports_incoming);
        msg!("protocol_rewards_fee {}", protocol_rewards_fee);
        // compute mSOL amount for protocol_rewards_fee
        let fee_as_msol_amount = self.state.calc_msol_from_lamports(protocol_rewards_fee)?;
        self.mint_to_treasury(fee_as_msol_amount)?;
        Ok(fee_as_msol_amount)
    }

    fn check_delinquent_upgrade_state_progression(&mut self) -> Result<()> {
        match self.state.delinquent_upgrader {
            DelinquentUpgraderState::IteratingStakes {
                visited_count,
                total_active_balance,
                total_delinquent_balance,
            } => {
                if visited_count == self.state.stake_system.stake_count() {
                    require_eq!(
                        total_active_balance,
                        self.state.validator_system.total_active_balance,
                        MarinadeError::UpgradingInvariantViolation,
                    );
                    self.state.delinquent_upgrader = DelinquentUpgraderState::IteratingValidators {
                        visited_count: 0,
                        delinquent_balance_left: total_delinquent_balance,
                    }
                }
            }
            _ => {}
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
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        let total_virtual_staked_lamports = self.state.total_virtual_staked_lamports();
        let msol_supply = self.state.msol_supply;
        let BeginOutput {
            mut stake,
            mut validator,
            is_treasury_msol_ready_for_transfer,
        } = self.begin(stake_index, validator_index)?;

        let delegation = self.stake_account.delegation().ok_or_else(|| {
            error!(MarinadeError::RequiredDelegatedStake).with_account_name("stake_account")
        })?;

        // record for event
        let validator_active_balance = validator.active_balance;
        let total_active_balance = self.state.validator_system.total_active_balance;

        // require stake is active (deactivation_epoch == u64::MAX)
        require_eq!(
            delegation.deactivation_epoch,
            std::u64::MAX,
            MarinadeError::RequiredActiveStake
        );
        // No fix for now
        require_neq!(
            stake.last_update_status,
            StakeStatus::Deactivating,
            MarinadeError::RequiredActiveStake
        );

        self.delinquent_upgrade(&mut stake, &mut validator)?;

        // current lamports amount, to compare with previous
        let delegated_lamports = delegation.stake;

        // we don't consider self.stake_account.meta().unwrap().rent_exempt_reserve as part of the stake
        // the reserve lamports are paid by the marinade-program/bot and return to marinade-program/bot once the account is deleted
        let stake_balance_without_rent = self.stake_account.to_account_info().lamports()
            - self.stake_account.meta().unwrap().rent_exempt_reserve;
        // normally extra-lamports in the native stake means MEV rewards
        let extra_lamports = stake_balance_without_rent.saturating_sub(delegated_lamports);
        msg!("Extra lamports in stake balance: {}", extra_lamports);
        let extra_msol_fees = if extra_lamports > 0 {
            // by withdrawing to reserve, we add to the SOL assets under control,
            // and by that we increase the mSOL price
            self.withdraw_to_reserve(extra_lamports)?;
            // after sending to reserve, we take protocol_fees as minted mSOL
            if is_treasury_msol_ready_for_transfer {
                Some(self.mint_protocol_fees(extra_lamports)?)
            } else {
                None
            }
        } else {
            if is_treasury_msol_ready_for_transfer {
                Some(0)
            } else {
                None
            }
        };

        msg!("current staked lamports {}", delegated_lamports);
        let delegation_growth_msol_fees =
            if delegated_lamports >= stake.last_update_delegated_lamports {
                // re-delegated by solana rewards
                let rewards = delegated_lamports - stake.last_update_delegated_lamports;
                msg!("Staking rewards: {}", rewards);

                let delegation_growth_msol_fees = if is_treasury_msol_ready_for_transfer {
                    Some(self.mint_protocol_fees(rewards)?)
                } else {
                    None
                };

                // validator active balance is updated with rewards
                validator.active_balance += rewards;
                // validator_system.total_active_balance is updated with re-delegated rewards (this impacts price-calculation)
                self.state.validator_system.total_active_balance += rewards;
                self.delinquent_upgrade_on_rewards(&mut validator, rewards)?;
                delegation_growth_msol_fees
            } else {
                //slashed
                let slashed = stake.last_update_delegated_lamports - delegated_lamports;
                msg!("slashed {}", slashed);
                //validator balance is updated with slashed
                validator.active_balance -= slashed;
                self.state.validator_system.total_active_balance -= slashed;
                self.delinquent_upgrade_on_slash(&mut validator, slashed)?;
                if is_treasury_msol_ready_for_transfer {
                    Some(0)
                } else {
                    None
                }
            };

        // mark stake-account as visited
        stake.last_update_epoch = self.clock.epoch;
        let delegation_change = {
            let old = stake.last_update_delegated_lamports;
            stake.last_update_delegated_lamports = delegated_lamports;
            U64ValueChange {
                old,
                new: delegated_lamports,
            }
        };

        // update validator-list
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

        // set new mSOL price
        let msol_price_change = self.update_msol_price()?;
        // save stake record
        self.state.stake_system.set(
            &mut self.stake_list.to_account_info().data.as_ref().borrow_mut(),
            stake_index,
            stake,
        )?;

        assert_eq!(
            self.state.available_reserve_balance + self.state.rent_exempt_for_token_acc,
            self.reserve_pda.lamports()
        );

        self.check_delinquent_upgrade_state_progression()?;

        emit!(UpdateActiveEvent {
            state: self.state.key(),
            epoch: self.clock.epoch,
            stake_index,
            stake_account: stake.stake_account,
            validator_index,
            validator_vote: validator.validator_account,
            delegation_change,
            delegation_growth_msol_fees,
            extra_lamports,
            extra_msol_fees,
            validator_active_balance,
            total_active_balance,
            msol_price_change,
            reward_fee_used: self.state.reward_fee,
            total_virtual_staked_lamports,
            msol_supply,
        });
        Ok(())
    }

    // helper fn to upgrade the data from Unknown to Active and advance the iteration
    // this is done only on the first loop after program upgrade (first staged of delinquent stake iterator)
    fn delinquent_upgrade(
        &mut self,
        stake: &mut StakeRecord,
        validator: &mut ValidatorRecord,
    ) -> Result<()> {
        if stake.last_update_status == StakeStatus::Unknown {
            stake.last_update_status = StakeStatus::Active;
            let actual_total_active_balance = self.state.validator_system.total_active_balance;
            match &mut self.state.delinquent_upgrader {
                DelinquentUpgraderState::IteratingStakes {
                    visited_count,
                    total_active_balance,
                    ..
                } => {
                    *visited_count += 1;
                    *total_active_balance += stake.last_update_delegated_lamports;
                    require_lte!(
                        *total_active_balance,
                        actual_total_active_balance,
                        MarinadeError::UpgradingInvariantViolation
                    );
                    validator.delinquent_upgrader_active_balance +=
                        stake.last_update_delegated_lamports;
                    require_lte!(
                        validator.delinquent_upgrader_active_balance,
                        validator.active_balance,
                        MarinadeError::UpgradingInvariantViolation
                    );
                }
                _ => return err!(MarinadeError::UpgradingInvariantViolation),
            }
        }
        Ok(())
    }

    // IF the DelinquentUpgrader loop is running (iterating)
    // keep the numbers in line
    fn delinquent_upgrade_on_rewards(
        &mut self,
        validator: &mut ValidatorRecord,
        rewards: u64,
    ) -> Result<()> {
        if let DelinquentUpgraderState::IteratingStakes {
            total_active_balance,
            ..
        } = &mut self.state.delinquent_upgrader
        {
            validator.delinquent_upgrader_active_balance += rewards;
            *total_active_balance += rewards;
        }
        Ok(())
    }

    // IF the DelinquentUpgrader loop is running (iterating)
    // keep the numbers in line
    fn delinquent_upgrade_on_slash(
        &mut self,
        validator: &mut ValidatorRecord,
        slashed: u64,
    ) -> Result<()> {
        if let DelinquentUpgraderState::IteratingStakes {
            total_active_balance,
            ..
        } = &mut self.state.delinquent_upgrader
        {
            validator.delinquent_upgrader_active_balance -= slashed;
            *total_active_balance -= slashed;
        }
        Ok(())
    }
}

impl<'info> UpdateDeactivated<'info> {
    /// Compute rewards for a single deactivated stake-account
    /// take 1% protocol fee for treasury & add the rest to validator_system.total_balance
    /// update mSOL price accordingly
    /// Optional Future Expansion: Partial: If the stake-account is a fully-deactivated stake account ready to withdraw,
    /// (cool-down period is complete) delete-withdraw the stake-account, send SOL to reserve-account
    pub fn process(&mut self, stake_index: u32, validator_index: u32) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        let total_virtual_staked_lamports = self.state.total_virtual_staked_lamports();
        let msol_supply = self.state.msol_supply;
        let operational_sol_balance = self.operational_sol_account.lamports();
        let BeginOutput {
            mut stake,
            mut validator,
            is_treasury_msol_ready_for_transfer,
        } = self.begin(stake_index, validator_index)?;

        let delegation = self.stake_account.delegation().ok_or_else(|| {
            error!(MarinadeError::RequiredDelegatedStake).with_account_name("stake_account")
        })?;
        // require deactivated or deactivating (deactivation_epoch != u64::MAX)
        require_neq!(
            delegation.deactivation_epoch,
            std::u64::MAX,
            MarinadeError::RequiredDeactivatingStake
        );
        if stake.last_update_status == StakeStatus::Active {
            // Detected deactivation of delinquent stake-account
            // applying emergency unstake procedure before processing the stake deletion
            require!(
                !stake.is_emergency_unstaking,
                MarinadeError::StakeAccountIsEmergencyUnstaking
            );
            stake.is_emergency_unstaking = true;
            self.state.emergency_cooling_down += stake.last_update_delegated_lamports;
            self.state.validator_system.total_active_balance -=
                stake.last_update_delegated_lamports;
            validator.active_balance -= stake.last_update_delegated_lamports;
            // update validator-list
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
        }

        // Jun-2023 In order to support Solana new redelegate instruction, we need to ignore stake_account.delegation().stake
        // this is because in the case of a redelegated-Deactivating account, the field stake_account.delegation().stake
        // *still contains the original stake amount*, even if the lamports() were sent to the redelegated-Activating account.
        //
        // So for deactivating accounts, in order to determine rewards received, we consider from now on:
        // (lamports - rent) versus last_update_delegated_lamports.
        //
        // For the case of redelegated-Deactivating, when we redelegate we set  last_update_delegated_lamports=0 for the account
        // that go into deactivation, so later, when we reach here last_update_delegated_lamports=0 and in (lamports - rent)
        // we will have rewards. Side note: In the rare event of somebody sending lamports to a deactivating account, we will simply
        // consider those lamports part of the rewards.

        // current lamports amount, to compare with previous
        let rent = self.stake_account.meta().unwrap().rent_exempt_reserve;
        let stake_balance_without_rent = self.stake_account.to_account_info().lamports() - rent;

        let msol_fees = if stake_balance_without_rent >= stake.last_update_delegated_lamports {
            // if there were rewards, mint treasury fee
            // Note: this includes any extra lamports in the stake-account (MEV rewards mostly)
            let rewards = stake_balance_without_rent - stake.last_update_delegated_lamports;
            msg!("Staking rewards: {}", rewards);
            if is_treasury_msol_ready_for_transfer {
                Some(self.mint_protocol_fees(rewards)?)
            } else {
                None
            }
        } else {
            // less than observed last time
            let slashed = stake.last_update_delegated_lamports - stake_balance_without_rent;
            msg!("Slashed {}", slashed);
            if is_treasury_msol_ready_for_transfer {
                Some(0)
            } else {
                None
            }
        };

        // withdraw all to reserve (the stake account will be marked for deletion by the system)
        self.common
            .withdraw_to_reserve(self.stake_account.to_account_info().lamports())?;
        // but send the rent-exempt lamports part to operational_sol_account for the future recreation of this slot's account
        transfer(
            CpiContext::new_with_signer(
                self.system_program.to_account_info(),
                Transfer {
                    from: self.reserve_pda.to_account_info(),
                    to: self.operational_sol_account.to_account_info(),
                },
                &[&[
                    &self.state.key().to_bytes(),
                    State::RESERVE_SEED,
                    &[self.state.reserve_bump_seed],
                ]],
            ),
            rent,
        )?;
        self.state.on_transfer_from_reserve(rent);

        if stake.last_update_delegated_lamports != 0 {
            if self.state.delinquent_upgrader.is_iterating_stakes()
            {
                let delinquent_amount = if !stake.is_emergency_unstaking {
                    let available = self
                        .state
                        .stake_system
                        .delayed_unstake_cooling_down
                        .min(stake.last_update_delegated_lamports);
                    self.state.stake_system.delayed_unstake_cooling_down -= available;
                    stake.last_update_delegated_lamports - available
                } else {
                    let available = self
                        .state
                        .emergency_cooling_down
                        .min(stake.last_update_delegated_lamports);
                    self.state.emergency_cooling_down -= available;
                    stake.last_update_delegated_lamports - available
                };
                if let DelinquentUpgraderState::IteratingStakes {
                    total_delinquent_balance,
                    ..
                } = &mut self.state.delinquent_upgrader
                {
                    *total_delinquent_balance += delinquent_amount;
                } else {
                    unreachable!()
                }
                // keeping the mSOL price invariant
                self.state.validator_system.total_active_balance -= delinquent_amount;
            } else {
                if !stake.is_emergency_unstaking {
                    // remove from delayed_unstake_cooling_down (amount is now in the reserve, is no longer cooling-down)
                    self.state.stake_system.delayed_unstake_cooling_down -=
                        stake.last_update_delegated_lamports;
                } else {
                    // remove from emergency_cooling_down (amount is now in the reserve, is no longer cooling-down)
                    self.state.emergency_cooling_down -= stake.last_update_delegated_lamports;
                }
            }
        }

        // We update mSOL price in case we receive "extra deactivating rewards" after the start of Delayed-unstake.
        // Those rewards went into reserve_pda, are part of mSOL price (benefit all stakers) and even might be re-staked
        // set new mSOL price
        let msol_price_change = self.update_msol_price()?;

        //remove deleted stake-account from our list
        self.common.state.stake_system.remove(
            &mut self
                .common
                .stake_list
                .to_account_info()
                .data
                .as_ref()
                .borrow_mut(),
            stake_index,
        )?;
        self.check_delinquent_upgrade_state_progression()?;

        emit!(UpdateDeactivatedEvent {
            state: self.state.key(),
            epoch: self.clock.epoch,
            stake_index,
            stake_account: stake.stake_account,
            balance_without_rent_exempt: stake_balance_without_rent,
            last_update_delegated_lamports: stake.last_update_delegated_lamports,
            msol_fees,
            msol_price_change,
            reward_fee_used: self.state.reward_fee,
            operational_sol_balance,
            total_virtual_staked_lamports,
            msol_supply,
        });

        Ok(())
    }
}
