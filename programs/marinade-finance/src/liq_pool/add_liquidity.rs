use crate::AddLiquidity;

use super::LiqPoolHelpers;
use crate::{calc::shares_from_value, checks::*};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, system_instruction, system_program};
use anchor_spl::token::{mint_to, MintTo};

impl<'info> AddLiquidity<'info> {
    fn check_transfer_from(&self, lamports: u64) -> ProgramResult {
        check_owner_program(&self.transfer_from, &system_program::ID, "transfer_from")?;
        if self.transfer_from.lamports() < lamports {
            msg!(
                "{} balance is {} but expected {}",
                self.transfer_from.key,
                self.transfer_from.lamports(),
                lamports
            );
            return Err(ProgramError::InsufficientFunds);
        }
        Ok(())
    }

    fn check_mint_to(&self) -> ProgramResult {
        check_token_mint(&self.mint_to, self.state.liq_pool.lp_mint, "mint_to")?;
        Ok(())
    }

    // fn add_liquidity()
    pub fn process(&mut self, lamports: u64) -> ProgramResult {
        msg!("add-liq pre check");
        check_min_amount(lamports, self.state.min_deposit, "add_liquidity")?;
        self.state
            .liq_pool
            .check_lp_mint(self.lp_mint.to_account_info().key)?;
        self.state
            .check_lp_mint_authority(self.lp_mint_authority.key)?;
        // self.state
        //     .check_msol_mint(self.msol_mint.to_account_info().key)?;
        self.state
            .liq_pool
            .check_liq_pool_msol_leg(self.liq_pool_msol_leg.to_account_info().key)?;
        self.state
            .check_liq_pool_sol_leg_pda(self.liq_pool_sol_leg_pda.key)?;
        self.check_transfer_from(lamports)?;
        self.state
            .liq_pool
            .check_liquidity_cap(lamports, self.liq_pool_sol_leg_pda.lamports())?;
        self.check_mint_to()?;
        check_address(
            self.system_program.key,
            &system_program::ID,
            "system_program",
        )?;
        check_address(self.token_program.key, &spl_token::ID, "token_program")?;

        msg!("add-liq after check");
        // Update virtual lp_supply by real one
        if self.lp_mint.supply > self.state.liq_pool.lp_supply {
            msg!("Someone minted lp tokens without our permission or bug found");
            return Err(ProgramError::InvalidAccountData);
        }
        self.state.liq_pool.lp_supply = self.lp_mint.supply;
        // we need to compute how many LP-shares to mint for this deposit in the liq-pool
        // in order to do that, we need total liq-pool value, to compute LP-share price
        // liq_pool_total_value = liq_pool_sol_account_pda.lamports() + value_from_msol_tokens(liq_pool_msol_account.token.balance)
        // shares_for_user = amount * shares_per_lamport => shares_for_user = amount * total_shares/total_value

        //compute current liq-pool total value BEFORE adding user's deposit
        let sol_leg_lamports = self
            .liq_pool_sol_leg_pda
            .lamports()
            .checked_sub(self.state.rent_exempt_for_token_acc)
            .expect("sol_leg_lamports");
        let msol_leg_value = self
            .state
            .calc_lamports_from_msol_amount(self.liq_pool_msol_leg.amount)
            .expect("msol_leg_value");
        let total_liq_pool_value = sol_leg_lamports + msol_leg_value;
        msg!(
            "liq_pool SOL:{}, liq_pool mSOL value:{} liq_pool_value:{}",
            sol_leg_lamports,
            msol_leg_value,
            total_liq_pool_value
        );

        let shares_for_user = shares_from_value(
            lamports,
            total_liq_pool_value,
            self.state.liq_pool.lp_supply,
        )?;

        msg!("LP for user {}", shares_for_user);

        //we start with a transfer instruction so the user can verify the SOL amount they're staking while approving the transaction
        //transfer sol into liq-pool sol leg
        invoke(
            &system_instruction::transfer(
                self.transfer_from.key,
                self.liq_pool_sol_leg_pda.key,
                lamports,
            ),
            &[
                self.transfer_from.clone(),
                self.liq_pool_sol_leg_pda.clone(),
                self.system_program.clone(),
            ],
        )?;

        //mint liq-pool shares (mSOL-SOL-LP tokens) for the user
        self.state.with_lp_mint_authority_seeds(|mint_seeds| {
            mint_to(
                CpiContext::new_with_signer(
                    self.token_program.clone(),
                    MintTo {
                        mint: self.lp_mint.to_account_info(),
                        to: self.mint_to.to_account_info(),
                        authority: self.lp_mint_authority.clone(),
                    },
                    &[mint_seeds],
                ),
                shares_for_user,
            )
        })?;
        self.state.liq_pool.on_lp_mint(shares_for_user);

        Ok(())
    }
}
