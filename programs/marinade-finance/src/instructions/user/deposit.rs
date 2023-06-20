use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_spl::token::{
    mint_to, transfer as transfer_tokens, Mint, MintTo, Token, TokenAccount,
    Transfer as TransferTokens,
};

use crate::error::MarinadeError;
use crate::events::user::DepositEvent;
use crate::state::liq_pool::LiqPool;
use crate::{require_lte, State};

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(
        mut,
        has_one = msol_mint
    )]
    pub state: Box<Account<'info, State>>,

    #[account(mut)]
    pub msol_mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        seeds = [
            &state.key().to_bytes(),
            LiqPool::SOL_LEG_SEED
        ],
        bump = state.liq_pool.sol_leg_bump_seed
    )]
    pub liq_pool_sol_leg_pda: SystemAccount<'info>,

    #[account(
        mut,
        address = state.liq_pool.msol_leg
    )]
    pub liq_pool_msol_leg: Box<Account<'info, TokenAccount>>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            LiqPool::MSOL_LEG_AUTHORITY_SEED
        ],
        bump = state.liq_pool.msol_leg_authority_bump_seed
    )]
    pub liq_pool_msol_leg_authority: UncheckedAccount<'info>,

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
        mut,
        owner = system_program::ID
    )]
    pub transfer_from: Signer<'info>,

    /// user mSOL Token account to send the mSOL
    #[account(
        mut,
        token::mint = state.msol_mint
    )]
    pub mint_to: Box<Account<'info, TokenAccount>>,

    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            State::MSOL_MINT_AUTHORITY_SEED
        ],
        bump = state.msol_mint_authority_bump_seed
    )]
    pub msol_mint_authority: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

impl<'info> Deposit<'info> {
    // fn deposit_sol()
    pub fn process(&mut self, lamports: u64) -> Result<()> {
        require_gte!(
            lamports,
            self.state.min_deposit,
            MarinadeError::DepositAmountIsTooLow
        );
        require_lte!(
            lamports,
            self.transfer_from.lamports(),
            MarinadeError::NotEnoughUserFunds
        );

        // impossible to happen check outside bug (msol mint auth is a PDA)
        require_lte!(
            self.msol_mint.supply,
            self.state.msol_supply,
            MarinadeError::UnregisteredMsolMinted
        );

        let total_virtual_staked_lamports = self.state.total_virtual_staked_lamports();
        let msol_supply = self.state.msol_supply;

        //compute how many mSOL to sell/mint for the user, base on how many lamports being deposited
        let user_msol_buy_order = self.state.calc_msol_from_lamports(lamports)?;
        msg!("--- user_m_sol_buy_order {}", user_msol_buy_order);

        //First we try to "sell" mSOL to the user from the LiqPool.
        //The LiqPool needs to get rid of their mSOL because it works better if fully "unbalanced", i.e. with all SOL no mSOL
        //so, if we can, the LiqPool "sells" mSOL to the user (no fee)
        //
        // At max, we can sell all the mSOL in the LiqPool.mSOL_leg
        let msol_swapped: u64 = user_msol_buy_order.min(self.liq_pool_msol_leg.amount);
        msg!("--- swap_m_sol_max {}", msol_swapped);

        //if we can sell from the LiqPool
        let sol_swapped = if msol_swapped > 0 {
            // how much lamports go into the LiqPool?
            let sol_swapped = if user_msol_buy_order == msol_swapped {
                //we are fulfilling 100% the user order
                lamports //100% of the user deposit
            } else {
                // partially filled
                // then it's the lamport value of the tokens we're selling
                self.state.calc_lamports_from_msol_amount(msol_swapped)?
            };

            // transfer mSOL to the user

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
                msol_swapped,
            )?;

            // transfer lamports to the LiqPool
            transfer(
                CpiContext::new(
                    self.system_program.to_account_info(),
                    Transfer {
                        from: self.transfer_from.to_account_info(),
                        to: self.liq_pool_sol_leg_pda.to_account_info(),
                    },
                ),
                sol_swapped,
            )?;

            sol_swapped
            //end of sale from the LiqPool
        } else {
            0
        };

        let sol_deposited = lamports - sol_swapped;
        // check if we have more lamports from the user
        let msol_minted = if sol_deposited > 0 {
            self.state.check_staking_cap(sol_deposited)?;

            //compute how much msol_to_mint
            //NOTE: it is IMPORTANT to use calc_msol_from_lamports() BEFORE adding the lamports
            // because on_transfer_to_reserve(sol_deposited) alters price calculation
            // the same goes for state.on_msol_mint()
            let msol_to_mint = self.state.calc_msol_from_lamports(sol_deposited)?;
            msg!("--- msol_to_mint {}", msol_to_mint);

            // transfer sol_deposited to reserve
            transfer(
                CpiContext::new(
                    self.system_program.to_account_info(),
                    Transfer {
                        from: self.transfer_from.to_account_info(),
                        to: self.reserve_pda.to_account_info(),
                    },
                ),
                sol_deposited,
            )?;
            self.state.on_transfer_to_reserve(sol_deposited);
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
            msol_to_mint
        } else {
            0
        };

        self.mint_to.reload()?;
        self.liq_pool_msol_leg.reload()?;
        emit!(DepositEvent {
            state: self.state.key(),
            sol_owner: self.transfer_from.key(),
            sol_swapped,
            msol_swapped,
            sol_deposited,
            msol_minted,
            new_user_sol_balance: self.transfer_from.lamports(),
            new_user_msol_balance: self.mint_to.amount,
            new_sol_leg_balance: self.liq_pool_sol_leg_pda.lamports(),
            new_msol_leg_balance: self.liq_pool_msol_leg.amount,
            new_reserve_balance: self.reserve_pda.lamports(),
            total_virtual_staked_lamports,
            msol_supply
        });

        Ok(())
    }
}
