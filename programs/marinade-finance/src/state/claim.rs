use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction, system_program};

use crate::{
    checks::{check_address, check_owner_program},
    state::StateHelpers,
    Claim, CommonError,
};

///How many epochs to wats for ticket. e.g.: Ticket created on epoch 14, ticket is due on epoch 16
const WAIT_EPOCHS: u64 = 2;
///Wait 30 extra minutes from epochs start so the bot has time to withdraw SOL from inactive stake-accounts
const EXTRA_WAIT_SECONDS: i64 = 30 * 60;

/// Claim instruction: a user claims a Ticket-account
/// This is done once tickets are due, meaning enough time has passed for the
/// bot to complete the unstake process and transfer the requested SOL to reserve_pda.
/// Checks that transfer request amount is less than total requested for unstake
impl<'info> Claim<'info> {
    //
    fn check_ticket_account(&self) -> ProgramResult {
        // ticket account program-owner must be marinade  (TODO: I think it was checked by anchor already)
        check_owner_program(
            &self.ticket_account,
            &crate::ID, //owner-program should be marinade
            "ticket_account",
        )?;
        if &self.ticket_account.state_address != self.state.to_account_info().key {
            msg!(
                "Ticket has wrong marinade instance {}",
                self.ticket_account.state_address
            );
            return Err(ProgramError::InvalidAccountData);
        }

        // should be initialized - checked by anchor
        // "initialized" means the first 8 bytes are the Anchor's struct hash magic number

        // not used
        if self.ticket_account.lamports_amount == 0 {
            msg!("Used ticket");
            return Err(ProgramError::InvalidAccountData);
        };

        //check if ticket is due
        if self.clock.epoch < self.ticket_account.created_epoch + WAIT_EPOCHS {
            msg!("Ticket not due yet");
            return Err(CommonError::TicketNotDue.into());
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
            return Err(CommonError::TicketNotReady.into());
        }

        if self.ticket_account.beneficiary != *self.transfer_sol_to.key {
            msg!("wrong beneficiary");
            return Err(CommonError::WrongBeneficiary.into());
        };

        Ok(())
    }

    pub fn process(&mut self) -> ProgramResult {
        // fn claim()
        check_address(
            self.system_program.to_account_info().key,
            &system_program::ID,
            "system_program",
        )?;
        check_owner_program(
            &self.transfer_sol_to,
            &system_program::ID,
            "transfer_sol_to",
        )?;
        self.state.check_reserve_address(self.reserve_pda.key)?;
        self.check_ticket_account()?;

        let lamports = self.ticket_account.lamports_amount;
        if lamports > self.state.circulating_ticket_balance {
            msg!(
                "Requested to withdraw {} when only {} is total circulating_ticket_balance",
                lamports,
                self.state.circulating_ticket_balance
            );
            return Err(ProgramError::InvalidAccountData);
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
            return Err(CommonError::TicketNotReady.into());
        }

        self.state.circulating_ticket_balance -= lamports;
        self.state.circulating_ticket_count -= 1;
        //disable ticket-account
        self.ticket_account.lamports_amount = 0;

        //transfer sol from reserve_pda to user
        self.state.with_reserve_seeds(|seeds| {
            invoke_signed(
                &system_instruction::transfer(
                    self.reserve_pda.key,
                    self.transfer_sol_to.key,
                    lamports,
                ),
                &[
                    self.system_program.clone(),
                    self.reserve_pda.clone(),
                    self.transfer_sol_to.clone(),
                ],
                &[seeds],
            )
        })?;
        self.state.on_transfer_from_reserve(lamports);

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
