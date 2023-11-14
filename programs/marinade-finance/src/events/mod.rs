use anchor_lang::prelude::*;

use crate::state::{fee::FeeCents, Fee};

pub mod admin;
pub mod crank;
pub mod delayed_unstake;
pub mod liq_pool;
pub mod management;
pub mod user;

#[derive(Clone, AnchorSerialize, AnchorDeserialize)]
pub struct U64ValueChange {
    pub old: u64,
    pub new: u64,
}

#[derive(Clone, AnchorSerialize, AnchorDeserialize)]
pub struct U32ValueChange {
    pub old: u32,
    pub new: u32,
}

#[derive(Clone, AnchorSerialize, AnchorDeserialize)]
pub struct FeeValueChange {
    pub old: Fee,
    pub new: Fee,
}

#[derive(Clone, AnchorSerialize, AnchorDeserialize)]
pub struct FeeCentsValueChange {
    pub old: FeeCents,
    pub new: FeeCents,
}

#[derive(Clone, AnchorSerialize, AnchorDeserialize)]
pub struct PubkeyValueChange {
    pub old: Pubkey,
    pub new: Pubkey,
}

#[derive(Clone, AnchorSerialize, AnchorDeserialize)]
pub struct BoolValueChange {
    pub old: bool,
    pub new: bool,
}
