use anchor_lang::prelude::*;
use anchor_spl::token::{burn, spl_token, Burn, Mint, Token, TokenAccount};

use crate::{
    checks::{check_address, check_min_amount},
    state::delayed_unstake_ticket::TicketAccountData as DelayedUnstakeTicket,
    State,
};

#[derive(Accounts)]
pub struct OrderUnstake<'info> {
    #[account(mut, has_one = msol_mint)]
    pub state: Box<Account<'info, State>>,
    #[account(mut)]
    pub msol_mint: Box<Account<'info, Mint>>,

    // Note: Ticket beneficiary is burn_msol_from.owner
    #[account(mut, token::mint = state.msol_mint)]
    pub burn_msol_from: Box<Account<'info, TokenAccount>>,

    pub burn_msol_authority: Signer<'info>, // burn_msol_from acc must be pre-delegated with enough amount to this key or input owner signature here

    #[account(zero, rent_exempt = enforce)]
    pub new_delayed_unstake_ticket: Box<Account<'info, DelayedUnstakeTicket>>,

    pub clock: Sysvar<'info, Clock>,
    pub rent: Sysvar<'info, Rent>,
    pub token_program: Program<'info, Token>,
}

impl<'info> OrderUnstake<'info> {
    fn check_burn_msol_from(&self, msol_amount: u64) -> Result<()> {
        if msol_amount == 0 {
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }

        // if delegated, check delegated amount
        if *self.burn_msol_authority.key == self.burn_msol_from.owner {
            if self.burn_msol_from.amount < msol_amount {
                msg!(
                    "Requested to unstake {} mSOL lamports but have only {}",
                    msol_amount,
                    self.burn_msol_from.amount
                );
                return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
            }
        } else if self
            .burn_msol_from
            .delegate
            .contains(self.burn_msol_authority.key)
        {
            // if delegated, check delegated amount
            // delegated_amount & delegate must be set on the user's msol account before calling OrderUnstake
            if self.burn_msol_from.delegated_amount < msol_amount {
                msg!(
                    "Delegated {} mSOL lamports. Requested {}",
                    self.burn_msol_from.delegated_amount,
                    msol_amount
                );
                return Err(Error::from(ProgramError::InsufficientFunds).with_source(source!()));
            }
        } else {
            msg!(
                "Token must be delegated to {}",
                self.burn_msol_authority.key
            );
            return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
        }
        Ok(())
    }

    // fn order_unstake() // create delayed-unstake Ticket-account
    pub fn process(&mut self, msol_amount: u64) -> Result<()> {
        // fn order_unstake()
        check_address(self.token_program.key, &spl_token::ID, "token_program")?;
        self.check_burn_msol_from(msol_amount)?;
        let ticket_beneficiary = self.burn_msol_from.owner;

        let lamports_amount = self.state.calc_lamports_from_msol_amount(msol_amount)?;

        check_min_amount(lamports_amount, self.state.min_withdraw, "withdraw SOL")?;

        // circulating_ticket_balance +
        self.state.circulating_ticket_balance = self
            .state
            .circulating_ticket_balance
            .checked_add(lamports_amount)
            .expect("circulating_ticket_balance overflow");
        self.state.circulating_ticket_count += 1;

        // burn mSOL (no delegate) -- commented here as reference
        // burn(
        //     CpiContext::new(
        //         self.token_program.clone(),
        //         Burn {
        //             mint: self.msol_mint.to_account_info(),
        //             to: self.burn_msol_from.to_account_info(),
        //             authority: self.ticket_beneficiary.clone(),
        //         },
        //     ),
        //     msol_amount,
        // )?;
        // --------
        //burn mSOL (with_token_delegate_authority_seeds)
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

        //initialize new_ticket_account
        self.new_delayed_unstake_ticket.state_address = *self.state.to_account_info().key;
        self.new_delayed_unstake_ticket.beneficiary = ticket_beneficiary;
        self.new_delayed_unstake_ticket.lamports_amount = lamports_amount;
        // If user calls OrderUnstake after we start the stake/unstake delta (close to the end of the epoch),
        // we must set ticket-due as if unstaking was asked **next-epoch**
        // Because there's a delay until the bot actually starts the unstakes
        // and it's not guaranteed that the unstake for the user will be started this epoch
        self.new_delayed_unstake_ticket.created_epoch = self.clock.epoch
            + if self.clock.epoch == self.state.stake_system.last_stake_delta_epoch {
                1
            } else {
                0
            };

        Ok(())
    }
}
