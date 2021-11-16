use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction, system_program};
use anchor_spl::token::{transfer, Transfer};

use crate::checks::check_min_amount;
use crate::{
    checks::{check_address, check_owner_program, check_token_mint},
    liq_pool::LiqPoolHelpers,
    CommonError, LiquidUnstake,
};

impl<'info> LiquidUnstake<'info> {
    fn check_get_msol_from(&self, msol_amount: u64) -> ProgramResult {
        check_token_mint(&self.get_msol_from, self.state.msol_mint, "get_msol_from")?;
        // if delegated, check delegated amount
        if *self.get_msol_from_authority.key == self.get_msol_from.owner {
            if self.get_msol_from.amount < msol_amount {
                msg!(
                    "Requested to unstake {} mSOL lamports but have only {}",
                    msol_amount,
                    self.get_msol_from.amount
                );
                return Err(ProgramError::InsufficientFunds);
            }
        } else if self
            .get_msol_from
            .delegate
            .contains(self.get_msol_from_authority.key)
        {
            // if delegated, check delegated amount
            // delegated_amount & delegate must be set on the user's msol account before calling OrderUnstake
            if self.get_msol_from.delegated_amount < msol_amount {
                msg!(
                    "Delegated {} mSOL lamports. Requested {}",
                    self.get_msol_from.delegated_amount,
                    msol_amount
                );
                return Err(ProgramError::InsufficientFunds);
            }
        } else {
            msg!(
                "Token must be delegated to {}",
                self.get_msol_from_authority.key
            );
            return Err(ProgramError::InvalidArgument);
        }
        Ok(())
    }

    fn check_transfer_sol_to(&self) -> ProgramResult {
        check_owner_program(&self.transfer_sol_to, &system_program::ID, "transfer_from")?;
        Ok(())
    }

    // fn liquid_unstake()
    pub fn process(&mut self, msol_amount: u64) -> ProgramResult {
        msg!("enter LiquidUnstake");

        self.state
            .check_msol_mint(self.msol_mint.to_account_info().key)?;
        self.state
            .check_liq_pool_sol_leg_pda(self.liq_pool_sol_leg_pda.key)?;
        self.state
            .liq_pool
            .check_liq_pool_msol_leg(self.liq_pool_msol_leg.to_account_info().key)?;
        self.check_get_msol_from(msol_amount)?;
        self.check_transfer_sol_to()?;
        let is_treasury_msol_ready_for_transfer = self
            .state
            .check_treasury_msol_account(&self.treasury_msol_account)?;
        check_address(
            self.system_program.key,
            &system_program::ID,
            "system_program",
        )?;
        check_address(self.token_program.key, &spl_token::ID, "token_program")?;

        let max_lamports = self
            .liq_pool_sol_leg_pda
            .lamports()
            .saturating_sub(self.state.rent_exempt_for_token_acc);

        // fee is computed based on the liquidity *after* the user takes the sol
        let user_remove_lamports = self.state.calc_lamports_from_msol_amount(msol_amount)?;
        let liquid_unstake_fee = if user_remove_lamports >= max_lamports {
            // user is removing all liquidity
            self.state.liq_pool.lp_max_fee
        } else {
            let after_lamports = max_lamports - user_remove_lamports; //how much will be left?
            self.state.liq_pool.linear_fee(after_lamports)
        };

        // compute fee in msol
        let msol_fee = liquid_unstake_fee.apply(msol_amount);
        msg!("msol_fee {}", msol_fee);

        // fee goes into treasury & LPs, so the user receives lamport value of data.msol_amount - msol_fee
        // compute how many lamports the msol_amount the user is "selling" (minus fee) is worth
        let working_lamports_value = self
            .state
            .calc_lamports_from_msol_amount(msol_amount - msol_fee)?;

        // it can't be more than what's in the LiqPool
        if working_lamports_value + self.state.rent_exempt_for_token_acc
            > self.liq_pool_sol_leg_pda.lamports()
        {
            return Err(CommonError::InsufficientLiquidity.into());
        }

        check_min_amount(
            working_lamports_value,
            self.state.min_withdraw,
            "withdraw SOL",
        )?;

        //transfer SOL from the liq-pool to the user
        if working_lamports_value > 0 {
            self.state.with_liq_pool_sol_leg_seeds(|sol_seeds| {
                invoke_signed(
                    &system_instruction::transfer(
                        self.liq_pool_sol_leg_pda.key,
                        self.transfer_sol_to.key,
                        working_lamports_value,
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

        // cut 25% from the fee for the treasury
        let treasury_msol_cut = if is_treasury_msol_ready_for_transfer {
            self.state.liq_pool.treasury_cut.apply(msol_fee)
        } else {
            0
        };
        msg!("treasury_msol_cut {}", treasury_msol_cut);

        //transfer mSOL to the liq-pool
        transfer(
            CpiContext::new(
                self.token_program.clone(),
                Transfer {
                    from: self.get_msol_from.to_account_info(),
                    to: self.liq_pool_msol_leg.to_account_info(),
                    authority: self.get_msol_from_authority.clone(),
                },
            ),
            msol_amount - treasury_msol_cut,
        )?;

        //transfer treasury cut to treasury_msol_account
        if treasury_msol_cut > 0 {
            transfer(
                CpiContext::new(
                    self.token_program.clone(),
                    Transfer {
                        from: self.get_msol_from.to_account_info(),
                        to: self.treasury_msol_account.to_account_info(),
                        authority: self.get_msol_from_authority.clone(),
                    },
                ),
                treasury_msol_cut,
            )?;
        }

        Ok(())
    }
}
