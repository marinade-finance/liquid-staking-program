use crate::{calc::proportional, checks::check_min_amount, state::liq_pool::LiqPool, State};
use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_spl::token::{
    burn, transfer as transfer_token, Burn, Mint, Token, TokenAccount, Transfer as TransferToken,
};

#[derive(Accounts)]
pub struct RemoveLiquidity<'info> {
    #[account(mut)]
    pub state: Box<Account<'info, State>>,

    #[account(
        mut,
        address = state.liq_pool.lp_mint
    )]
    pub lp_mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        token::mint = state.liq_pool.lp_mint
    )]
    pub burn_from: Box<Account<'info, TokenAccount>>,
    pub burn_from_authority: Signer<'info>,

    #[account(mut)]
    pub transfer_sol_to: SystemAccount<'info>,

    #[account(
        mut,
        token::mint = state.msol_mint
    )]
    pub transfer_msol_to: Box<Account<'info, TokenAccount>>,

    // legs
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

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

impl<'info> RemoveLiquidity<'info> {
    fn check_burn_from(&self, tokens: u64) -> Result<()> {
        // if delegated, check delegated amount
        if *self.burn_from_authority.key == self.burn_from.owner {
            if self.burn_from.amount < tokens {
                msg!(
                    "Requested to remove {} liquidity but have only {}",
                    tokens,
                    self.burn_from.amount
                );
                return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
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
                return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
            }
        } else {
            msg!(
                "Token must be delegated to {}",
                self.burn_from_authority.key
            );
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        }
        Ok(())
    }

    pub fn process(&mut self, tokens: u64) -> Result<()> {
        msg!("rem-liq pre check");
        self.check_burn_from(tokens)?;

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
                sol_out_amount,
            )?;
        }

        if msol_out_amount > 0 {
            msg!("transfer mSOL");
            transfer_token(
                CpiContext::new_with_signer(
                    self.token_program.to_account_info(),
                    TransferToken {
                        from: self.liq_pool_msol_leg.to_account_info(),
                        to: self.transfer_msol_to.to_account_info(),
                        authority: self.liq_pool_msol_leg_authority.to_account_info(),
                    },
                    &[&[
                        &self.state.key().to_bytes(),
                        LiqPool::MSOL_LEG_AUTHORITY_SEED,
                        &[self.state.liq_pool.msol_leg_authority_bump_seed],
                    ]],
                ),
                msol_out_amount,
            )?;
        }

        burn(
            CpiContext::new(
                self.token_program.to_account_info(),
                Burn {
                    mint: self.lp_mint.to_account_info(),
                    from: self.burn_from.to_account_info(),
                    authority: self.burn_from_authority.to_account_info(),
                },
            ),
            tokens,
        )?;
        self.state.liq_pool.on_lp_burn(tokens)?;

        msg!("end instruction rem-liq");
        Ok(())
    }
}
