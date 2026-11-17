//! Small helpers over the Trident client: account IO, ids, token / stake / vote account builders.

pub use anchor_lang::solana_program::program_pack::Pack;
pub use trident_fuzz::fuzzing::*;

use anchor_spl::token::spl_token;

pub fn system_id() -> Pubkey {
    anchor_lang::solana_program::system_program::ID
}
pub fn token_id() -> Pubkey {
    anchor_spl::token::ID
}
pub fn stake_id() -> Pubkey {
    anchor_lang::solana_program::stake::program::ID
}
pub fn stake_config_id() -> Pubkey {
    anchor_lang::solana_program::stake::config::ID
}
pub fn vote_id() -> Pubkey {
    solana_vote_interface::program::ID
}
pub fn clock_id() -> Pubkey {
    anchor_lang::solana_program::sysvar::clock::ID
}
pub fn rent_id() -> Pubkey {
    anchor_lang::solana_program::sysvar::rent::ID
}
pub fn epoch_schedule_id() -> Pubkey {
    anchor_lang::solana_program::sysvar::epoch_schedule::ID
}
pub fn stake_history_id() -> Pubkey {
    anchor_lang::solana_program::sysvar::stake_history::ID
}
pub fn program_id() -> Pubkey {
    marinade_finance::ID
}

pub const SOL: u64 = 1_000_000_000;

pub fn put(t: &mut Trident, key: &Pubkey, lamports: u64, data: Vec<u8>, owner: &Pubkey) {
    let acc = AccountSharedData::create(lamports, data, *owner, false, u64::MAX);
    t.set_account_custom(key, &acc);
}

pub fn lam(t: &mut Trident, key: &Pubkey) -> u64 {
    t.get_account(key).lamports()
}

pub fn raw(t: &mut Trident, key: &Pubkey) -> Vec<u8> {
    t.get_account(key).data().to_vec()
}

pub fn owner_of(t: &mut Trident, key: &Pubkey) -> Pubkey {
    *t.get_account(key).owner()
}

pub fn rent_min(t: &Trident, len: usize) -> u64 {
    t.get_sysvar::<solana_sdk::rent::Rent>().minimum_balance(len)
}

pub fn put_mint(t: &mut Trident, key: &Pubkey, authority: &Pubkey, supply: u64) {
    let mut d = vec![0u8; spl_token::state::Mint::LEN];
    spl_token::state::Mint {
        mint_authority: Some(*authority).into(),
        supply,
        decimals: 9,
        is_initialized: true,
        freeze_authority: None.into(),
    }
    .pack_into_slice(&mut d);
    let l = rent_min(t, d.len());
    put(t, key, l, d, &token_id());
}

pub fn put_token_account(t: &mut Trident, key: &Pubkey, mint: &Pubkey, owner: &Pubkey, amount: u64) {
    let mut d = vec![0u8; spl_token::state::Account::LEN];
    spl_token::state::Account {
        mint: *mint,
        owner: *owner,
        amount,
        delegate: None.into(),
        state: spl_token::state::AccountState::Initialized,
        is_native: None.into(),
        delegated_amount: 0,
        close_authority: None.into(),
    }
    .pack_into_slice(&mut d);
    let l = rent_min(t, d.len());
    put(t, key, l, d, &token_id());
}

pub fn read_mint(t: &mut Trident, key: &Pubkey) -> Option<spl_token::state::Mint> {
    let a = t.get_account(key);
    if *a.owner() != token_id() {
        return None;
    }
    spl_token::state::Mint::unpack(a.data()).ok()
}

pub fn read_token(t: &mut Trident, key: &Pubkey) -> Option<spl_token::state::Account> {
    let a = t.get_account(key);
    if *a.owner() != token_id() {
        return None;
    }
    spl_token::state::Account::unpack(a.data()).ok()
}

pub fn token_amount(t: &mut Trident, key: &Pubkey) -> u64 {
    read_token(t, key).map(|a| a.amount).unwrap_or(0)
}

pub fn put_vote(t: &mut Trident, key: &Pubkey) {
    use solana_vote_interface::state::{VoteStateV3, VoteStateVersions};
    let st = VoteStateV3 {
        node_pubkey: t.random_pubkey(),
        authorized_withdrawer: t.random_pubkey(),
        commission: 5,
        ..Default::default()
    };
    let mut data = vec![0u8; VoteStateV3::size_of()];
    bincode::serialize_into(&mut data[..], &VoteStateVersions::new_current(st)).expect("vote state");
    let l = rent_min(t, data.len());
    put(t, key, l, data, &vote_id());
}

use anchor_lang::solana_program::stake::stake_flags::StakeFlags;
use anchor_lang::solana_program::stake::state::{
    Authorized, Delegation, Lockup, Meta, Stake, StakeStateV2,
};

pub fn stake_rent(t: &Trident) -> u64 {
    rent_min(t, StakeStateV2::size_of())
}

/// Delegated stake account injected directly (equivalent to create + initialize + delegate).
#[allow(clippy::too_many_arguments)]
pub fn put_stake(
    t: &mut Trident,
    key: &Pubkey,
    staker: &Pubkey,
    withdrawer: &Pubkey,
    vote: &Pubkey,
    delegated: u64,
    activation_epoch: u64,
    deactivation_epoch: u64,
) {
    let rent = stake_rent(t);
    let st = StakeStateV2::Stake(
        Meta {
            rent_exempt_reserve: rent,
            authorized: Authorized { staker: *staker, withdrawer: *withdrawer },
            lockup: Lockup::default(),
        },
        Stake {
            delegation: Delegation {
                voter_pubkey: *vote,
                stake: delegated,
                activation_epoch,
                deactivation_epoch,
                ..Default::default()
            },
            credits_observed: 0,
        },
        StakeFlags::empty(),
    );
    let mut d = vec![0u8; StakeStateV2::size_of()];
    bincode::serialize_into(&mut d[..], &st).unwrap();
    put(t, key, rent + delegated, d, &stake_id());
}

pub fn put_stake_initialized(t: &mut Trident, key: &Pubkey, staker: &Pubkey, withdrawer: &Pubkey, extra: u64) {
    let rent = stake_rent(t);
    let st = StakeStateV2::Initialized(Meta {
        rent_exempt_reserve: rent,
        authorized: Authorized { staker: *staker, withdrawer: *withdrawer },
        lockup: Lockup::default(),
    });
    let mut d = vec![0u8; StakeStateV2::size_of()];
    bincode::serialize_into(&mut d[..], &st).unwrap();
    put(t, key, rent + extra, d, &stake_id());
}

pub fn read_stake(t: &mut Trident, key: &Pubkey) -> Option<StakeStateV2> {
    let a = t.get_account(key);
    if *a.owner() != stake_id() {
        return None;
    }
    bincode::deserialize(a.data()).ok()
}

/// Rewrite the delegation of an existing stake account (rewards: +, slashing: -), keeping lamports in step.
pub fn adjust_stake(t: &mut Trident, key: &Pubkey, delta: i64) -> bool {
    let a = t.get_account(key);
    let Some(StakeStateV2::Stake(meta, mut stake, flags)) = read_stake(t, key) else { return false };
    let new = if delta >= 0 {
        stake.delegation.stake.saturating_add(delta as u64)
    } else {
        stake.delegation.stake.saturating_sub(delta.unsigned_abs())
    };
    let applied = new as i128 - stake.delegation.stake as i128;
    stake.delegation.stake = new;
    let mut d = a.data().to_vec();
    bincode::serialize_into(&mut d[..], &StakeStateV2::Stake(meta, stake, flags)).unwrap();
    let l = (a.lamports() as i128 + applied).max(0) as u64;
    put(t, key, l, d, &stake_id());
    true
}

/// What the stake program does to a delinquent validator's stake without the owner's signature.
pub fn force_deactivate(t: &mut Trident, key: &Pubkey, epoch: u64) -> bool {
    let a = t.get_account(key);
    let Some(StakeStateV2::Stake(meta, mut stake, flags)) = read_stake(t, key) else { return false };
    if stake.delegation.deactivation_epoch != u64::MAX {
        return false;
    }
    stake.delegation.deactivation_epoch = epoch;
    let mut d = a.data().to_vec();
    bincode::serialize_into(&mut d[..], &StakeStateV2::Stake(meta, stake, flags)).unwrap();
    put(t, key, a.lamports(), d, &stake_id());
    true
}

/// give a user stake account a lockup with a custodian (deposit_stake_account must clear it)
pub fn set_lockup(t: &mut Trident, key: &Pubkey, custodian: &Pubkey, epoch: u64) {
    let a = t.get_account(key);
    let mut d = a.data().to_vec();
    let Ok(mut st) = bincode::deserialize::<StakeStateV2>(&d) else { return };
    match &mut st {
        StakeStateV2::Stake(m, _, _) | StakeStateV2::Initialized(m) => {
            m.lockup = Lockup { unix_timestamp: 0, epoch, custodian: *custodian };
        }
        _ => return,
    }
    bincode::serialize_into(&mut d[..], &st).unwrap();
    put(t, key, a.lamports(), d, &stake_id());
}
