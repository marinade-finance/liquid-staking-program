use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction};

use crate::state::delayed_unstake_ticket::TicketAccountData;
use crate::MarinadeError;
use crate::State;

///How many epochs to wats for ticket. e.g.: Ticket created on epoch 14, ticket is due on epoch 15
const WAIT_EPOCHS: u64 = 1;
///Wait 30 extra minutes from epochs start so the bot has time to withdraw SOL from inactive stake-accounts
const EXTRA_WAIT_SECONDS: i64 = 30 * 60;

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(mut)]
    pub state: Account<'info, State>,
    #[account(mut, seeds = [&state.key().to_bytes(),
            State::RESERVE_SEED],
            bump = state.reserve_bump_seed)]
    pub reserve_pda: SystemAccount<'info>,

    #[account(mut)]
    pub ticket_account: Account<'info, TicketAccountData>,

    #[account(mut)]
    pub transfer_sol_to: SystemAccount<'info>,

    pub clock: Sysvar<'info, Clock>,

    pub system_program: Program<'info, System>,
}

/// Claim instruction: a user claims a Ticket-account
/// This is done once tickets are due, meaning enough time has passed for the
/// bot to complete the unstake process and transfer the requested SOL to reserve_pda.
/// Checks that transfer request amount is less than total requested for unstake
impl<'info> Claim<'info> {
    //
    fn check_ticket_account(&self) -> Result<()> {
        if &self.ticket_account.state_address != self.state.to_account_info().key {
            msg!(
                "Ticket has wrong marinade instance {}",
                self.ticket_account.state_address
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }

        // should be initialized - checked by anchor
        // "initialized" means the first 8 bytes are the Anchor's struct hash magic number

        // not used
        if self.ticket_account.lamports_amount == 0 {
            msg!("Used ticket");
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        };

        //check if ticket is due
        if self.clock.epoch < self.ticket_account.created_epoch + WAIT_EPOCHS {
            msg!("Ticket not due yet");
            return err!(MarinadeError::TicketNotDue);
        }
        // Wait X MORE HOURS FROM THE beginning of the EPOCH to give the bot time to withdraw inactive-stake-accounts
        if self.ticket_account.created_epoch + WAIT_EPOCHS == self.clock.epoch
            && self.clock.unix_timestamp - self.clock.epoch_start_timestamp < EXTRA_WAIT_SECONDS
        {
            msg!(
                "Ticket not ready {} {}",
                self.clock.epoch_start_timestamp,
                self.clock.unix_timestamp
            );
            return err!(MarinadeError::TicketNotReady);
        }

        if self.ticket_account.beneficiary != *self.transfer_sol_to.key {
            msg!("wrong beneficiary");
            return err!(MarinadeError::WrongBeneficiary);
        };

        Ok(())
    }

    pub fn process(&mut self) -> Result<()> {
        // fn claim()
        self.check_ticket_account()?;

        let lamports = self.ticket_account.lamports_amount;
        if lamports > self.state.circulating_ticket_balance {
            msg!(
                "Requested to withdraw {} when only {} is total circulating_ticket_balance",
                lamports,
                self.state.circulating_ticket_balance
            );
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }

        // Real balance not virtual field
        let available_for_claim =
            self.reserve_pda.lamports() - self.state.rent_exempt_for_token_acc;
        if lamports > available_for_claim {
            msg!(
                "Requested to claim {} when only {} ready. Wait a few hours and retry",
                lamports,
                available_for_claim
            );
            //Error: "Wait a few hours and retry"
            return Err(MarinadeError::TicketNotReady.into());
        }

        self.state.circulating_ticket_balance -= lamports;
        self.state.circulating_ticket_count -= 1;
        //disable ticket-account
        self.ticket_account.lamports_amount = 0;

        //transfer sol from reserve_pda to user
        invoke_signed(
            &system_instruction::transfer(self.reserve_pda.key, self.transfer_sol_to.key, lamports),
            &[
                self.system_program.to_account_info(),
                self.reserve_pda.to_account_info(),
                self.transfer_sol_to.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                State::RESERVE_SEED,
                &[self.state.reserve_bump_seed],
            ]],
        )?;
        self.state.on_transfer_from_reserve(lamports)?;

        // move all rent-exempt ticket-account lamports to the user,
        // the ticket-account will be deleted eventually because is no longer rent-exempt
        let source_account_info = self.ticket_account.to_account_info();
        let dest_account_info = self.transfer_sol_to.to_account_info();
        let dest_starting_lamports = dest_account_info.lamports();
        **dest_account_info.lamports.borrow_mut() = dest_starting_lamports
            .checked_add(source_account_info.lamports())
            .ok_or(ProgramError::InvalidAccountData)?;
        **source_account_info.lamports.borrow_mut() = 0;

        Ok(())
    }
}
