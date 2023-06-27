use anchor_lang::prelude::*;

#[event]
pub struct ClaimEvent {
    pub state: Pubkey,
    pub epoch: u64,
    pub ticket: Pubkey,
    pub beneficiary: Pubkey,
    pub amount: u64,
    pub new_circulating_ticket_balance: u64,
    pub new_circulating_ticket_count: u64,
    pub new_reserve_balance: u64,
    pub new_user_balance: u64,
}

#[event]
pub struct OrderUnstakeEvent {
    pub state: Pubkey,
    pub ticket_epoch: u64,
    pub ticket: Pubkey,
    pub beneficiary: Pubkey,
    pub msol_amount: u64,
    pub sol_amount: u64,
    pub new_circulating_ticket_balance: u64,
    pub new_circulating_ticket_count: u64,
    pub new_user_msol_balance: u64,
    // MSOL price used
    pub total_virtual_staked_lamports: u64,
    pub msol_supply: u64,
}