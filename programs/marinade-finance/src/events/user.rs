use anchor_lang::prelude::*;

#[event]
pub struct DepositStakeAccountEvent {
    pub state: Pubkey,
    pub stake: Pubkey,
    pub stake_index: u32,
    pub validator: Pubkey,
    pub validator_index: u32,
    pub delegated: u64,
    pub withdrawer: Pubkey,
    pub msol_minted: u64,
    pub new_user_msol_balance: u64,
    pub new_validator_active_balance: u64,
    pub new_total_active_balance: u64,
    // MSOL price used
    pub total_virtual_staked_lamports: u64,
    pub msol_supply: u64,
}

#[event]
pub struct DepositEvent {
    pub state: Pubkey,
    pub sol_owner: Pubkey,
    pub sol_swapped: u64,
    pub msol_swapped: u64,
    pub sol_deposited: u64,
    pub msol_minted: u64,
    pub new_user_sol_balance: u64,
    pub new_user_msol_balance: u64,
    pub new_sol_leg_balance: u64,
    pub new_msol_leg_balance: u64,
    pub new_reserve_balance: u64,
    // MSOL price used
    pub total_virtual_staked_lamports: u64,
    pub msol_supply: u64,
}
