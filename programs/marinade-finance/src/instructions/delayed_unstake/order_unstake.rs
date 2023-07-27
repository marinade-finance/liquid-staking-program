use anchor_lang::prelude::*;
use anchor_spl::token::{burn, Burn, Mint, Token, TokenAccount};

use crate::{
    checks::check_token_source_account, error::MarinadeError,
    events::delayed_unstake::OrderUnstakeEvent, state::delayed_unstake_ticket::TicketAccountData,
    State,
};

#[derive(Accounts)]
pub struct OrderUnstake<'info> {
    #[account(
        mut,
        has_one = msol_mint
    )]
    pub state: Box<Account<'info, State>>,
    #[account(mut)]
    pub msol_mint: Box<Account<'info, Mint>>,

    // Note: Ticket beneficiary is burn_msol_from.owner
    #[account(
        mut,
        token::mint = state.msol_mint
    )]
    pub burn_msol_from: Box<Account<'info, TokenAccount>>,

    pub burn_msol_authority: Signer<'info>, // burn_msol_from acc must be pre-delegated with enough amount to this key or input owner signature here

    #[account(
        zero,
        rent_exempt = enforce
    )]
    pub new_ticket_account: Box<Account<'info, TicketAccountData>>,

    pub clock: Sysvar<'info, Clock>,
    pub rent: Sysvar<'info, Rent>,
    pub token_program: Program<'info, Token>,
}

impl<'info> OrderUnstake<'info> {
    // fn order_unstake() // create delayed-unstake Ticket-account
    pub fn process(&mut self, msol_amount: u64) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        check_token_source_account(
            &self.burn_msol_from,
            self.burn_msol_authority.key,
            msol_amount,
        )
        .map_err(|e| e.with_account_name("burn_msol_from"))?;
        let ticket_beneficiary = self.burn_msol_from.owner;
        let user_msol_balance = self.burn_msol_from.amount;

        // save msol price source
        let total_virtual_staked_lamports = self.state.total_virtual_staked_lamports();
        let msol_supply = self.state.msol_supply;

        let sol_value_of_msol_burned = self.state.msol_to_sol(msol_amount)?;
        // apply delay_unstake_fee to avoid economical attacks
        // delay_unstake_fee must be >= one epoch staking rewards
        let delay_unstake_fee_lamports = self
            .state
            .delayed_unstake_fee
            .apply(sol_value_of_msol_burned);
        // the fee value will be burned but not delivered, thus increasing mSOL value slightly for all mSOL holders
        let lamports_for_user = sol_value_of_msol_burned - delay_unstake_fee_lamports;

        require_gte!(
            lamports_for_user,
            self.state.min_withdraw,
            MarinadeError::WithdrawAmountIsTooLow
        );

        // record for event and then update
        let circulating_ticket_balance = self.state.circulating_ticket_balance;
        let circulating_ticket_count = self.state.circulating_ticket_count;
        // circulating_ticket_balance +
        self.state.circulating_ticket_balance += lamports_for_user;
        self.state.circulating_ticket_count += 1;

        // burn mSOL
        burn(
            CpiContext::new(
                self.token_program.to_account_info(),
                Burn {
                    mint: self.msol_mint.to_account_info(),
                    from: self.burn_msol_from.to_account_info(),
                    authority: self.burn_msol_authority.to_account_info(),
                },
            ),
            msol_amount,
        )?;
        self.state.on_msol_burn(msol_amount)?;

        // initialize new_ticket_account
        let created_epoch = self.clock.epoch
            + if self.clock.epoch == self.state.stake_system.last_stake_delta_epoch {
                1
            } else {
                0
            };
        self.new_ticket_account.set_inner(TicketAccountData {
            state_address: self.state.key(),
            beneficiary: ticket_beneficiary,
            lamports_amount: lamports_for_user,
            created_epoch,
        });
        emit!(OrderUnstakeEvent {
            state: self.state.key(),
            ticket_epoch: created_epoch,
            ticket: self.new_ticket_account.key(),
            beneficiary: ticket_beneficiary,
            user_msol_balance,
            circulating_ticket_count,
            circulating_ticket_balance,
            burned_msol_amount: msol_amount,
            sol_amount: lamports_for_user,
            fee_bp_cents: self.state.delayed_unstake_fee.bp_cents,
            total_virtual_staked_lamports,
            msol_supply,
        });

        Ok(())
    }
}
