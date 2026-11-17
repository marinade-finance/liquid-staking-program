//! Operational mode: replicate marcrank's `top-up-stake-rent` (see `marcrank/src/commands/top_up_stake_rent.rs`).
//!
//! The Marinade binary that predates the rent fix (`d4e2023`) requires `lamports == delegation.stake +
//! meta.rent_exempt_reserve` for every stake account it touches (merge_stakes, create_canonical_stake, ...) and
//! mis-accounts update_deactivated. Under a reduced Rent sysvar, stake accounts created by `stake_reserve` hold
//! `delegation + live rent` while the stake program writes the frozen `meta.rent_exempt_reserve` (2_282_880).
//! On mainnet the crank bot closes that gap by transferring the difference into every Marinade stake account
//! before doing the epoch work. `HARNESS_RENT_TOPUP=1` makes the harness behave the same way:
//!
//! * at the start of `Crank::start_of_epoch` / `Crank::do_work`, like marcrank's do-work;
//! * immediately before every direct crank op wrapper on [`Marinade`] that reads stake balances.
//!
//! The lamports come from a dedicated crank payer and are booked in the env ledger as injected (the payer is
//! funded with `env.fund`), so the conservation invariants stay exact.
//!
//! [`known_live_limitations`] lists what the bot cannot fix on the live binary. It is consulted only when
//! `HARNESS_RENT_TOPUP=1` and the binary under test is the Marinade binary of the mainnet profile.

#![allow(deprecated)]

use crate::env::{sol, TestEnv};
use crate::marinade::Marinade;
use anyhow::{anyhow, Result};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_stake_interface::state::StakeStateV2;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// the stake program's PSEUDO_RENT_EXEMPT_RESERVE, the only reserve a genuine account can carry
pub const STAKE_FROZEN_RENT_EXEMPT_RESERVE: u64 = 2_282_880;
/// marcrank: MAX_TOP_UP_ACCOUNTS_PER_RUN
pub const MAX_TOP_UP_ACCOUNTS_PER_RUN: usize = 512;

pub const ENV_VAR: &str = "HARNESS_RENT_TOPUP";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// `HARNESS_RENT_TOPUP=1`: the bot runs next to the binary that is live on mainnet (the Marinade binary of
    /// `profiles/mainnet.json`). Any other binary (HEAD, fixed builds) is left alone: it does not need the
    /// top-up and is actively hurt by it (see ARCHITECTURE.md, "Operational mode").
    Live,
    /// `HARNESS_RENT_TOPUP=force`: the bot runs next to any binary (experiments, e.g. "does HEAD tolerate the bot?").
    Force,
}

/// `HARNESS_RENT_TOPUP` (cached for the process).
pub fn mode() -> Mode {
    static M: OnceLock<Mode> = OnceLock::new();
    *M.get_or_init(|| match std::env::var(ENV_VAR).as_deref() {
        Ok("1") => Mode::Live,
        Ok("force") => Mode::Force,
        _ => Mode::Off,
    })
}

/// Is the operational top-up applied for the binary in `env`?
pub fn active(env: &TestEnv) -> bool {
    match mode() {
        Mode::Off => false,
        Mode::Force => true,
        Mode::Live => is_live_binary(&env.marinade_so.sha256),
    }
}

/// Is `sha256` the Marinade binary of the mainnet profile?
pub fn is_live_binary(sha256: &str) -> bool {
    mainnet_profile_marinade_sha().as_deref() == Some(sha256)
}

/// Process-wide counters (for reporting): accounts topped up, lamports transferred, accounts "needing triage".
pub static TOPPED_UP: AtomicU64 = AtomicU64::new(0);
pub static TOPPED_UP_LAMPORTS: AtomicU64 = AtomicU64::new(0);
pub static NEEDS_TRIAGE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
pub enum TopUp {
    NotNeeded,
    Needed(u64),
    Unexpected { deficit: u64, gap: u64 },
}

/// marcrank's `top_up`, verbatim. The crank instructions require lamports == delegation.stake + the frozen
/// meta.rent_exempt_reserve, which funding at the live Rent sysvar no longer satisfies.
pub fn top_up(stake: &StakeStateV2, balance: u64, live_rent: u64) -> TopUp {
    let (Some(delegation), Some(meta)) = (stake.delegation(), stake.meta()) else {
        return TopUp::NotNeeded;
    };
    let needed = (delegation.stake + meta.rent_exempt_reserve).saturating_sub(balance);
    if needed == 0 {
        return TopUp::NotNeeded;
    }
    // an account that also holds extra lamports is short by less than the full gap, so the gap bounds the top-up rather than defining it
    let gap = meta.rent_exempt_reserve.min(STAKE_FROZEN_RENT_EXEMPT_RESERVE).saturating_sub(live_rent);
    if needed > gap {
        return TopUp::Unexpected { deficit: needed, gap };
    }
    TopUp::Needed(needed)
}

impl Marinade {
    /// marcrank `top-up-stake-rent`: for every Marinade stake account that is short, transfer
    /// `delegation.stake + meta.rent_exempt_reserve - lamports` from `payer` (bounded by the rent gap; accounts
    /// short by more are skipped, "needs triage"; at most 512 per run). Returns the number of accounts topped up.
    /// The payer is funded with `env.fund` as needed, which books the lamports as injected.
    pub fn top_up_stake_rent(&self, env: &mut TestEnv, payer: &Keypair) -> Result<u32> {
        let live_rent = env.stake_rent();
        let mut planned: Vec<(Address, u64)> = vec![];
        for rec in self.stake_records(env) {
            let a = crate::convert::to_addr(&rec.stake_account);
            let Some(acc) = env.get_account(&a) else { continue };
            let Ok(state) = bincode::deserialize::<StakeStateV2>(&acc.data) else { continue };
            match top_up(&state, acc.lamports, live_rent) {
                TopUp::NotNeeded => {}
                TopUp::Unexpected { deficit, gap } => {
                    NEEDS_TRIAGE.fetch_add(1, Ordering::Relaxed);
                    if std::env::var("HARNESS_TOPUP_VERBOSE").is_ok() {
                        eprintln!("[topup] stake {a} is short of {deficit} but the rent-reduction gap is {gap}: not topping up, needs triage");
                    }
                }
                TopUp::Needed(n) => planned.push((a, n)),
            }
        }
        planned.truncate(MAX_TOP_UP_ACCOUNTS_PER_RUN);
        if planned.is_empty() {
            return Ok(0);
        }
        let total: u64 = planned.iter().map(|(_, n)| n).sum();
        // keep the (system) payer rent-exempt after the transfers; the funding is injected into the ledger
        let want = total + sol(1);
        let have = env.lamports(&payer.pubkey());
        if have < want {
            env.fund(&payer.pubkey(), want - have);
        }
        let mut done = 0u32;
        for (stake, needed) in planned {
            let ix = solana_system_interface::instruction::transfer(&payer.pubkey(), &stake, needed);
            env.send(&[ix], &[payer]).map_err(|e| anyhow!("top-up transfer of {needed} to {stake} failed: {e}"))?;
            TOPPED_UP.fetch_add(1, Ordering::Relaxed);
            TOPPED_UP_LAMPORTS.fetch_add(needed, Ordering::Relaxed);
            done += 1;
        }
        Ok(done)
    }

    /// Run the top-up with this instance's crank payer when the operational mode is active for this binary; no-op otherwise.
    pub fn operational_top_up(&self, env: &mut TestEnv) {
        if active(env) {
            self.top_up_stake_rent(env, &self.rent_payer).expect("operational top-up (HARNESS_RENT_TOPUP=1)");
        }
    }
}

// ------------------------------------------------------------------------------------------ known limitations

/// A failure that the crank bot cannot prevent on the live binary.
#[derive(Debug)]
pub struct Limitation {
    pub id: &'static str,
    /// the failing op's name contains this
    pub op: &'static str,
    /// the failure text contains this
    pub error: &'static str,
    pub why: &'static str,
}

/// Expected failures of the binary live on mainnet when the crank bot's top-up is applied.
pub fn known_live_limitations() -> &'static [Limitation] {
    &[Limitation {
        id: "deposit_stake_account-user-stake",
        op: "deposit_stake_account",
        // MarinadeError::WrongStakeBalance (6048)
        error: "WrongStakeBalance",
        why: "user stake accounts created under today's Rent hold delegation + live rent, but the old program requires \
              lamports == delegation + meta.rent_exempt_reserve (2_282_880); the bot only tops up Marinade's own stake \
              accounts, never a user's account (a user can only deposit after sending the 616_640 gap lamports to it themselves)",
    }]
}

/// sha256 of the Marinade binary of the mainnet profile (the base profile, `HARNESS_BASE`), if it has one.
pub fn mainnet_profile_marinade_sha() -> Option<String> {
    crate::profile::live_binary().ok().map(|b| b.sha256)
}

/// True when limitations apply: `HARNESS_RENT_TOPUP` is set AND the binary is the mainnet profile's Marinade binary.
pub fn limitations_apply(binary_sha256: &str) -> bool {
    mode() != Mode::Off && is_live_binary(binary_sha256)
}

/// If `op` failing with `error` is a known live limitation, print a KNOWN-LIVE-LIMITATION line and return it.
pub fn waive(binary_sha256: &str, test: &str, op: &str, error: &str) -> Option<&'static Limitation> {
    if !limitations_apply(binary_sha256) {
        return None;
    }
    let l = known_live_limitations().iter().find(|l| op.contains(l.op) && error.contains(l.error))?;
    // one line per (test, limitation): the fuzzer hits the same limitation many times
    static SEEN: std::sync::Mutex<Vec<(String, &'static str)>> = std::sync::Mutex::new(Vec::new());
    let first = {
        let mut seen = SEEN.lock().unwrap();
        let key = (test.to_string(), l.id);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    };
    if first {
        eprintln!("KNOWN-LIVE-LIMITATION [{}] {test}: {op} failed as expected on the live binary: {}", l.id, l.why);
    }
    Some(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_stake_interface::stake_flags::StakeFlags;
    use solana_stake_interface::state::{Delegation, Meta, Stake};

    const FROZEN_RENT: u64 = STAKE_FROZEN_RENT_EXEMPT_RESERVE;
    const LIVE_RENT: u64 = 2_077_224;
    const DELEGATED: u64 = 10_000_000_000;

    fn delegated_stake(rent_exempt_reserve: u64) -> StakeStateV2 {
        StakeStateV2::Stake(
            Meta { rent_exempt_reserve, ..Meta::default() },
            Stake { delegation: Delegation { stake: DELEGATED, ..Delegation::default() }, credits_observed: 0 },
            StakeFlags::empty(),
        )
    }

    #[test]
    fn account_created_after_rent_reduction_needs_the_gap() {
        assert_eq!(top_up(&delegated_stake(FROZEN_RENT), DELEGATED + LIVE_RENT, LIVE_RENT), TopUp::Needed(205_656));
    }

    #[test]
    fn account_created_before_rent_reduction_is_left_alone() {
        assert_eq!(top_up(&delegated_stake(FROZEN_RENT), DELEGATED + FROZEN_RENT, LIVE_RENT), TopUp::NotNeeded);
    }

    #[test]
    fn mev_surplus_is_left_alone() {
        assert_eq!(top_up(&delegated_stake(FROZEN_RENT), DELEGATED + FROZEN_RENT + 8_444, LIVE_RENT), TopUp::NotNeeded);
    }

    #[test]
    fn partial_surplus_still_gets_topped_up() {
        assert_eq!(top_up(&delegated_stake(FROZEN_RENT), DELEGATED + LIVE_RENT + 1, LIVE_RENT), TopUp::Needed(205_655));
    }

    #[test]
    fn deficit_within_the_rent_gap_is_funded() {
        assert_eq!(top_up(&delegated_stake(FROZEN_RENT), DELEGATED + FROZEN_RENT - 1_000, LIVE_RENT), TopUp::Needed(1_000));
    }

    #[test]
    fn deficit_larger_than_the_rent_gap_is_not_funded() {
        assert_eq!(
            top_up(&delegated_stake(FROZEN_RENT), DELEGATED + LIVE_RENT - 1, LIVE_RENT),
            TopUp::Unexpected { deficit: 205_657, gap: 205_656 }
        );
    }

    #[test]
    fn reserve_beyond_the_schedule_is_not_funded() {
        let forged = 100 * FROZEN_RENT;
        assert_eq!(
            top_up(&delegated_stake(forged), DELEGATED, LIVE_RENT),
            TopUp::Unexpected { deficit: forged, gap: FROZEN_RENT - LIVE_RENT }
        );
    }

    #[test]
    fn reserve_just_above_the_frozen_value_is_not_funded() {
        assert_eq!(
            top_up(&delegated_stake(4_000_000), DELEGATED + LIVE_RENT, LIVE_RENT),
            TopUp::Unexpected { deficit: 1_922_776, gap: FROZEN_RENT - LIVE_RENT }
        );
    }

    #[test]
    fn undelegated_account_is_left_alone() {
        let initialized = StakeStateV2::Initialized(Meta { rent_exempt_reserve: FROZEN_RENT, ..Meta::default() });
        assert_eq!(top_up(&initialized, 0, LIVE_RENT), TopUp::NotNeeded);
    }
}
