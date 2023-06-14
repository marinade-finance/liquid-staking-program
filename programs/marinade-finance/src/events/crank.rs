use anchor_lang::prelude::*;

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

// TODO: redelegate

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
