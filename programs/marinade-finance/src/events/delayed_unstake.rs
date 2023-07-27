use anchor_lang::prelude::*;

#[event]
pub struct ClaimEvent {
    pub state: Pubkey,
    pub epoch: u64,
    pub ticket: Pubkey,
    pub beneficiary: Pubkey,
    pub circulating_ticket_balance: u64,
    pub circulating_ticket_count: u64,
    pub reserve_balance: u64,
    pub user_balance: u64,
    pub amount: u64,
}

#[event]
pub struct OrderUnstakeEvent {
    pub state: Pubkey,
    pub ticket_epoch: u64,
    pub ticket: Pubkey,
    pub beneficiary: Pubkey,
    pub circulating_ticket_balance: u64,
    pub circulating_ticket_count: u64,
    pub user_msol_balance: u64,
    pub burned_msol_amount: u64,
    pub sol_amount: u64,
    pub fee_bp_cents: u32,
    // MSOL price used
    pub total_virtual_staked_lamports: u64,
    pub msol_supply: u64,
}
