use anchor_lang::prelude::*;

use super::U32ValueChange;

#[event]
pub struct AddValidatorEvent {
    pub state: Pubkey,
    pub validator: Pubkey,
    pub index: u32,
    pub score: u32,
}

// TODO: EmergencyUnstake
// TODO: PartialUnstake

#[event]
pub struct RemoveValidatorEvent {
    pub state: Pubkey,
    pub validator: Pubkey,
    pub index: u32,
    pub operational_sol_balance: u64,
}

#[event]
pub struct SetValidatorScoreEvent {
    pub state: Pubkey,
    pub validator: Pubkey,
    pub index: u32,
    pub score_change: U32ValueChange,
}
