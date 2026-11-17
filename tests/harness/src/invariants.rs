//! Accounting invariants for one Marinade instance, checked after every op.
//!
//! Notation (all values read from chain state):
//! * `R  = state.rent_exempt_for_token_acc`, `A = state.available_reserve_balance`
//! * `reserve = lamports(reserve PDA)`, `rent_s = Rent::minimum_balance(200)` (live stake rent)
//! * a stake record `r` has `status in {Active, Deactivating}`, `r.last_update_delegated_lamports = L(r)`
//!
//! Exact relations (derived from the instruction code):
//! 1. `state.msol_supply == msol_mint.supply` (>= when `exact_supply` is off: external burns)
//! 2. `state.liq_pool.lp_supply == lp_mint.supply` (>= when `exact_supply` is off)
//! 3. `reserve == A + R` (every program path moves state.available_reserve_balance and the reserve
//!    in lockstep and `update_*` re-reads it); `reserve >= A + R` when `exact_reserve` is off (donations)
//! 4. `lamports(sol leg) >= R`
//! 5. `total_active_balance == sum(validator.active_balance)`; `total_validator_score == sum(score)`
//! 6. per validator `v`: `v.active_balance == sum(L(r) : r Active, delegated to v)`
//! 7. `delayed_unstake_cooling_down == sum(L(r) : r Deactivating && !emergency)`,
//!    `emergency_cooling_down == sum(L(r) : r Deactivating && emergency)`
//! 8. `circulating_ticket_balance == sum(ticket.lamports_amount)`, `circulating_ticket_count == #ticket accounts`
//!    (zero-value tickets included: they can never be claimed, see tests/findings.rs)
//! 9. every tracked stake account: owned by the stake program, initialized+delegated, staker == deposit
//!    authority PDA, withdrawer == withdraw authority PDA, `lamports >= Rent::minimum_balance(200)`
//!    (rent-exempt under the LIVE rent) and `lamports >= delegation.stake`; Active record <=> delegation
//!    not deactivating; no duplicates; the validator it is delegated to exists (Active records only: a cooling-down stake may outlive a removed validator)
//! 10. every Marinade-owned account is rent-exempt under the live rent
//! 11. lamport conservation: `total(all accounts) - (injected - burned)` is unchanged between snapshots
//!     (the harness books every external mutation: airdrops, rewards, deployments, fees)
//! 12. `delinquent_upgrader` is `Done`
//!
//! Relations between snapshots (disabled by `allow_loss` for slashing):
//! 13. mSOL price `tvs/supply` never decreases, up to one lamport of rounding:
//!     `tvs_now*supply_prev + max(tvs) >= tvs_prev*supply_now` (cross-multiplied, u128)
//! 14. solvency: `(reserve - R) + sum(lamports - rent_s over tracked stakes) >= total_lamports_under_control`
//!     (unrecorded rewards/MEV/rent surplus only add to the left side)
//!
//! [`check_fully_updated`] (after a full crank, no slashing): every record satisfies
//! `last_update_epoch == epoch`, and for Active records `lamports - rent_s == L(r) == delegation.stake`.

use crate::convert::to_addr;
use crate::env::{StakeView, TestEnv};
use crate::marinade::{program_id, Marinade};
use marinade_finance::state::stake_system::StakeStatus;
use serde::Serialize;
use solana_address::Address;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
pub struct Violation {
    pub name: &'static str,
    pub detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.name, self.detail)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StateView {
    pub msol_supply: u64,
    pub msol_mint_supply: u64,
    pub lp_supply: u64,
    pub lp_mint_supply: u64,
    pub total_active_balance: u64,
    pub delayed_cooling_down: u64,
    pub emergency_cooling_down: u64,
    pub available_reserve_balance: u64,
    pub rent_exempt_for_token_acc: u64,
    pub ticket_balance: u64,
    pub ticket_count: u64,
    pub total_validator_score: u32,
    pub stake_count: u32,
    pub validator_count: u32,
    pub msol_price_stored: u64,
    pub tlc: u64,
    pub tvs: u64,
    pub upgrader_done: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct StakeSnap {
    pub address: String,
    pub last_update_delegated_lamports: u64,
    pub last_update_epoch: u64,
    pub active: bool,
    pub emergency: bool,
    pub lamports: u64,
    pub delegated: Option<u64>,
    pub vote: Option<String>,
    pub meta_rent_exempt_reserve: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ValidatorSnap {
    pub vote: String,
    pub active_balance: u64,
    pub score: u32,
}

/// Cloneable, inspectable picture of the instance + ledger.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub slot: u64,
    pub epoch: u64,
    pub state: StateView,
    pub stakes: Vec<StakeSnap>,
    pub validators: Vec<ValidatorSnap>,
    pub reserve_lamports: u64,
    pub sol_leg_lamports: u64,
    pub tickets: Vec<(String, u64)>,
    pub total_lamports: u128,
    pub external_net: i128,
    pub stake_rent: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub exact_reserve: bool,
    pub exact_supply: bool,
    /// slashing happened: skip the price-monotonic and solvency checks
    pub allow_loss: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { exact_reserve: true, exact_supply: true, allow_loss: false }
    }
}

pub fn snapshot(env: &TestEnv, m: &Marinade) -> Snapshot {
    let st = m.state(env);
    let recs = m.stake_records(env);
    let vals = m.validator_records(env);
    let tickets = m.tickets(env);
    let stakes = recs
        .iter()
        .map(|r| {
            let addr = to_addr(&r.stake_account);
            let v = env.stake_view(&addr);
            StakeSnap {
                address: addr.to_string(),
                last_update_delegated_lamports: r.last_update_delegated_lamports,
                last_update_epoch: r.last_update_epoch,
                active: r.last_update_status == StakeStatus::Active,
                emergency: r.is_emergency_unstaking,
                lamports: env.lamports(&addr),
                delegated: v.as_ref().and_then(|v| v.delegation.as_ref().map(|d| d.stake)),
                vote: v.as_ref().and_then(|v| v.delegation.as_ref().map(|d| d.voter.to_string())),
                meta_rent_exempt_reserve: v.as_ref().map(|v| v.rent_exempt_reserve).unwrap_or(0),
            }
        })
        .collect();
    Snapshot {
        slot: env.slot(),
        epoch: env.epoch(),
        state: StateView {
            msol_supply: st.msol_supply,
            msol_mint_supply: env.mint_supply(&m.msol_mint),
            lp_supply: st.liq_pool.lp_supply,
            lp_mint_supply: env.mint_supply(&m.lp_mint),
            total_active_balance: st.validator_system.total_active_balance,
            delayed_cooling_down: st.stake_system.delayed_unstake_cooling_down,
            emergency_cooling_down: st.emergency_cooling_down,
            available_reserve_balance: st.available_reserve_balance,
            rent_exempt_for_token_acc: st.rent_exempt_for_token_acc,
            ticket_balance: st.circulating_ticket_balance,
            ticket_count: st.circulating_ticket_count,
            total_validator_score: st.validator_system.total_validator_score,
            stake_count: st.stake_system.stake_count(),
            validator_count: st.validator_system.validator_count(),
            msol_price_stored: st.msol_price,
            tlc: st.total_lamports_under_control(),
            tvs: st.total_virtual_staked_lamports(),
            upgrader_done: st.delinquent_upgrader.is_done(),
        },
        stakes,
        validators: vals
            .iter()
            .map(|v| ValidatorSnap { vote: to_addr(&v.validator_account).to_string(), active_balance: v.active_balance, score: v.score })
            .collect(),
        reserve_lamports: env.lamports(&m.reserve),
        sol_leg_lamports: env.lamports(&m.sol_leg),
        tickets: tickets.iter().map(|(k, t)| (k.to_string(), t.lamports_amount)).collect(),
        total_lamports: env.total_lamports(),
        external_net: env.ledger.injected - env.ledger.burned as i128,
        stake_rent: env.stake_rent(),
    }
}

fn v(name: &'static str, detail: String) -> Violation {
    Violation { name, detail }
}

/// Check the state-only invariants (1-12) of `now`, plus the relations to `prev` (11, 13, 14).
pub fn check(env: &TestEnv, m: &Marinade, prev: Option<&Snapshot>, opts: &Options) -> (Snapshot, Vec<Violation>) {
    let now = snapshot(env, m);
    let mut out = vec![];
    let s = &now.state;
    // 1, 2
    if opts.exact_supply {
        if s.msol_supply != s.msol_mint_supply {
            out.push(v("msol_supply", format!("state {} != mint {}", s.msol_supply, s.msol_mint_supply)));
        }
        if s.lp_supply != s.lp_mint_supply {
            out.push(v("lp_supply", format!("state {} != mint {}", s.lp_supply, s.lp_mint_supply)));
        }
    } else {
        if s.msol_supply < s.msol_mint_supply {
            out.push(v("msol_supply", format!("state {} < mint {}", s.msol_supply, s.msol_mint_supply)));
        }
        if s.lp_supply < s.lp_mint_supply {
            out.push(v("lp_supply", format!("state {} < mint {}", s.lp_supply, s.lp_mint_supply)));
        }
    }
    // 3, 4
    let virt = s.available_reserve_balance + s.rent_exempt_for_token_acc;
    if now.reserve_lamports < virt || (opts.exact_reserve && now.reserve_lamports != virt) {
        out.push(v("reserve", format!("reserve lamports {} vs A+R {} (A={}, R={})", now.reserve_lamports, virt, s.available_reserve_balance, s.rent_exempt_for_token_acc)));
    }
    if now.sol_leg_lamports < s.rent_exempt_for_token_acc {
        out.push(v("sol_leg", format!("sol leg {} < R {}", now.sol_leg_lamports, s.rent_exempt_for_token_acc)));
    }
    // 5
    let sum_active: u64 = now.validators.iter().map(|x| x.active_balance).sum();
    if sum_active != s.total_active_balance {
        out.push(v("total_active_balance", format!("state {} != sum(validators) {}", s.total_active_balance, sum_active)));
    }
    let sum_score: u64 = now.validators.iter().map(|x| x.score as u64).sum();
    if sum_score != s.total_validator_score as u64 {
        out.push(v("total_validator_score", format!("state {} != sum {}", s.total_validator_score, sum_score)));
    }
    // 6, 7, 9
    let vote_set: BTreeSet<&String> = now.validators.iter().map(|x| &x.vote).collect();
    let mut per_val: BTreeMap<&String, u64> = BTreeMap::new();
    let (mut delayed, mut emergency) = (0u64, 0u64);
    let mut seen = BTreeSet::new();
    let rent_s = now.stake_rent;
    for st in &now.stakes {
        if !seen.insert(&st.address) {
            out.push(v("stake_list", format!("duplicate record {}", st.address)));
        }
        let addr: Address = st.address.parse().unwrap();
        let view: Option<StakeView> = env.stake_view(&addr);
        let Some(view) = view else {
            out.push(v("stake_account", format!("{} missing or undecodable", st.address)));
            continue;
        };
        if view.owner != crate::profile::addr(crate::profile::STAKE_ID) {
            out.push(v("stake_account", format!("{} not owned by the stake program", st.address)));
        }
        let Some(d) = &view.delegation else {
            out.push(v("stake_account", format!("{} is {} (not delegated)", st.address, view.kind)));
            continue;
        };
        if view.staker != m.deposit_auth || view.withdrawer != m.withdraw_auth {
            out.push(v("stake_authority", format!("{} staker {} withdrawer {}", st.address, view.staker, view.withdrawer)));
        }
        if view.lamports < rent_s {
            out.push(v("stake_rent_exempt", format!("{} holds {} < live stake rent {}", st.address, view.lamports, rent_s)));
        }
        if view.lamports < d.stake {
            out.push(v("stake_lamports", format!("{} holds {} < delegation {}", st.address, view.lamports, d.stake)));
        }
        // remove_validator only needs active_balance == 0: a stake that is still cooling down may outlive its validator
        // record (it then needs the validator re-added before update_deactivated, see tests/findings.rs); an ACTIVE
        // stake on an unlisted validator can never be right
        if st.active && !vote_set.contains(&d.voter.to_string()) {
            out.push(v("stake_validator", format!("{} is Active but delegated to unlisted validator {}", st.address, d.voter)));
        }
        if st.active {
            if d.deactivation_epoch != u64::MAX {
                out.push(v("stake_status", format!("{} recorded Active but deactivation_epoch={}", st.address, d.deactivation_epoch)));
            }
            *per_val.entry(vote_key(&now, &d.voter.to_string())).or_default() += st.last_update_delegated_lamports;
        } else {
            if d.deactivation_epoch == u64::MAX {
                out.push(v("stake_status", format!("{} recorded Deactivating but still active in the stake program", st.address)));
            }
            if st.emergency {
                emergency += st.last_update_delegated_lamports;
            } else {
                delayed += st.last_update_delegated_lamports;
            }
        }
    }
    if s.stake_count as usize != now.stakes.len() {
        out.push(v("stake_count", format!("{} != {}", s.stake_count, now.stakes.len())));
    }
    for val in &now.validators {
        let sum = per_val.get(&val.vote).copied().unwrap_or(0);
        if sum != val.active_balance {
            out.push(v("validator_active_balance", format!("validator {} active_balance {} != sum of active stake records {}", val.vote, val.active_balance, sum)));
        }
    }
    if delayed != s.delayed_cooling_down {
        out.push(v("delayed_cooling_down", format!("state {} != sum(deactivating, !emergency) {}", s.delayed_cooling_down, delayed)));
    }
    if emergency != s.emergency_cooling_down {
        out.push(v("emergency_cooling_down", format!("state {} != sum(deactivating, emergency) {}", s.emergency_cooling_down, emergency)));
    }
    // 8
    // zero-value tickets (min_withdraw = 0 allows ordering an unstake worth 0 lamports) are never claimable and
    // never closed, but they are counted in circulating_ticket_count: count every existing ticket account
    let tsum: u64 = now.tickets.iter().map(|t| t.1).sum();
    let tcount = now.tickets.len() as u64;
    if tsum != s.ticket_balance || tcount != s.ticket_count {
        out.push(v("tickets", format!("state balance/count {}/{} != tickets {}/{}", s.ticket_balance, s.ticket_count, tsum, tcount)));
    }
    // 10
    let rent = env.rent();
    for (k, acc) in env.accounts_owned_by(&program_id()) {
        if !rent.is_exempt(acc.lamports, acc.data.len()) {
            out.push(v("rent_exempt", format!("marinade-owned {} has {} lamports for {} bytes (min {})", k, acc.lamports, acc.data.len(), rent.minimum_balance(acc.data.len()))));
        }
    }
    // 12
    if !s.upgrader_done {
        out.push(v("delinquent_upgrader", "not Done".into()));
    }
    // relations
    if let Some(p) = prev {
        // 11
        let a = now.total_lamports as i128 - now.external_net;
        let b = p.total_lamports as i128 - p.external_net;
        if a != b {
            out.push(v("lamport_conservation", format!("total-external changed by {} lamports ({} -> {})", a - b, b, a)));
        }
        // 13
        if !opts.allow_loss && p.state.msol_supply > 0 && s.msol_supply > 0 {
            let lhs = s.tvs as u128 * p.state.msol_supply as u128 + (s.tvs.max(p.state.tvs)) as u128;
            let rhs = p.state.tvs as u128 * s.msol_supply as u128;
            if lhs < rhs {
                out.push(v(
                    "msol_price_monotonic",
                    format!(
                        "price fell: tvs/supply {}/{} -> {}/{} ({:.9} -> {:.9})",
                        p.state.tvs, p.state.msol_supply, s.tvs, s.msol_supply,
                        p.state.tvs as f64 / p.state.msol_supply as f64, s.tvs as f64 / s.msol_supply as f64
                    ),
                ));
            }
        }
    }
    // 14
    if !opts.allow_loss {
        let assets: i128 = (now.reserve_lamports as i128 - s.rent_exempt_for_token_acc as i128)
            + now.stakes.iter().map(|x| x.lamports as i128 - rent_s as i128).sum::<i128>();
        if assets < s.tlc as i128 {
            out.push(v("solvency", format!("real assets {} < total_lamports_under_control {}", assets, s.tlc)));
        }
    }
    (now, out)
}

fn vote_key<'a>(now: &'a Snapshot, vote: &str) -> &'a String {
    // map a vote string to the interned key of the validator list (falls back to the first, never used for unlisted)
    now.validators.iter().map(|x| &x.vote).find(|x| x.as_str() == vote).unwrap_or(&now.validators[0].vote)
}

/// `check_all(&env, &prev_snapshot)`: defaults, no failure aggregation beyond the violation list.
pub fn check_all(env: &TestEnv, m: &Marinade, prev: &Snapshot) -> Result<(), Vec<Violation>> {
    let (_, viol) = check(env, m, Some(prev), &Options::default());
    if viol.is_empty() { Ok(()) } else { Err(viol) }
}

/// Stateful helper: keeps the previous snapshot.
#[derive(Clone)]
pub struct Checker {
    pub prev: Snapshot,
    pub opts: Options,
}

impl Checker {
    pub fn new(env: &TestEnv, m: &Marinade) -> Self {
        Checker { prev: snapshot(env, m), opts: Options::default() }
    }
    pub fn with_opts(mut self, o: Options) -> Self {
        self.opts = o;
        self
    }
    /// Check against the previous snapshot, then advance it (also when violated).
    pub fn check(&mut self, env: &TestEnv, m: &Marinade) -> Result<(), Vec<Violation>> {
        let (now, viol) = check(env, m, Some(&self.prev), &self.opts);
        self.prev = now;
        if viol.is_empty() { Ok(()) } else { Err(viol) }
    }
    /// As [`Self::check`] with a readable error naming the op.
    pub fn check_after(&mut self, env: &TestEnv, m: &Marinade, op: &str) -> Result<(), String> {
        self.check(env, m).map_err(|vs| {
            format!("after {op}: {}", vs.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("; "))
        })
    }
}

/// After a full crank in the current epoch (no slashing): nothing is left un-accounted.
pub fn check_fully_updated(env: &TestEnv, m: &Marinade) -> Vec<Violation> {
    let mut out = vec![];
    let rent_s = env.stake_rent();
    let operational = crate::topup::active(env);
    let epoch = env.epoch();
    for r in m.stake_records(env) {
        let addr = to_addr(&r.stake_account);
        let Some(view) = env.stake_view(&addr) else { continue };
        let Some(d) = view.delegation else { continue };
        if r.last_update_status == StakeStatus::Active {
            if r.last_update_epoch != epoch {
                out.push(v("fully_updated", format!("{addr} last updated in epoch {} (now {epoch})", r.last_update_epoch)));
            }
            // operational mode (HARNESS_RENT_TOPUP): the binary under test works against delegation + the frozen meta
            // reserve, which is where the bot's top-up leaves the account
            let ok_balance = view.lamports == d.stake + rent_s || (operational && view.lamports == d.stake + view.rent_exempt_reserve);
            if r.last_update_delegated_lamports != d.stake || !ok_balance {
                out.push(v(
                    "fully_updated",
                    format!("{addr}: L={} delegation={} lamports-rent={}", r.last_update_delegated_lamports, d.stake, view.lamports as i128 - rent_s as i128),
                ));
            }
        }
    }
    out
}
