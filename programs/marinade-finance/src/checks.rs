use crate::CommonError;
use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, TokenAccount};

pub fn check_min_amount(amount: u64, min_amount: u64, action_name: &str) -> ProgramResult {
    if amount >= min_amount {
        Ok(())
    } else {
        msg!(
            "{}: Number too low {} (min is {})",
            action_name,
            amount,
            min_amount,
        );
        Err(CommonError::NumberTooLow.into())
    }
}

pub fn check_address(
    actual_address: &Pubkey,
    reference_address: &Pubkey,
    field_name: &str,
) -> ProgramResult {
    if actual_address == reference_address {
        Ok(())
    } else {
        msg!(
            "Invalid {} address: expected {} got {}",
            field_name,
            reference_address,
            actual_address
        );
        Err(ProgramError::InvalidArgument)
    }
}

pub fn check_owner_program<'info, A: ToAccountInfo<'info>>(
    account: &A,
    owner: &Pubkey,
    field_name: &str,
) -> ProgramResult {
    let actual_owner = account.to_account_info().owner;
    if actual_owner == owner {
        Ok(())
    } else {
        msg!(
            "Invalid {} owner_program: expected {} got {}",
            field_name,
            owner,
            actual_owner
        );
        Err(ProgramError::InvalidArgument)
    }
}

pub fn check_mint_authority(
    mint: &Mint,
    mint_authority: Pubkey,
    field_name: &str,
) -> ProgramResult {
    if mint.mint_authority.contains(&mint_authority) {
        Ok(())
    } else {
        msg!(
            "Invalid {} mint authority {}. Expected {}",
            field_name,
            mint.mint_authority.unwrap_or_default(),
            mint_authority
        );
        Err(ProgramError::InvalidAccountData)
    }
}

pub fn check_freeze_authority(mint: &Mint, field_name: &str) -> ProgramResult {
    if mint.freeze_authority.is_none() {
        Ok(())
    } else {
        msg!("Mint {} must have freeze authority not set", field_name);
        Err(ProgramError::InvalidAccountData)
    }
}

pub fn check_mint_empty(mint: &Mint, field_name: &str) -> ProgramResult {
    if mint.supply == 0 {
        Ok(())
    } else {
        msg!("Non empty mint {} supply: {}", field_name, mint.supply);
        Err(ProgramError::InvalidArgument)
    }
}

pub fn check_token_mint(token: &TokenAccount, mint: Pubkey, field_name: &str) -> ProgramResult {
    if token.mint == mint {
        Ok(())
    } else {
        msg!(
            "Invalid token {} mint {}. Expected {}",
            field_name,
            token.mint,
            mint
        );
        Err(ProgramError::InvalidAccountData)
    }
}

pub fn check_token_owner(token: &TokenAccount, owner: &Pubkey, field_name: &str) -> ProgramResult {
    if token.owner == *owner {
        Ok(())
    } else {
        msg!(
            "Invalid token account {} owner {}. Expected {}",
            field_name,
            token.owner,
            owner
        );
        Err(ProgramError::InvalidAccountData)
    }
}
