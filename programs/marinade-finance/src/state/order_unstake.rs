use anchor_lang::prelude::*;
use anchor_spl::token::{burn, Burn};

use crate::{
    checks::{check_address, check_min_amount, check_owner_program, check_token_mint},
    OrderUnstake,
};

impl<'info> OrderUnstake<'info> {
    fn check_burn_msol_from(&self, msol_amount: u64) -> ProgramResult {
        check_token_mint(&self.burn_msol_from, self.state.msol_mint, "burn_msol_from")?;

        if msol_amount == 0 {
            return Err(ProgramError::InvalidAccountData);
        }

        // if delegated, check delegated amount
        if *self.burn_msol_authority.key == self.burn_msol_from.owner {
            if self.burn_msol_from.amount < msol_amount {
                msg!(
                    "Requested to unstake {} mSOL lamports but have only {}",
                    msol_amount,
                    self.burn_msol_from.amount
                );
                return Err(ProgramError::InsufficientFunds);
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
                return Err(ProgramError::InsufficientFunds);
            }
        } else {
            msg!(
                "Token must be delegated to {}",
                self.burn_msol_authority.key
            );
            return Err(ProgramError::InvalidArgument);
        }
        Ok(())
    }

    fn check_new_ticket_account(&self) -> ProgramResult {
        // ticket account program-owner must be marinade (TODO: I think it was checked by anchor already)
        check_owner_program(
            &self.new_ticket_account,
            &crate::ID, //owner-program should be marinade
            "new_ticket_account",
        )?;

        // should be uninitialized - checked by anchor
        // should be rent-exempt - checked by anchor
        Ok(())
    }

    // fn order_unstake() // create delayed-unstake Ticket-account
    pub fn process(&mut self, msol_amount: u64) -> ProgramResult {
        // fn order_unstake()
        check_address(self.token_program.key, &spl_token::ID, "token_program")?;
        self.check_new_ticket_account()?;
        self.state
            .check_msol_mint(self.msol_mint.to_account_info().key)?;
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
                self.token_program.clone(),
                Burn {
                    mint: self.msol_mint.to_account_info(),
                    to: self.burn_msol_from.to_account_info(),
                    authority: self.burn_msol_authority.clone(),
                },
            ),
            msol_amount,
        )?;
        self.state.on_msol_burn(msol_amount);

        //initialize new_ticket_account
        self.new_ticket_account.state_address = *self.state.to_account_info().key;
        self.new_ticket_account.beneficiary = ticket_beneficiary;
        self.new_ticket_account.lamports_amount = lamports_amount;
        // If user calls OrderUnstake after we start the stake/unstake delta (close to the end of the epoch),
        // we must set ticket-due as if unstaking was asked **next-epoch**
        // Because there's a delay until the bot actually starts the unstakes
        // and it's not guaranteed that the unstake for the user will be started this epoch
        self.new_ticket_account.created_epoch = self.clock.epoch
            + if self.clock.epoch == self.state.stake_system.last_stake_delta_epoch {
                1
            } else {
                0
            };

        Ok(())
    }
}
