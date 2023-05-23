use crate::state::liq_pool::LiqPool;
use crate::State;
use crate::{calc::shares_from_value, checks::*};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::system_program;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_spl::token::{mint_to, Mint, MintTo, Token, TokenAccount};

#[derive(Accounts)]
pub struct AddLiquidity<'info> {
    #[account(mut)]
    pub state: Box<Account<'info, State>>,

    #[account(mut, address = state.liq_pool.lp_mint)]
    pub lp_mint: Box<Account<'info, Mint>>,

    /// CHECK: PDA
    #[account(seeds = [&state.key().to_bytes(),
            LiqPool::LP_MINT_AUTHORITY_SEED],
            bump = state.liq_pool.lp_mint_authority_bump_seed)]
    pub lp_mint_authority: UncheckedAccount<'info>,

    // liq_pool_msol_leg to be able to compute current msol value in liq_pool
    #[account(address = state.liq_pool.msol_leg)]
    pub liq_pool_msol_leg: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [&state.key().to_bytes(),
            LiqPool::SOL_LEG_SEED],
            bump = state.liq_pool.sol_leg_bump_seed)]
    pub liq_pool_sol_leg_pda: SystemAccount<'info>,

    #[account(mut, owner = system_program::ID)]
    pub transfer_from: Signer<'info>,

    // user SPL-Token account to send the newly minted LP tokens
    #[account(mut, token::mint = state.liq_pool.lp_mint)]
    pub mint_to: Box<Account<'info, TokenAccount>>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

impl<'info> AddLiquidity<'info> {
    fn check_transfer_from(&self, lamports: u64) -> Result<()> {
        if self.transfer_from.lamports() < lamports {
            msg!(
                "{} balance is {} but expected {}",
                self.transfer_from.key,
                self.transfer_from.lamports(),
                lamports
            );
            return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
        }
        Ok(())
    }

    // fn add_liquidity()
    pub fn process(&mut self, lamports: u64) -> Result<()> {
        msg!("add-liq pre check");
        check_min_amount(lamports, self.state.min_deposit, "add_liquidity")?;
        self.check_transfer_from(lamports)?;
        self.state
            .liq_pool
            .check_liquidity_cap(lamports, self.liq_pool_sol_leg_pda.lamports())?;

        msg!("add-liq after check");
        // Update virtual lp_supply by real one
        if self.lp_mint.supply > self.state.liq_pool.lp_supply {
            msg!("Someone minted lp tokens without our permission or bug found");
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
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
        transfer(
            CpiContext::new(
                self.system_program.to_account_info(),
                Transfer {
                    from: self.transfer_from.to_account_info(),
                    to: self.liq_pool_sol_leg_pda.to_account_info(),
                },
            ),
            lamports,
        )?;

        //mint liq-pool shares (mSOL-SOL-LP tokens) for the user
        mint_to(
            CpiContext::new_with_signer(
                self.token_program.to_account_info(),
                MintTo {
                    mint: self.lp_mint.to_account_info(),
                    to: self.mint_to.to_account_info(),
                    authority: self.lp_mint_authority.to_account_info(),
                },
                &[&[
                    &self.state.key().to_bytes(),
                    LiqPool::LP_MINT_AUTHORITY_SEED,
                    &[self.state.liq_pool.lp_mint_authority_bump_seed],
                ]],
            ),
            shares_for_user,
        )?;
        self.state.liq_pool.on_lp_mint(shares_for_user);

        Ok(())
    }
}