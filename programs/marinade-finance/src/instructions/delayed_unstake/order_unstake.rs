use anchor_lang::prelude::*;
use anchor_spl::token::{burn, Burn, Mint, Token, TokenAccount};

use crate::{
    error::MarinadeError, events::delayed_unstake::OrderUnstakeEvent, require_lte,
    state::delayed_unstake_ticket::TicketAccountData, State,
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
    fn check_burn_msol_from(&self, msol_amount: u64) -> Result<()> {
        if self
            .burn_msol_from
            .delegate
            .contains(self.burn_msol_authority.key)
        {
            // if delegated, check delegated amount
            // delegated_amount & delegate must be set on the user's msol account before calling OrderUnstake
            require_lte!(
                msol_amount,
                self.burn_msol_from.delegated_amount,
                MarinadeError::NotEnoughUserFunds
            );
        } else if self.burn_msol_authority.key() == self.burn_msol_from.owner {
            require_lte!(
                msol_amount,
                self.burn_msol_from.amount,
                MarinadeError::NotEnoughUserFunds
            );
        } else {
            return err!(MarinadeError::WrongTokenOwnerOrDelegate).map_err(|e| {
                e.with_account_name("burn_msol_from")
                    .with_pubkeys((self.burn_msol_from.owner, self.burn_msol_authority.key()))
            });
        }
        Ok(())
    }

    // fn order_unstake() // create delayed-unstake Ticket-account
    pub fn process(&mut self, msol_amount: u64) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);

        self.check_burn_msol_from(msol_amount)?;
        let ticket_beneficiary = self.burn_msol_from.owner;
        let user_msol_balance = self.burn_msol_from.amount;

        // save msol price source
        let total_virtual_staked_lamports = self.state.total_virtual_staked_lamports();
        let msol_supply = self.state.msol_supply;
        let lamports_amount = self.state.calc_lamports_from_msol_amount(msol_amount)?;

        require_gte!(
            lamports_amount,
            self.state.min_withdraw,
            MarinadeError::WithdrawAmountIsTooLow
        );

        // record for event and then update
        let circulating_ticket_balance = self.state.circulating_ticket_balance;
        let circulating_ticket_count = self.state.circulating_ticket_count;
        // circulating_ticket_balance +
        self.state.circulating_ticket_balance += lamports_amount;
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
            lamports_amount,
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
            sol_amount: lamports_amount,
            total_virtual_staked_lamports,
            msol_supply,
        });

        Ok(())
    }
}
