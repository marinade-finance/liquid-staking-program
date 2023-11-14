use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_spl::token::{
    transfer as transfer_token, Mint, Token, TokenAccount, Transfer as TransferToken,
};

use crate::{
    checks::check_token_source_account, events::liq_pool::LiquidUnstakeEvent,
    state::liq_pool::LiqPool, MarinadeError, State,
};

#[derive(Accounts)]
pub struct LiquidUnstake<'info> {
    #[account(
        mut,
        has_one = treasury_msol_account,
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

    /// CHECK: deserialized in code, must be the one in State (State has_one treasury_msol_account)
    #[account(mut)]
    pub treasury_msol_account: UncheckedAccount<'info>,

    #[account(
        mut,
        token::mint = state.msol_mint
    )]
    pub get_msol_from: Box<Account<'info, TokenAccount>>,
    pub get_msol_from_authority: Signer<'info>, //burn_msol_from owner or delegate_authority

    #[account(mut)]
    pub transfer_sol_to: SystemAccount<'info>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

impl<'info> LiquidUnstake<'info> {
    // fn liquid_unstake()
    pub fn process(&mut self, msol_amount: u64) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        check_token_source_account(
            &self.get_msol_from,
            self.get_msol_from_authority.key,
            msol_amount,
        )
        .map_err(|e| e.with_account_name("get_msol_from"))?;
        let user_sol_balance = self.transfer_sol_to.lamports();
        let user_msol_balance = self.get_msol_from.amount;
        let treasury_msol_balance = self
            .state
            .get_treasury_msol_balance(&self.treasury_msol_account);

        let liq_pool_msol_balance = self.liq_pool_msol_leg.amount;
        let liq_pool_sol_balance = self.liq_pool_sol_leg_pda.lamports();
        let liq_pool_available_sol_balance =
            liq_pool_sol_balance.saturating_sub(self.state.rent_exempt_for_token_acc);

        // fee is computed based on the liquidity *after* the user takes the sol
        let user_remove_lamports = self.state.msol_to_sol(msol_amount)?;
        let liquid_unstake_fee = if user_remove_lamports >= liq_pool_available_sol_balance {
            // user is removing all liquidity
            self.state.liq_pool.lp_max_fee
        } else {
            let after_lamports = liq_pool_available_sol_balance - user_remove_lamports; //how much will be left?
            self.state.liq_pool.linear_fee(after_lamports)
        };

        // compute fee in msol
        let msol_fee = liquid_unstake_fee.apply(msol_amount);
        msg!("msol_fee {}", msol_fee);

        // fee goes into treasury & LPs, so the user receives lamport value of data.msol_amount - msol_fee
        // compute how many lamports the msol_amount the user is "selling" (minus fee) is worth
        let working_lamports_value = self.state.msol_to_sol(msol_amount - msol_fee)?;

        // it can't be more than what's in the LiqPool
        if working_lamports_value + self.state.rent_exempt_for_token_acc
            > self.liq_pool_sol_leg_pda.lamports()
        {
            return err!(MarinadeError::InsufficientLiquidity);
        }

        require_gte!(
            working_lamports_value,
            self.state.min_withdraw,
            MarinadeError::WithdrawAmountIsTooLow
        );

        //transfer SOL from the liq-pool to the user
        if working_lamports_value > 0 {
            transfer(
                CpiContext::new_with_signer(
                    self.system_program.to_account_info(),
                    Transfer {
                        from: self.liq_pool_sol_leg_pda.to_account_info(),
                        to: self.transfer_sol_to.to_account_info(),
                    },
                    &[&[
                        &self.state.key().to_bytes(),
                        LiqPool::SOL_LEG_SEED,
                        &[self.state.liq_pool.sol_leg_bump_seed],
                    ]],
                ),
                working_lamports_value,
            )?;
        }

        // cut 25% from the fee for the treasury
        let treasury_msol_cut = if treasury_msol_balance.is_some() {
            self.state.liq_pool.treasury_cut.apply(msol_fee)
        } else {
            0
        };
        msg!("treasury_msol_cut {}", treasury_msol_cut);

        //transfer mSOL to the liq-pool
        transfer_token(
            CpiContext::new(
                self.token_program.to_account_info(),
                TransferToken {
                    from: self.get_msol_from.to_account_info(),
                    to: self.liq_pool_msol_leg.to_account_info(),
                    authority: self.get_msol_from_authority.to_account_info(),
                },
            ),
            msol_amount - treasury_msol_cut,
        )?;

        //transfer treasury cut to treasury_msol_account
        if treasury_msol_cut > 0 {
            transfer_token(
                CpiContext::new(
                    self.token_program.to_account_info(),
                    TransferToken {
                        from: self.get_msol_from.to_account_info(),
                        to: self.treasury_msol_account.to_account_info(),
                        authority: self.get_msol_from_authority.to_account_info(),
                    },
                ),
                treasury_msol_cut,
            )?;
        }

        emit!(LiquidUnstakeEvent {
            state: self.state.key(),
            msol_owner: self.get_msol_from.owner,
            msol_amount,
            liq_pool_sol_balance,
            liq_pool_msol_balance,
            treasury_msol_balance,
            user_msol_balance,
            user_sol_balance,
            msol_fee,
            treasury_msol_cut,
            sol_amount: working_lamports_value,
            lp_liquidity_target: self.state.liq_pool.lp_liquidity_target,
            lp_max_fee: self.state.liq_pool.lp_max_fee,
            lp_min_fee: self.state.liq_pool.lp_min_fee,
            treasury_cut: self.state.liq_pool.treasury_cut
        });

        Ok(())
    }
}
