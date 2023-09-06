use crate::error::MarinadeError;
use crate::ID;
use anchor_lang::{prelude::*, Discriminator};
use anchor_lang::solana_program::clock::Epoch;

use super::list::List;

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct StakeRecord {
    pub stake_account: Pubkey,
    pub last_update_delegated_lamports: u64,
    pub last_update_epoch: u64,
    pub is_emergency_unstaking: u8, // 1 for cooling down after emergency unstake, 0 otherwise
}

impl StakeRecord {
    pub fn new(
        stake_account: &Pubkey,
        delegated_lamports: u64,
        clock: &Clock,
        is_emergency_unstaking: u8,
    ) -> Self {
        Self {
            stake_account: *stake_account,
            last_update_delegated_lamports: delegated_lamports,
            last_update_epoch: clock.epoch,
            is_emergency_unstaking,
        }
    }
}


#[derive(Debug, Clone, AnchorSerialize, AnchorDeserialize)]
pub struct StakeList {}

impl Discriminator for StakeList {
    const DISCRIMINATOR: [u8; 8] = *b"staker__";
}

impl AccountDeserialize for StakeList {
    fn try_deserialize_unchecked(buf: &mut &[u8]) -> Result<Self> {
        *buf = &buf[8..];
        Ok(Self {})
    }

    fn try_deserialize(buf: &mut &[u8]) -> Result<Self> {
        if buf.len() < 8 {
            return err!(MarinadeError::InvalidStakeListDiscriminator);
        }
        if buf[0..8] != Self::DISCRIMINATOR {
            return err!(MarinadeError::InvalidStakeListDiscriminator);
        }
        *buf = &buf[8..];
        Ok(Self {})
    }
}

// By implementing an AccountSerialize, Anchor will not try
// to write the account data at the end of the instruction
// because the default implementation of the try_serialize is empty
// The account data is written explicitly in the code
impl AccountSerialize for StakeList {}

impl Owner for StakeList {
    fn owner() -> Pubkey {
        crate::ID
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
    pub const MIN_UPDATE_WINDOW: u64 = 3_000; //min value is 3_000 => half an hour

    pub fn bytes_for_list(count: u32, additional_record_space: u32) -> u32 {
        List::bytes_for(
            StakeRecord::default().try_to_vec().unwrap().len() as u32 + additional_record_space,
            count,
        )
    }

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
    ) -> Result<Self> {
        let stake_list = List::new(
            &StakeList::DISCRIMINATOR,
            StakeRecord::default().try_to_vec().unwrap().len() as u32 + additional_record_space,
            stake_list_account,
            stake_list_data,
        )
        .map_err(|e| e.with_account_name("stake_list"))?;

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

    pub fn stake_list_capacity(&self, stake_list_len: usize) -> Result<u32> {
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
        is_emergency_unstaking: u8,
    ) -> Result<()> {
        self.stake_list
            .push(
                stake_list_data,
                StakeRecord::new(
                    stake_account,
                    delegated_lamports,
                    clock,
                    is_emergency_unstaking,
                ),
            )
            .map_err(|e| e.with_account_name("stake_list"))?;
        Ok(())
    }

    fn get(&self, stake_list_data: &[u8], index: u32) -> Result<StakeRecord> {
        self.stake_list
            .get(stake_list_data, index)
            .map_err(|e| e.with_account_name("stake_list"))
    }

    /// get the stake account record from an index, and check that the account is the same passed as parameter to the instruction
    pub fn get_checked(
        &self,
        stake_list_data: &[u8],
        index: u32,
        received_pubkey: &Pubkey,
    ) -> Result<StakeRecord> {
        let stake_record = self.get(stake_list_data, index)?;
        require_keys_eq!(
            stake_record.stake_account,
            *received_pubkey,
            MarinadeError::WrongStakeAccountOrIndex
        );
        Ok(stake_record)
    }

    pub fn set(&self, stake_list_data: &mut [u8], index: u32, stake: StakeRecord) -> Result<()> {
        self.stake_list
            .set(stake_list_data, index, stake)
            .map_err(|e| e.with_account_name("stake_list"))
    }
    pub fn remove(&mut self, stake_list_data: &mut [u8], index: u32) -> Result<()> {
        self.stake_list
            .remove(stake_list_data, index)
            .map_err(|e| e.with_account_name("stake_list"))
    }
}
