//use std::convert::TryInto;

use crate::{calc::proportional, checks::check_address, error::CommonError, list::List, ID};
use anchor_lang::prelude::*;

pub mod add;
pub mod config_validator_system;
pub mod remove;
pub mod set_score;

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct ValidatorRecord {
    /// Validator vote pubkey
    pub validator_account: Pubkey,

    /// Validator total balance in lamports
    pub active_balance: u64, // must be 0 for removing
    pub score: u32,
    pub last_stake_delta_epoch: u64,
    pub duplication_flag_bump_seed: u8,
}

impl ValidatorRecord {
    pub const DISCRIMINATOR: &'static [u8; 8] = b"validatr";
    pub const DUPLICATE_FLAG_SEED: &'static [u8] = b"unique_validator";

    pub fn find_duplication_flag(state: &Pubkey, validator_account: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[
                &state.to_bytes()[..32],
                Self::DUPLICATE_FLAG_SEED,
                &validator_account.to_bytes()[..32],
            ],
            &ID,
        )
    }

    pub fn with_duplication_flag_seeds<R, F: FnOnce(&[&[u8]]) -> R>(
        &self,
        state: &Pubkey,
        f: F,
    ) -> R {
        f(&[
            &state.to_bytes()[..32],
            Self::DUPLICATE_FLAG_SEED,
            &self.validator_account.to_bytes()[..32],
            &[self.duplication_flag_bump_seed],
        ])
    }

    pub fn duplication_flag_address(&self, state: &Pubkey) -> Pubkey {
        self.with_duplication_flag_seeds(state, |seeds| Pubkey::create_program_address(seeds, &ID))
            .unwrap()
    }

    pub fn new(
        validator_account: Pubkey,
        score: u32,
        state: &Pubkey,
        duplication_flag_address: &Pubkey,
    ) -> Result<Self, ProgramError> {
        let (actual_duplication_flag, duplication_flag_bump_seed) =
            Self::find_duplication_flag(state, &validator_account);
        if duplication_flag_address != &actual_duplication_flag {
            msg!(
                "Duplication flag {} does not match {}",
                duplication_flag_address,
                actual_duplication_flag
            );
            return Err(ProgramError::InvalidArgument);
        }
        Ok(Self {
            validator_account,
            active_balance: 0,
            score,
            last_stake_delta_epoch: std::u64::MAX, // never
            duplication_flag_bump_seed,
        })
    }
}

#[derive(Clone, AnchorSerialize, AnchorDeserialize, Debug)]
pub struct ValidatorSystem {
    pub validator_list: List,
    pub manager_authority: Pubkey,
    pub total_validator_score: u32,
    /// sum of all active lamports staked
    pub total_active_balance: u64,
    /// allow & auto-add validator when a user deposits a stake-account of a non-listed validator
    pub auto_add_validator_enabled: u8,
}

impl ValidatorSystem {
    pub fn bytes_for_list(count: u32, additional_record_space: u32) -> u32 {
        List::bytes_for(
            ValidatorRecord::default().try_to_vec().unwrap().len() as u32 + additional_record_space,
            count,
        )
    }

    /*
    pub fn list_capacity(account_len: usize) -> u32 {
        List::<ValidatorRecordDiscriminator, ValidatorRecord, u32>::capacity_of(
            ValidatorRecord::default().try_to_vec().unwrap().len() as u32,
            account_len,
        )
    }*/

    pub fn new(
        validator_list_account: Pubkey,
        validator_list_data: &mut [u8],
        manager_authority: Pubkey,
        additional_record_space: u32,
    ) -> Result<Self, ProgramError> {
        Ok(Self {
            validator_list: List::new(
                ValidatorRecord::DISCRIMINATOR,
                ValidatorRecord::default().try_to_vec().unwrap().len() as u32
                    + additional_record_space,
                validator_list_account,
                validator_list_data,
                "validator_list",
            )?,
            manager_authority,
            total_validator_score: 0,
            total_active_balance: 0,
            auto_add_validator_enabled: 0,
        })
    }

    pub fn validator_list_address(&self) -> &Pubkey {
        &self.validator_list.account
    }

    pub fn validator_count(&self) -> u32 {
        self.validator_list.len()
    }

    pub fn validator_list_capacity(&self, validator_list_len: usize) -> Result<u32, ProgramError> {
        self.validator_list.capacity(validator_list_len)
    }

    /*
    pub fn validator_records<'a: 'info, 'info>(
        &'a self,
        validator_list: &'a AccountInfo<'info>,
    ) -> impl Iterator<Item = Result<ValidatorRecord, ProgramError>> + 'a {
        self.validator_list.iter(validator_list, "validator_list")
    }*/

    pub fn validator_record_size(&self) -> u32 {
        self.validator_list.item_size()
    }

    pub fn add(
        &mut self,
        validator_list_data: &mut [u8],
        validator_account: Pubkey,
        score: u32,
        state: &Pubkey,
        duplication_flag_address: &Pubkey,
    ) -> ProgramResult {
        self.validator_list.push(
            validator_list_data,
            ValidatorRecord::new(validator_account, score, state, duplication_flag_address)?,
            "validator_list",
        )?;
        self.total_validator_score += score as u32;
        Ok(())
    }

    pub fn add_with_balance(
        &mut self,
        validator_list_data: &mut [u8],
        validator_account: Pubkey,
        score: u32,
        balance: u64,
        state: &Pubkey,
        duplication_flag_address: &Pubkey,
    ) -> ProgramResult {
        let mut validator =
            ValidatorRecord::new(validator_account, score, state, duplication_flag_address)?;
        validator.active_balance = balance;
        self.validator_list
            .push(validator_list_data, validator, "validator_list")?;
        self.total_validator_score += score as u32;
        Ok(())
    }

    pub fn remove(
        &mut self,
        validator_list_data: &mut [u8],
        index: u32,
        record: ValidatorRecord,
    ) -> ProgramResult {
        if record.active_balance > 0 {
            msg!(
                "Can not remove validator {} with balance {}",
                record.validator_account,
                record.active_balance
            );
            return Err(ProgramError::InvalidInstructionData);
        }
        self.total_validator_score = self.total_validator_score.saturating_sub(record.score);

        self.validator_list
            .remove(validator_list_data, index, "validator_list")?;

        Ok(())
    }

    pub fn get(
        &self,
        validator_list_data: &[u8],
        index: u32,
    ) -> Result<ValidatorRecord, ProgramError> {
        self.validator_list
            .get(validator_list_data, index, "validator_list")
    }

    // Do not forget to update totals
    pub fn set(
        &self,
        validator_list_data: &mut [u8],
        index: u32,
        validator_record: ValidatorRecord,
    ) -> ProgramResult {
        self.validator_list.set(
            validator_list_data,
            index,
            validator_record,
            "validator_list",
        )
    }

    pub fn validator_stake_target(
        &self,
        validator: &ValidatorRecord,
        total_stake_target: u64,
    ) -> Result<u64, CommonError> {
        if self.total_validator_score == 0 {
            return Ok(0);
        }
        proportional(
            total_stake_target,
            validator.score as u64,
            self.total_validator_score as u64,
        )
    }

    pub fn check_validator_list<'info>(
        &self,
        validator_list: &AccountInfo<'info>,
    ) -> ProgramResult {
        check_address(
            validator_list.key,
            self.validator_list_address(),
            "validator_list",
        )?;
        if &validator_list.data.borrow().as_ref()[0..8] != ValidatorRecord::DISCRIMINATOR {
            msg!("Wrong validator list account discriminator");
            return Err(ProgramError::InvalidAccountData);
        }
        Ok(())
    }

    pub fn check_validator_manager_authority(&self, manager_authority: &Pubkey) -> ProgramResult {
        check_address(
            manager_authority,
            &self.manager_authority,
            "validator_manager_authority",
        )
    }
}
