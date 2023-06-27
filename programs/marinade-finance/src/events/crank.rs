use anchor_lang::prelude::*;

use crate::state::Fee;

use super::U64ValueChange;

#[derive(Clone, AnchorDeserialize, AnchorSerialize)]
pub struct SplitStakeAccountInfo {
    pub account: Pubkey,
    pub index: u32,
}

#[event]
pub struct DeactivateStakeEvent {
    pub state: Pubkey,
    pub epoch: u64,
    pub stake_index: u32,
    pub stake_account: Pubkey,
    pub last_update_stake_delegation: u64,
    pub split_stake_account: Option<SplitStakeAccountInfo>, // None if whole stake is deactivating
    pub validator_index: u32,
    pub validator_vote: Pubkey,
    pub unstaked_amount: u64,
    pub total_stake_target: u64,
    pub validator_stake_target: u64,
    pub new_total_active_balance: u64,
    pub new_delayed_unstake_cooling_down: u64,
    pub new_validator_active_balance: u64,
    pub total_unstake_delta: u64,
}

#[event]
pub struct MergeStakesEvent {
    pub state: Pubkey,
    pub epoch: u64,
    pub destination_stake_index: u32,
    pub destination_stake_account: Pubkey,
    pub last_update_destination_stake_delegation: u64,
    pub source_stake_index: u32,
    pub source_stake_account: Pubkey,
    pub last_update_source_stake_delegation: u64,
    pub validator_index: u32,
    pub validator_vote: Pubkey,
    pub extra_delegated: u64,
    pub returned_stake_rent: u64,
    pub new_validator_active_balance: u64,
    pub new_total_active_balance: u64,
    pub new_operational_sol_balance: u64,
}

#[event]
pub struct RedelegateEvent {
    pub state: Pubkey,
    pub epoch: u64,
    pub stake_index: u32,
    pub stake_account: Pubkey,
    pub last_update_delegation: u64,
    pub source_validator_index: u32,
    pub source_validator_vote: Pubkey,
    pub source_validator_score: u32,
    pub source_validator_balance: u64,
    pub source_validator_stake_target: u64,
    pub dest_validator_index: u32,
    pub dest_validator_vote: Pubkey,
    pub dest_validator_score: u32,
    pub dest_validator_balance: u64,
    pub dest_validator_stake_target: u64,
    pub redelegate_amount: u64,
    pub split_stake_account: Option<SplitStakeAccountInfo>, // None if whole stake is being redelegated
    pub redelegate_stake_index: u32,
    pub redelegate_stake_account: Pubkey,
}

#[event]
pub struct StakeReserveEvent {
    pub state: Pubkey,
    pub epoch: u64,
    pub stake_index: u32,
    pub stake_account: Pubkey,
    pub validator_index: u32,
    pub validator_vote: Pubkey,
    pub amount: u64,
    pub total_stake_target: u64,
    pub validator_stake_target: u64,
    pub new_reserve_balance: u64,
    pub new_total_active_balance: u64,
    pub new_validator_active_balance: u64,
    pub total_stake_delta: u64,
}

#[event]
pub struct UpdateActiveEvent {
    pub state: Pubkey,
    pub epoch: u64,
    pub stake_index: u32,
    pub stake_account: Pubkey,
    pub validator_index: u32,
    pub validator_vote: Pubkey,
    pub delegation_change: U64ValueChange,
    pub delegation_growth_msol_fees: Option<u64>,
    pub extra_lamports: u64,
    pub extra_msol_fees: Option<u64>,
    pub new_validator_active_balance: u64,
    pub new_total_active_balance: u64,
    pub msol_price_change: U64ValueChange,
    pub reward_fee_used: Fee,
    // MSOL price used
    pub total_virtual_staked_lamports: u64,
    pub msol_supply: u64,
}

#[event]
pub struct UpdateDeactivatedEvent {
    pub state: Pubkey,
    pub epoch: u64,
    pub stake_index: u32,
    pub stake_account: Pubkey,
    pub balance_without_rent_exempt: u64,
    pub last_update_delegated_lamports: u64,
    pub msol_fees: Option<u64>,
    pub msol_price_change: U64ValueChange,
    pub reward_fee_used: Fee,
    pub new_operational_sol_balance: u64,
    // MSOL price used
    pub total_virtual_staked_lamports: u64,
    pub msol_supply: u64,
}