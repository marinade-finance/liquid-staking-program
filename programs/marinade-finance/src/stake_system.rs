use crate::{checks::check_address, list::List, located::Located, State, ID};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::clock::Epoch;

pub mod deactivate_stake;
pub mod deposit_stake_account;
pub mod emergency_unstake;
pub mod merge;
pub mod stake_reserve;

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct StakeRecord {
    pub stake_account: Pubkey,
    pub last_update_delegated_lamports: u64,
    pub last_update_epoch: u64,
    pub is_emergency_unstaking: u8, // 1 for cooling down after emergency unstake, 0 otherwise
}

impl StakeRecord {
    pub const DISCRIMINATOR: &'static [u8; 8] = b"staker__";

    pub fn new(stake_account: &Pubkey, delegated_lamports: u64, clock: &Clock) -> Self {
        Self {
            stake_account: *stake_account,
            last_update_delegated_lamports: delegated_lamports,
            last_update_epoch: clock.epoch,
            is_emergency_unstaking: 0,
        }
    }
}

#[derive(Clone, AnchorSerialize, AnchorDeserialize, Debug)]
pub struct StakeSystem {
    pub stake_list: List,
    //pub last_update_epoch: u64,
    //pub updated_during_last_epoch: u32,
    pub delayed_unstake_cooling_down: u64,
    pub stake_deposit_bump_seed: u8,
    pub stake_withdraw_bump_seed: u8,

    /// set by admin, how much slots before the end of the epoch, stake-delta can start
    pub slots_for_stake_delta: u64,
    /// Marks the start of stake-delta operations, meaning that if somebody starts a delayed-unstake ticket
    /// after this var is set with epoch_num the ticket will have epoch_created = current_epoch+1
    /// (the user must wait one more epoch, because their unstake-delta will be execute in this epoch)
    pub last_stake_delta_epoch: u64,
    pub min_stake: u64, // Minimal stake account delegation
    /// can be set by validator-manager-auth to allow a second run of stake-delta to stake late stakers in the last minute of the epoch
    /// so we maximize user's rewards
    pub extra_stake_delta_runs: u32,
}

impl StakeSystem {
    pub const STAKE_WITHDRAW_SEED: &'static [u8] = b"withdraw";
    pub const STAKE_DEPOSIT_SEED: &'static [u8] = b"deposit";

    pub fn bytes_for_list(count: u32, additional_record_space: u32) -> u32 {
        List::bytes_for(
            StakeRecord::default().try_to_vec().unwrap().len() as u32 + additional_record_space,
            count,
        )
    }

    /*
    pub fn list_capacity(account_len: usize) -> u32 {
        List::<StakeDiscriminator, StakeRecord, u32>::capacity_of(
            StakeRecord::default().try_to_vec().unwrap().len() as u32,
            account_len,
        )
    }*/

    pub fn find_stake_withdraw_authority(state: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[&state.to_bytes()[..32], Self::STAKE_WITHDRAW_SEED], &ID)
    }

    pub fn find_stake_deposit_authority(state: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[&state.to_bytes()[..32], Self::STAKE_DEPOSIT_SEED], &ID)
    }

    pub fn new(
        state: &Pubkey,
        stake_list_account: Pubkey,
        stake_list_data: &mut [u8],
        slots_for_stake_delta: u64,
        min_stake: u64,
        extra_stake_delta_runs: u32,
        additional_record_space: u32,
    ) -> Result<Self, ProgramError> {
        let stake_list = List::new(
            StakeRecord::DISCRIMINATOR,
            StakeRecord::default().try_to_vec().unwrap().len() as u32 + additional_record_space,
            stake_list_account,
            stake_list_data,
            "stake_list",
        )?;

        Ok(Self {
            stake_list,
            delayed_unstake_cooling_down: 0,
            stake_deposit_bump_seed: Self::find_stake_deposit_authority(state).1,
            stake_withdraw_bump_seed: Self::find_stake_withdraw_authority(state).1,
            slots_for_stake_delta,
            last_stake_delta_epoch: Epoch::MAX, // never
            min_stake,
            extra_stake_delta_runs,
        })
    }

    pub fn stake_list_address(&self) -> &Pubkey {
        &self.stake_list.account
    }

    pub fn stake_count(&self) -> u32 {
        self.stake_list.len()
    }

    pub fn stake_list_capacity(&self, stake_list_len: usize) -> Result<u32, ProgramError> {
        self.stake_list.capacity(stake_list_len)
    }

    pub fn stake_record_size(&self) -> u32 {
        self.stake_list.item_size()
    }

    pub fn add(
        &mut self,
        stake_list_data: &mut [u8],
        stake_account: &Pubkey,
        delegated_lamports: u64,
        clock: &Clock,
    ) -> ProgramResult {
        self.stake_list.push(
            stake_list_data,
            StakeRecord::new(stake_account, delegated_lamports, clock),
            "stake_list",
        )?;
        Ok(())
    }

    pub fn get(&self, stake_list_data: &[u8], index: u32) -> Result<StakeRecord, ProgramError> {
        self.stake_list.get(stake_list_data, index, "stake_list")
    }

    pub fn set(&self, stake_list_data: &mut [u8], index: u32, stake: StakeRecord) -> ProgramResult {
        self.stake_list
            .set(stake_list_data, index, stake, "stake_list")
    }
    pub fn remove(&mut self, stake_list_data: &mut [u8], index: u32) -> ProgramResult {
        self.stake_list.remove(stake_list_data, index, "stake_list")
    }

    pub fn check_stake_list<'info>(&self, stake_list: &AccountInfo<'info>) -> ProgramResult {
        check_address(stake_list.key, self.stake_list_address(), "stake_list")?;
        if &stake_list.data.borrow().as_ref()[0..8] != StakeRecord::DISCRIMINATOR {
            msg!("Wrong stake list account discriminator");
            return Err(ProgramError::InvalidAccountData);
        }
        Ok(())
    }
}

pub trait StakeSystemHelpers {
    fn stake_withdraw_authority(&self) -> Pubkey;
    fn with_stake_withdraw_authority_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R;
    fn check_stake_withdraw_authority(&self, stake_withdraw_authority: &Pubkey) -> ProgramResult;

    fn stake_deposit_authority(&self) -> Pubkey;
    fn with_stake_deposit_authority_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R;
    fn check_stake_deposit_authority(&self, stake_deposit_authority: &Pubkey) -> ProgramResult;
}

impl<T> StakeSystemHelpers for T
where
    T: Located<State>,
{
    fn stake_withdraw_authority(&self) -> Pubkey {
        self.with_stake_withdraw_authority_seeds(|seeds| {
            Pubkey::create_program_address(seeds, &ID).unwrap()
        })
    }

    fn with_stake_withdraw_authority_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R {
        f(&[
            &self.key().to_bytes()[..32],
            StakeSystem::STAKE_WITHDRAW_SEED,
            &[self.as_ref().stake_system.stake_withdraw_bump_seed],
        ])
    }

    fn check_stake_withdraw_authority(&self, stake_withdraw_authority: &Pubkey) -> ProgramResult {
        check_address(
            stake_withdraw_authority,
            &self.stake_withdraw_authority(),
            "stake_withdraw_authority",
        )
    }

    fn stake_deposit_authority(&self) -> Pubkey {
        self.with_stake_deposit_authority_seeds(|seeds| {
            Pubkey::create_program_address(seeds, &ID).unwrap()
        })
    }

    fn with_stake_deposit_authority_seeds<R, F: FnOnce(&[&[u8]]) -> R>(&self, f: F) -> R {
        f(&[
            &self.key().to_bytes()[..32],
            StakeSystem::STAKE_DEPOSIT_SEED,
            &[self.as_ref().stake_system.stake_deposit_bump_seed],
        ])
    }

    fn check_stake_deposit_authority(&self, stake_deposit_authority: &Pubkey) -> ProgramResult {
        check_address(
            stake_deposit_authority,
            &self.stake_deposit_authority(),
            "stake_deposit_authority",
        )
    }
}
