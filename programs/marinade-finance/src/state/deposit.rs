use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, system_instruction, system_program};
use anchor_spl::token::{mint_to, transfer, MintTo, Transfer};

use crate::{
    checks::{check_address, check_min_amount, check_owner_program, check_token_mint},
    liq_pool::LiqPoolHelpers,
    state::StateHelpers,
    Deposit,
};

impl<'info> Deposit<'info> {
    fn check_transfer_from(&self, lamports: u64) -> ProgramResult {
        check_owner_program(&self.transfer_from, &system_program::ID, "transfer_from")?;
        if self.transfer_from.lamports() < lamports {
            return Err(ProgramError::InsufficientFunds);
        }
        Ok(())
    }

    fn check_mint_to(&self) -> ProgramResult {
        check_token_mint(&self.mint_to, self.state.msol_mint, "mint_to")?;
        Ok(())
    }

    // fn deposit_sol()
    pub fn process(&mut self, lamports: u64) -> ProgramResult {
        check_min_amount(lamports, self.state.min_deposit, "deposit SOL")?;
        self.state.check_reserve_address(self.reserve_pda.key)?;
        self.state
            .check_msol_mint(self.msol_mint.to_account_info().key)?;
        self.state
            .check_liq_pool_sol_leg_pda(self.liq_pool_sol_leg_pda.key)?;
        self.state
            .liq_pool
            .check_liq_pool_msol_leg(self.liq_pool_msol_leg.to_account_info().key)?;
        self.check_transfer_from(lamports)?;
        self.check_mint_to()?;
        self.state
            .check_msol_mint_authority(self.msol_mint_authority.key)?;
        check_address(
            self.system_program.to_account_info().key,
            &system_program::ID,
            "system_program",
        )?;
        check_address(
            self.token_program.to_account_info().key,
            &spl_token::ID,
            "token_program",
        )?;

        // impossible to happen check outside bug (msol mint auth is a PDA)
        if self.msol_mint.supply > self.state.msol_supply {
            msg!(
                "Warning: mSOL minted {} lamports outside of marinade",
                self.msol_mint.supply - self.state.msol_supply
            );
            return Err(ProgramError::InvalidAccountData);
        }

        let user_lamports = lamports;

        //compute how many mSOL to sell/mint for the user, base on how many lamports being deposited
        let user_msol_buy_order = self.state.calc_msol_from_lamports(user_lamports)?;
        msg!("--- user_m_sol_buy_order {}", user_msol_buy_order);

        //First we try to "sell" mSOL to the user from the LiqPool.
        //The LiqPool needs to get rid of their mSOL because it works better if fully "unbalanced", i.e. with all SOL no mSOL
        //so, if we can, the LiqPool "sells" mSOL to the user (no fee)
        //
        // At max, we can sell all the mSOL in the LiqPool.mSOL_leg
        let swap_msol_max: u64 = user_msol_buy_order.min(self.liq_pool_msol_leg.amount);
        msg!("--- swap_m_sol_max {}", swap_msol_max);

        //if we can sell from the LiqPool
        let user_lamports = if swap_msol_max > 0 {
            // how much lamports go into the LiqPool?
            let lamports_for_the_liq_pool = if user_msol_buy_order == swap_msol_max {
                //we are fulfilling 100% the user order
                user_lamports //100% of the user deposit
            } else {
                //partially filled
                //then it's the lamport value of the tokens we're selling
                self.state.calc_lamports_from_msol_amount(swap_msol_max)?
            };

            //transfer mSOL to the user
            self.state.with_liq_pool_msol_leg_authority_seeds(|seeds| {
                transfer(
                    CpiContext::new_with_signer(
                        self.token_program.clone(),
                        Transfer {
                            from: self.liq_pool_msol_leg.to_account_info(),
                            to: self.mint_to.to_account_info(),
                            authority: self.liq_pool_msol_leg_authority.clone(),
                        },
                        &[seeds],
                    ),
                    swap_msol_max,
                )
            })?;

            //transfer lamports to the LiqPool
            invoke(
                &system_instruction::transfer(
                    self.transfer_from.key,
                    self.liq_pool_sol_leg_pda.key,
                    lamports_for_the_liq_pool,
                ),
                &[
                    self.transfer_from.clone(),
                    self.liq_pool_sol_leg_pda.clone(),
                    self.system_program.clone(),
                ],
            )?;

            //we took "lamports_for_the_liq_pool" from the "user_lamports"
            user_lamports.saturating_sub(lamports_for_the_liq_pool)
            //end of sale from the LiqPool
        } else {
            user_lamports
        };

        // check if we have more lamports from the user
        if user_lamports > 0 {
            self.state.check_staking_cap(user_lamports)?;

            //compute how much msol_to_mint
            //NOTE: it is IMPORTANT to use calc_msol_from_lamports() BEFORE adding the lamports
            // because on_transfer_to_reserve(user_lamports) alters price calculation
            // the same goes for state.on_msol_mint()
            let msol_to_mint = self.state.calc_msol_from_lamports(user_lamports)?;
            msg!("--- msol_to_mint {}", msol_to_mint);

            //transfer user_lamports to reserve
            invoke(
                &system_instruction::transfer(
                    self.transfer_from.key,
                    self.reserve_pda.key,
                    user_lamports,
                ),
                &[
                    self.transfer_from.clone(),
                    self.reserve_pda.clone(),
                    self.system_program.clone(),
                ],
            )?;
            self.state.on_transfer_to_reserve(user_lamports);
            if msol_to_mint > 0 {
                self.state.with_msol_mint_authority_seeds(|mint_seeds| {
                    mint_to(
                        CpiContext::new_with_signer(
                            self.token_program.clone(),
                            MintTo {
                                mint: self.msol_mint.to_account_info(),
                                to: self.mint_to.to_account_info(),
                                authority: self.msol_mint_authority.clone(),
                            },
                            &[mint_seeds],
                        ),
                        msol_to_mint,
                    )
                })?;
                self.state.on_msol_mint(msol_to_mint);
            }
            // self.state.stake_total += user_lamports; // auto calculated
            // self.state.epoch_stake_orders += user_lamports;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::liq_pool::LiqPool;
    use crate::list::List;
    use crate::stake_system::StakeSystem;
    use crate::validator_system::ValidatorSystem;
    use crate::Fee;
    use crate::State;

    use super::*;
    use solana_sdk::program_option::COption;
    use solana_sdk::program_pack::Pack;
    use solana_sdk::signature::Keypair;
    use solana_sdk::signature::Signer;
    use soltes::*;
    use spl_token::{state::Account as SplToken, state::Mint as SplMint, ID as SPL_TOKEN_ID};

    #[test]
    fn simple_deposit() -> ProgramResult {
        setup();

        let state_address = Keypair::new().pubkey();
        let msol_mint_address = Keypair::new().pubkey();
        let admin_authority = Keypair::new().pubkey();
        let operational_sol_account = Keypair::new().pubkey();
        let treasury_msol_account = Keypair::new().pubkey();
        let stake_list_address = Keypair::new().pubkey();
        let validator_list_address = Keypair::new().pubkey();
        let validator_manager_authority = Keypair::new().pubkey();
        let lp_mint_address = Keypair::new().pubkey();
        let msol_leg_address = Keypair::new().pubkey();

        let state = State {
            msol_mint: msol_mint_address,
            admin_authority,
            operational_sol_account,
            treasury_msol_account,
            reserve_bump_seed: State::find_reserve_address(&state_address).1,
            msol_mint_authority_bump_seed: State::find_msol_mint_authority(&state_address).1,
            rent_exempt_for_token_acc: SplToken::LEN as u64,
            reward_fee: Fee { basis_points: 1 },
            stake_system: StakeSystem {
                stake_list: List {
                    account: stake_list_address,
                    item_size: 1000,
                    count: 0,
                    new_account: Pubkey::default(),
                    copied_count: 0,
                },
                delayed_unstake_cooling_down: 0,
                stake_deposit_bump_seed: StakeSystem::find_stake_deposit_authority(&state_address)
                    .1,
                stake_withdraw_bump_seed: StakeSystem::find_stake_withdraw_authority(
                    &state_address,
                )
                .1,
                slots_for_stake_delta: 10,
                last_stake_delta_epoch: 122,
                min_stake: 10000,
                extra_stake_delta_runs: 1,
            },
            validator_system: ValidatorSystem {
                validator_list: List {
                    account: validator_list_address,
                    item_size: 1000,
                    count: 0,
                    new_account: Pubkey::default(),
                    copied_count: 0,
                },
                manager_authority: validator_manager_authority,
                total_validator_score: 0,
                total_active_balance: 0,
                auto_add_validator_enabled: 0,
            },
            liq_pool: LiqPool {
                lp_mint: lp_mint_address,
                lp_mint_authority_bump_seed: LiqPool::find_lp_mint_authority(&state_address).1,
                sol_leg_bump_seed: LiqPool::find_sol_leg_address(&state_address).1,
                msol_leg_authority_bump_seed: LiqPool::find_msol_leg_authority(&state_address).1,
                msol_leg: msol_leg_address,
                lp_liquidity_target: 100000000,
                lp_max_fee: Fee { basis_points: 1000 },
                lp_min_fee: Fee { basis_points: 100 },
                treasury_cut: Fee { basis_points: 10 },
                lp_supply: 0,
                lent_from_sol_leg: 0,
                liquidity_sol_cap: u64::MAX,
            },
            available_reserve_balance: 100000000,
            msol_supply: 100000000,
            msol_price: 1111111,
            circulating_ticket_count: 0,
            circulating_ticket_balance: 0,
            lent_from_reserve: 0,
            min_deposit: 0,
            min_withdraw: 0,
            staking_sol_cap: u64::MAX,
            emergency_cooling_down: 0,
        };
        let mut state_account = AccountBuilder::new(state_address)
            .with_data(state.try_to_vec().unwrap())
            .with_owner(crate::ID);

        let msol_mint = SplMint {
            mint_authority: COption::Some(State::find_msol_mint_authority(&state_address).0),
            supply: 0,
            decimals: 9,
            is_initialized: true,
            freeze_authority: COption::None,
        };

        crate::__private::__global::deposit(
            &crate::id(),
            &[state_account.to_account_info(false, true),],
            &34u64.try_to_vec()?,
        )?;

        Ok(())
    }
}
