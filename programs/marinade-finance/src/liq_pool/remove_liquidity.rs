use crate::{
    calc::proportional,
    checks::{check_address, check_min_amount, check_owner_program, check_token_mint},
    liq_pool::LiqPoolHelpers,
    RemoveLiquidity,
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction, system_program};
use anchor_spl::token::{burn, transfer, Burn, Transfer};

impl<'info> RemoveLiquidity<'info> {
    fn check_burn_from(&self, tokens: u64) -> ProgramResult {
        check_token_mint(&self.burn_from, self.state.liq_pool.lp_mint, "burn_from")?;
        // if delegated, check delegated amount
        if *self.burn_from_authority.key == self.burn_from.owner {
            if self.burn_from.amount < tokens {
                msg!(
                    "Requested to remove {} liquidity but have only {}",
                    tokens,
                    self.burn_from.amount
                );
                return Err(ProgramError::InsufficientFunds);
            }
        } else if self
            .burn_from
            .delegate
            .contains(self.burn_from_authority.key)
        {
            // if delegated, check delegated amount
            // delegated_amount & delegate must be set on the user's lp account before
            if self.burn_from.delegated_amount < tokens {
                msg!(
                    "Delegated {} liquidity. Requested {}",
                    self.burn_from.delegated_amount,
                    tokens
                );
                return Err(ProgramError::InsufficientFunds);
            }
        } else {
            msg!(
                "Token must be delegated to {}",
                self.burn_from_authority.key
            );
            return Err(ProgramError::InvalidArgument);
        }
        Ok(())
    }

    fn check_transfer_sol_to(&self) -> ProgramResult {
        check_owner_program(
            &self.transfer_sol_to,
            &system_program::ID,
            "transfer_sol_to",
        )?;
        Ok(())
    }

    fn check_transfer_msol_to(&self) -> ProgramResult {
        check_token_mint(
            &self.transfer_msol_to,
            self.state.msol_mint,
            "transfer_msol_to",
        )?;
        Ok(())
    }

    pub fn process(&mut self, tokens: u64) -> ProgramResult {
        msg!("rem-liq pre check");
        self.state
            .liq_pool
            .check_lp_mint(self.lp_mint.to_account_info().key)?;
        // self.state
        //     .check_msol_mint(self.msol_mint.to_account_info().key)?;
        self.check_burn_from(tokens)?;
        self.check_transfer_sol_to()?;
        self.check_transfer_msol_to()?;
        self.state
            .check_liq_pool_sol_leg_pda(self.liq_pool_sol_leg_pda.key)?;
        self.state
            .liq_pool
            .check_liq_pool_msol_leg(self.liq_pool_msol_leg.to_account_info().key)?;
        self.state
            .check_liq_pool_msol_leg_authority(self.liq_pool_msol_leg_authority.key)?;
        check_address(
            self.system_program.key,
            &system_program::ID,
            "system_program",
        )?;
        check_address(self.token_program.key, &spl_token::ID, "token_program")?;

        // Update virtual lp_supply by real one
        if self.lp_mint.supply > self.state.liq_pool.lp_supply {
            msg!("Someone minted lp tokens without our permission or bug found");
            // return Err(ProgramError::InvalidAccountData);
        } else {
            // maybe burn
            self.state.liq_pool.lp_supply = self.lp_mint.supply;
        }

        msg!("mSOL-SOL-LP total supply:{}", self.lp_mint.supply);

        let sol_out_amount = proportional(
            tokens,
            self.liq_pool_sol_leg_pda
                .lamports()
                .checked_sub(self.state.rent_exempt_for_token_acc)
                .unwrap(),
            self.state.liq_pool.lp_supply, // Use virtual amount
        )?;
        let msol_out_amount = proportional(
            tokens,
            self.liq_pool_msol_leg.amount,
            self.state.liq_pool.lp_supply, // Use virtual amount
        )?;

        check_min_amount(
            sol_out_amount
                .checked_add(
                    self.state
                        .calc_lamports_from_msol_amount(msol_out_amount)
                        .expect("Error converting mSOLs to lamports"),
                )
                .expect("lamports overflow"),
            self.state.min_withdraw,
            "removed liquidity",
        )?;
        msg!(
            "SOL out amount:{}, mSOL out amount:{}",
            sol_out_amount,
            msol_out_amount
        );

        if sol_out_amount > 0 {
            msg!("transfer SOL");
            self.state.with_liq_pool_sol_leg_seeds(|sol_seeds| {
                invoke_signed(
                    &system_instruction::transfer(
                        self.liq_pool_sol_leg_pda.key,
                        self.transfer_sol_to.key,
                        sol_out_amount,
                    ),
                    &[
                        self.liq_pool_sol_leg_pda.clone(),
                        self.transfer_sol_to.clone(),
                        self.system_program.clone(),
                    ],
                    &[sol_seeds],
                )
            })?;
        }

        if msol_out_amount > 0 {
            msg!("transfer mSOL");
            self.state
                .with_liq_pool_msol_leg_authority_seeds(|msol_seeds| {
                    transfer(
                        CpiContext::new_with_signer(
                            self.token_program.clone(),
                            Transfer {
                                from: self.liq_pool_msol_leg.to_account_info(),
                                to: self.transfer_msol_to.to_account_info(),
                                authority: self.liq_pool_msol_leg_authority.clone(),
                            },
                            &[msol_seeds],
                        ),
                        msol_out_amount,
                    )
                })?;
        }

        burn(
            CpiContext::new(
                self.token_program.clone(),
                Burn {
                    mint: self.lp_mint.to_account_info(),
                    to: self.burn_from.to_account_info(),
                    authority: self.burn_from_authority.clone(),
                },
            ),
            tokens,
        )?;
        self.state.liq_pool.on_lp_burn(tokens);

        msg!("end instruction rem-liq");
        Ok(())
    }
}
