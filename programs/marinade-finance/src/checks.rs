use crate::MarinadeError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::stake::state::StakeState;
use anchor_spl::token::{Mint, TokenAccount};

pub fn check_owner_program<'info, A: ToAccountInfo<'info>>(
    account: &A,
    owner: &Pubkey,
    field_name: &str,
) -> Result<()> {
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
        Err(Error::from(ProgramError::InvalidArgument)
            .with_account_name(field_name)
            .with_pubkeys((*actual_owner, *owner))
            .with_source(source!()))
    }
}

pub fn check_mint_authority(mint: &Mint, mint_authority: &Pubkey, field_name: &str) -> Result<()> {
    if mint.mint_authority.contains(mint_authority) {
        Ok(())
    } else {
        msg!(
            "Invalid {} mint authority {}. Expected {}",
            field_name,
            mint.mint_authority.unwrap_or_default(),
            mint_authority
        );
        Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()))
    }
}

pub fn check_freeze_authority(mint: &Mint, field_name: &str) -> Result<()> {
    if mint.freeze_authority.is_none() {
        Ok(())
    } else {
        msg!("Mint {} must have freeze authority not set", field_name);
        Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()))
    }
}

pub fn check_mint_empty(mint: &Mint, field_name: &str) -> Result<()> {
    if mint.supply == 0 {
        Ok(())
    } else {
        msg!("Non empty mint {} supply: {}", field_name, mint.supply);
        Err(Error::from(ProgramError::InvalidArgument).with_source(source!()))
    }
}

pub fn check_token_mint(token: &TokenAccount, mint: &Pubkey, field_name: &str) -> Result<()> {
    if token.mint == *mint {
        Ok(())
    } else {
        msg!(
            "Invalid token {} mint {}. Expected {}",
            field_name,
            token.mint,
            mint
        );
        Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()))
    }
}

pub fn check_token_owner(token: &TokenAccount, owner: &Pubkey, field_name: &str) -> Result<()> {
    if token.owner == *owner {
        Ok(())
    } else {
        msg!(
            "Invalid token account {} owner {}. Expected {}",
            field_name,
            token.owner,
            owner
        );
        Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()))
    }
}

// check that the account is delegated and to the right validator
// also that the stake amount is updated
pub fn check_stake_amount_and_validator(
    stake_state: &StakeState,
    expected_stake_amount: u64,
    validator_vote_pubkey: &Pubkey,
) -> Result<()> {
    let currently_staked = if let Some(delegation) = stake_state.delegation() {
        require_keys_eq!(
            delegation.voter_pubkey,
            *validator_vote_pubkey,
            MarinadeError::WrongValidatorAccountOrIndex
        );
        delegation.stake
    } else {
        return err!(MarinadeError::StakeNotDelegated);
    };
    // do not allow to operate on an account where last_update_delegated_lamports != currently_staked
    if currently_staked != expected_stake_amount {
        msg!(
            "Operation on a stake account not yet updated. expected stake:{}, current:{}",
            expected_stake_amount,
            currently_staked
        );
        return err!(MarinadeError::StakeAccountNotUpdatedYet);
    }
    Ok(())
}

#[macro_export]
macro_rules! require_lte {
    ($value1: expr, $value2: expr, $error_code: expr $(,)?) => {
        if $value1 > $value2 {
            return Err(error!($error_code).with_values(($value1, $value2)));
        }
    };
}

#[macro_export]
macro_rules! require_lt {
    ($value1: expr, $value2: expr, $error_code: expr $(,)?) => {
        if $value1 >= $value2 {
            return Err(error!($error_code).with_values(($value1, $value2)));
        }
    };
}

pub fn check_token_source_account<'info>(
    source_account: &Account<'info, TokenAccount>,
    authority: &Pubkey,
    token_amount: u64,
) -> Result<()> {
    if source_account.delegate.contains(authority) {
        // if delegated, check delegated amount
        // delegated_amount & delegate must be set on the user's msol account before calling OrderUnstake
        require_lte!(
            token_amount,
            source_account.delegated_amount,
            MarinadeError::NotEnoughUserFunds
        );
    } else if *authority == source_account.owner {
        require_lte!(
            token_amount,
            source_account.amount,
            MarinadeError::NotEnoughUserFunds
        );
    } else {
        return err!(MarinadeError::WrongTokenOwnerOrDelegate)
            .map_err(|e| e.with_pubkeys((source_account.owner, *authority)));
    }
    Ok(())
}
