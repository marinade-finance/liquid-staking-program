use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_spl::token::{
    mint_to, transfer as transfer_tokens, Mint, MintTo, Token, TokenAccount,
    Transfer as TransferTokens,
};

use crate::checks::check_min_amount;
use crate::state::liq_pool::LiqPool;
use crate::State;

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut, has_one = msol_mint)]
    pub state: Box<Account<'info, State>>,

    #[account(mut)]
    pub msol_mint: Box<Account<'info, Mint>>,

    #[account(mut, seeds = [&state.key().to_bytes(),
            LiqPool::SOL_LEG_SEED],
            bump = state.liq_pool.sol_leg_bump_seed)]
    pub liq_pool_sol_leg_pda: SystemAccount<'info>,

    #[account(mut, address = state.liq_pool.msol_leg)]
    pub liq_pool_msol_leg: Box<Account<'info, TokenAccount>>,
    /// CHECK: PDA
    #[account(seeds = [&state.key().to_bytes(),
            LiqPool::MSOL_LEG_AUTHORITY_SEED],
            bump = state.liq_pool.msol_leg_authority_bump_seed)]
    pub liq_pool_msol_leg_authority: UncheckedAccount<'info>,

    #[account(mut, seeds = [&state.key().to_bytes(),
            State::RESERVE_SEED],
            bump = state.reserve_bump_seed)]
    pub reserve_pda: SystemAccount<'info>,

    #[account(mut)]
    #[account(owner = system_program::ID)]
    pub transfer_from: Signer<'info>,

    /// user mSOL Token account to send the mSOL
    #[account(mut, token::mint = state.msol_mint)]
    pub mint_to: Box<Account<'info, TokenAccount>>,

    /// CHECK: PDA
    #[account(seeds = [&state.key().to_bytes(),
            State::MSOL_MINT_AUTHORITY_SEED],
            bump = state.msol_mint_authority_bump_seed)]
    pub msol_mint_authority: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

impl<'info> Deposit<'info> {
    fn check_transfer_from(&self, lamports: u64) -> Result<()> {
        if self.transfer_from.lamports() < lamports {
            return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
        }
        Ok(())
    }

    // fn deposit_sol()
    pub fn process(&mut self, lamports: u64) -> Result<()> {
        check_min_amount(lamports, self.state.min_deposit, "deposit SOL")?;
        self.check_transfer_from(lamports)?;

        // impossible to happen check outside bug (msol mint auth is a PDA)
        if self.msol_mint.supply > self.state.msol_supply {
            msg!(
                "Warning: mSOL minted {} lamports outside of marinade",
                self.msol_mint.supply - self.state.msol_supply
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
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

            transfer_tokens(
                CpiContext::new_with_signer(
                    self.token_program.to_account_info(),
                    TransferTokens {
                        from: self.liq_pool_msol_leg.to_account_info(),
                        to: self.mint_to.to_account_info(),
                        authority: self.liq_pool_msol_leg_authority.to_account_info(),
                    },
                    &[&[
                        &self.state.key().to_bytes(),
                        LiqPool::MSOL_LEG_AUTHORITY_SEED,
                        &[self.state.liq_pool.msol_leg_authority_bump_seed],
                    ]],
                ),
                swap_msol_max,
            )?;

            //transfer lamports to the LiqPool
            transfer(
                CpiContext::new(
                    self.system_program.to_account_info(),
                    Transfer {
                        from: self.transfer_from.to_account_info(),
                        to: self.liq_pool_sol_leg_pda.to_account_info(),
                    },
                ),
                lamports_for_the_liq_pool,
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
            transfer(
                CpiContext::new(
                    self.system_program.to_account_info(),
                    Transfer {
                        from: self.transfer_from.to_account_info(),
                        to: self.reserve_pda.to_account_info(),
                    },
                ),
                user_lamports,
            )?;
            self.state.on_transfer_to_reserve(user_lamports);
            if msol_to_mint > 0 {
                mint_to(
                    CpiContext::new_with_signer(
                        self.token_program.to_account_info(),
                        MintTo {
                            mint: self.msol_mint.to_account_info(),
                            to: self.mint_to.to_account_info(),
                            authority: self.msol_mint_authority.to_account_info(),
                        },
                        &[&[
                            &self.state.key().to_bytes(),
                            State::MSOL_MINT_AUTHORITY_SEED,
                            &[self.state.msol_mint_authority_bump_seed],
                        ]],
                    ),
                    msol_to_mint,
                )?;
                self.state.on_msol_mint(msol_to_mint);
            }
            // self.state.stake_total += user_lamports; // auto calculated
            // self.state.epoch_stake_orders += user_lamports;
        }

        Ok(())
    }
}
