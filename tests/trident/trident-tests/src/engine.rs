//! Fuzz step engine: build a valid instruction for the current chain state, optionally mutate its
//! account set, execute it, and judge the outcome (oracles + invariants).

use crate::invariants;
use crate::ixs::{self, Gate, Ix, ALL_IX};
use crate::mutate::{self, Mutation};
use crate::util::*;
use crate::world::{World, TICKET_SIZE};
use anchor_lang::solana_program::stake::state::StakeStateV2;
use marinade_finance::instructions::admin::{ChangeAuthorityData, ConfigLpParams, ConfigMarinadeParams};
use marinade_finance::state::fee::{Fee, FeeCents};
use marinade_finance::State;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

#[derive(Default)]
pub struct Fx {
    pub world: Option<World>,
    pub steps: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// adversarial: lots of account mutation, short sequences
    Ix,
    /// mostly valid long sequences with epoch progression, rewards, a little mutation
    Flow,
}

// ---------------------------------------------------------------------------- global bookkeeping

static STATS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());
static FINDINGS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
static LAST_FLUSH: Mutex<Option<std::time::Instant>> = Mutex::new(None);

thread_local! {
    static LAST_PANIC: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Chain a panic hook that remembers the last panic message + location (program panics are caught by
/// the native executor and only surface as `ProgramFailedToComplete`).
pub fn install_panic_recorder() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        LAST_PANIC.with(|p| *p.borrow_mut() = format!("{msg} @ {loc}"));
        prev(info)
    }));
}

fn stat(key: String) {
    *STATS.lock().unwrap().entry(key).or_default() += 1;
}

fn env_u(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn only() -> Option<Vec<String>> {
    std::env::var("FUZZ_ONLY").ok().filter(|s| !s.is_empty()).map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
}

fn allowed(sig: &str) -> bool {
    std::env::var("FUZZ_ALLOW").map(|v| v.split(',').any(|a| !a.is_empty() && sig.contains(a.trim()))).unwrap_or(false)
}

/// Write cumulative stats (called from `end`); cheap no-op most of the time.
pub fn flush_stats(tag: &str, force: bool) {
    let mut last = LAST_FLUSH.lock().unwrap();
    if !force && last.map(|l| l.elapsed().as_secs() < 5).unwrap_or(false) {
        return;
    }
    *last = Some(std::time::Instant::now());
    let dir = std::env::var("FUZZ_OUT").unwrap_or_else(|_| "fuzz-out".into());
    let _ = std::fs::create_dir_all(&dir);
    let s = STATS.lock().unwrap();
    let body = serde_lines(&s);
    let _ = std::fs::write(format!("{dir}/{tag}-stats.txt"), body);
}

fn serde_lines(s: &BTreeMap<String, u64>) -> String {
    s.iter().map(|(k, v)| format!("{v}\t{k}\n")).collect()
}

fn normalize(msg: &str) -> String {
    // drop digits and long base58-looking tokens so the signature is stable across runs
    let mut out = String::new();
    for tok in msg.split_whitespace().take(24) {
        let t: String = tok.chars().filter(|c| !c.is_ascii_digit()).collect();
        if tok.len() >= 20 && tok.chars().all(|c| c.is_ascii_alphanumeric()) {
            out.push_str("<key> ");
        } else {
            out.push_str(&t);
            out.push(' ');
        }
    }
    out.trim().chars().take(120).collect()
}

/// Record a finding. First occurrence of a signature writes a report and panics (so Trident prints
/// the seed and the iteration is replayable with `trident fuzz debug`); repeats are only counted.
fn finding(cat: &str, ix: &str, msg: String, ctx: String) {
    let sig = format!("{cat}:{ix}:{}", normalize(&msg));
    stat(format!("FINDING {sig}"));
    let first = {
        let mut g = FINDINGS.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::new);
        let c = m.entry(sig.clone()).or_default();
        *c += 1;
        *c == 1
    };
    if !first || allowed(&sig) {
        return;
    }
    let dir = std::env::var("FUZZ_OUT").unwrap_or_else(|_| "fuzz-out".into());
    let _ = std::fs::create_dir_all(format!("{dir}/findings"));
    let file = format!("{dir}/findings/{}.txt", sig.replace(|c: char| !c.is_ascii_alphanumeric(), "_").chars().take(120).collect::<String>());
    let _ = std::fs::write(&file, format!("{sig}\n\n{msg}\n\n{ctx}\n"));
    panic!("FINDING [{sig}] {msg} (details: {file})");
}

// ---------------------------------------------------------------------------- argument helpers

fn rnd(t: &mut Trident, n: usize) -> usize {
    t.random_from_range(0..n)
}

fn pct(t: &mut Trident, p: u32) -> bool {
    (t.random_from_range(0u32..100)) < p
}

/// u64 with edge values: 0, 1, around `hint` (balance / min), u64::MAX, random, log-uniform.
fn amount(t: &mut Trident, hint: u64) -> u64 {
    match rnd(t, 14) {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => hint,
        4 => hint.saturating_sub(1),
        5 => hint.saturating_add(1),
        6 => u64::MAX,
        7 => u64::MAX - 1,
        8 => hint / 2,
        9..=10 => {
            if hint == 0 {
                0
            } else {
                t.random_from_range(0..=hint)
            }
        }
        11 => 1_000_000,
        _ => {
            let bits = t.random_from_range(0u32..64);
            let v: u64 = t.random_from_range(0..=u64::MAX);
            v >> (63 - bits)
        }
    }
}

fn idx(t: &mut Trident, len: usize) -> u32 {
    match rnd(t, 12) {
        0 => len as u32,
        1 => u32::MAX,
        2 => (len as u32).saturating_add(1),
        _ if len == 0 => 0,
        _ => rnd(t, len) as u32,
    }
}

fn user_idx(t: &mut Trident, w: &World) -> usize {
    rnd(t, w.users.len())
}

fn wallet(t: &mut Trident, w: &World) -> Pubkey {
    match rnd(t, 4) {
        0 => t.random_pubkey(),
        1 => w.attacker.key,
        _ => w.users[user_idx(t, w)].key,
    }
}

fn refresh_authorities(w: &mut World, st: &State) {
    w.a.admin = st.admin_authority;
    w.a.vmgr = st.validator_system.manager_authority;
    w.a.pauser = st.pause_authority;
    w.a.operational = st.operational_sol_account;
    w.a.treasury_msol = st.treasury_msol_account;
}

// ---------------------------------------------------------------------------- builders

struct Built {
    ix: Ix,
    new_accounts: Vec<Pubkey>,
}

fn fee(t: &mut Trident) -> Fee {
    Fee::from_basis_points(match rnd(t, 6) {
        0 => 0,
        1 => 10_000,
        2 => 10_001,
        3 => u32::MAX,
        _ => t.random_from_range(0u32..1200),
    })
}

fn feec(t: &mut Trident) -> FeeCents {
    FeeCents::from_bp_cents(match rnd(t, 6) {
        0 => 0,
        1 => 1_000_000,
        2 => 1_000_001,
        3 => u32::MAX,
        _ => t.random_from_range(0u32..30_000),
    })
}

fn opt<T>(t: &mut Trident, f: impl FnOnce(&mut Trident) -> T) -> Option<T> {
    if pct(t, 35) {
        Some(f(t))
    } else {
        None
    }
}

fn build(w: &mut World, t: &mut Trident, name: &str, st: &State) -> Option<Built> {
    let a = w.a.clone();
    let ui = user_idx(t, w);
    let u = w.users[ui].clone();
    let mut new_accounts = vec![];
    let recs = w.stakes(t, &a);
    let vals = w.validators(t, &a);
    let stake_pick = |t: &mut Trident, w: &World| -> (u32, Pubkey, u32) {
        // (stake_index, stake_account, validator_index of its validator)
        if recs.is_empty() || pct(t, 8) {
            let k = if pct(t, 50) { t.random_pubkey() } else { w.pool[rnd(t, w.pool.len())] };
            return (idx(t, recs.len()), k, idx(t, vals.len()));
        }
        let i = rnd(t, recs.len());
        let mut vi = idx(t, vals.len());
        let acc = t.get_account(&recs[i].stake_account);
        if !pct(t, 10) {
            if let Ok(StakeStateV2::Stake(_, s, _)) = bincode::deserialize::<StakeStateV2>(acc.data()) {
                if let Some(p) = vals.iter().position(|v| v.validator_account == s.delegation.voter_pubkey) {
                    vi = p as u32;
                }
            }
        }
        (i as u32, recs[i].stake_account, vi)
    };
    let min_dep = st.min_deposit;
    let ix = match name {
        "initialize" => {
            let c = w.c.clone();
            let mut cfg = w.cfg.clone();
            if pct(t, 20) {
                cfg.min_stake = amount(t, SOL);
            }
            if pct(t, 20) {
                cfg.reward_fee_bp = t.random_from_range(0u32..2000);
            }
            if pct(t, 20) {
                cfg.slots_for_stake_delta = amount(t, 3000);
            }
            let mut data = crate::world::init_data(&c, &cfg);
            if pct(t, 15) {
                data.liq_pool.lp_max_fee = fee(t);
                data.liq_pool.lp_min_fee = fee(t);
                data.liq_pool.lp_treasury_cut = fee(t);
            }
            ixs::initialize(&c, data)
        }
        "deposit" => {
            let bal = lam(t, &u.key);
            ixs::deposit(&a, &u.key, &u.msol, amount(t, if pct(t, 50) { min_dep } else { bal }))
        }
        "liquid_unstake" => {
            let bal = token_amount(t, &u.msol);
            let to = if pct(t, 70) { u.key } else { wallet(t, w) };
            ixs::liquid_unstake(&a, &u.msol, &u.key, &to, amount(t, bal))
        }
        "add_liquidity" => {
            let bal = lam(t, &u.key);
            ixs::add_liquidity(&a, &u.key, &u.lp, amount(t, if pct(t, 50) { st.min_deposit.max(SOL) } else { bal }))
        }
        "remove_liquidity" => {
            let bal = token_amount(t, &u.lp);
            ixs::remove_liquidity(&a, &u.lp, &u.key, &u.key, &u.msol, amount(t, bal))
        }
        "order_unstake" => {
            let k = t.random_pubkey();
            let l = rent_min(t, TICKET_SIZE);
            put(t, &k, l, vec![0u8; TICKET_SIZE], &program_id());
            new_accounts.push(k);
            let bal = token_amount(t, &u.msol);
            ixs::order_unstake(&a, &u.msol, &u.key, &k, amount(t, bal))
        }
        "claim" => {
            let live: Vec<Pubkey> = w.tickets.iter().copied().filter(|k| w.ticket(t, k).is_some()).collect();
            let k = if live.is_empty() || pct(t, 5) { w.spare_tickets[rnd(t, w.spare_tickets.len())] } else { live[rnd(t, live.len())] };
            let ben = w.ticket(t, &k).map(|d| d.beneficiary).unwrap_or(u.key);
            ixs::claim(&a, &k, &ben)
        }
        "stake_reserve" => {
            let vi = idx(t, vals.len());
            let vote = vals.get(vi as usize).map(|v| v.validator_account).unwrap_or_else(|| t.random_pubkey());
            let k = t.random_pubkey();
            new_accounts.push(k);
            ixs::stake_reserve(&a, vi, &vote, &k, &u.key)
        }
        "update_active" => {
            let (si, sk, vi) = stake_pick(t, w);
            ixs::update_active(&a, &sk, si, vi)
        }
        "update_deactivated" => {
            let (si, sk, vi) = stake_pick(t, w);
            ixs::update_deactivated(&a, &sk, si, vi)
        }
        "deactivate_stake" => {
            let (si, sk, vi) = stake_pick(t, w);
            let k = t.random_pubkey();
            new_accounts.push(k);
            ixs::deactivate_stake(&a, &sk, &k, &u.key, si, vi)
        }
        "emergency_unstake" => {
            let (si, sk, vi) = stake_pick(t, w);
            ixs::emergency_unstake(&a, &sk, si, vi)
        }
        "partial_unstake" => {
            let (si, sk, vi) = stake_pick(t, w);
            let k = t.random_pubkey();
            new_accounts.push(k);
            let hint = recs.get(si as usize).map(|r| r.last_update_delegated_lamports).unwrap_or(SOL);
            ixs::partial_unstake(&a, &sk, &k, &u.key, si, vi, amount(t, hint))
        }
        "merge_stakes" => {
            let (si, sk, vi) = stake_pick(t, w);
            let (di, dk) = if recs.len() > 1 && pct(t, 85) {
                let j = rnd(t, recs.len());
                (j as u32, recs[j].stake_account)
            } else {
                let vote = vals.get(vi as usize).map(|v| v.validator_account).unwrap_or_default();
                (idx(t, recs.len()), a.canonical_stake(&vote))
            };
            ixs::merge_stakes(&a, &dk, &sk, di, si, vi)
        }
        "create_canonical_stake" => {
            let (si, sk, vi) = stake_pick(t, w);
            let vote = vals.get(vi as usize).map(|v| v.validator_account).unwrap_or_else(|| t.random_pubkey());
            ixs::create_canonical_stake(&a, &a.canonical_stake(&vote), &sk, si, vi)
        }
        "withdraw_stake_account" => {
            let (si, sk, vi) = stake_pick(t, w);
            let k = t.random_pubkey();
            new_accounts.push(k);
            let bal = token_amount(t, &u.msol);
            let ben = wallet(t, w);
            ixs::withdraw_stake_account(&a, &sk, &k, &u.key, &u.msol, &u.key, si, vi, amount(t, bal), &ben)
        }
        "deposit_stake_account" => {
            let e = w.epoch(t);
            let k = t.random_pubkey();
            let vi = idx(t, vals.len());
            let reg = vals.get(vi as usize).map(|v| v.validator_account);
            let kind = rnd(t, 10);
            let vote = match kind {
                6 => w.spare_votes[rnd(t, w.spare_votes.len())],
                _ => reg.unwrap_or(w.votes[0]),
            };
            let amt = amount(t, 20 * SOL).clamp(1, 1_000 * SOL);
            let other = w.users[(ui + 1) % w.users.len()].key;
            match kind {
                7 => put_stake(t, &k, &u.key, &u.key, &vote, amt, e, u64::MAX),
                8 => put_stake(t, &k, &u.key, &other, &vote, amt, e - 3, u64::MAX),
                9 => put_stake_initialized(t, &k, &u.key, &u.key, amt),
                5 => put_stake(t, &k, &u.key, &u.key, &vote, amt, e - 6, e - 1),
                _ => put_stake(t, &k, &u.key, &u.key, &vote, amt, e - 3, u64::MAX),
            }
            new_accounts.push(k);
            w.tags.insert(k, vec!["stake_user"]);
            ixs::deposit_stake_account(&a, &k, &u.key, &a.dup_flag(&vote), &u.key, &u.msol, vi)
        }
        "add_validator" => {
            let vote = match rnd(t, 6) {
                0 => w.fake_vote,
                1 => t.random_pubkey(),
                2 => w.votes[rnd(t, w.votes.len())],
                _ => {
                    let v = t.random_pubkey();
                    put_vote(t, &v);
                    new_accounts.push(v);
                    v
                }
            };
            ixs::add_validator(&a, &vote, &a.dup_flag(&vote), &u.key, amount(t, 1000).min(u32::MAX as u64) as u32)
        }
        "remove_validator" => {
            let vi = idx(t, vals.len());
            let vote = vals.get(vi as usize).map(|v| v.validator_account).unwrap_or_else(|| t.random_pubkey());
            ixs::remove_validator(&a, vi, &vote, &a.dup_flag(&vote))
        }
        "set_validator_score" => {
            let vi = idx(t, vals.len());
            let vote = vals.get(vi as usize).map(|v| v.validator_account).unwrap_or_else(|| t.random_pubkey());
            ixs::set_validator_score(&a, vi, &vote, amount(t, 1000).min(u32::MAX as u64) as u32)
        }
        "config_validator_system" => ixs::config_validator_system(&a, amount(t, 10).min(u32::MAX as u64) as u32),
        "config_marinade" => {
            let p = ConfigMarinadeParams {
                rewards_fee: opt(t, |t| fee(t)),
                slots_for_stake_delta: opt(t, |t| amount(t, 3000)),
                min_stake: opt(t, |t| amount(t, SOL)),
                min_deposit: opt(t, |t| amount(t, 1_000_000)),
                min_withdraw: opt(t, |t| amount(t, 1_000_000)),
                staking_sol_cap: opt(t, |t| amount(t, 1_000_000 * SOL)),
                liquidity_sol_cap: opt(t, |t| amount(t, 1_000_000 * SOL)),
                withdraw_stake_account_enabled: opt(t, |t| pct(t, 70)),
                delayed_unstake_fee: opt(t, |t| feec(t)),
                withdraw_stake_account_fee: opt(t, |t| feec(t)),
                max_stake_moved_per_epoch: opt(t, |t| fee(t)),
                deposit_sol_fee: opt(t, |t| feec(t)),
                deposit_stake_account_fee: opt(t, |t| feec(t)),
            };
            ixs::config_marinade(&a, p)
        }
        "config_lp" => {
            let p = ConfigLpParams { min_fee: opt(t, |t| fee(t)), max_fee: opt(t, |t| fee(t)), liquidity_target: opt(t, |t| amount(t, 100 * SOL)), treasury_cut: opt(t, |t| fee(t)) };
            ixs::config_lp(&a, p)
        }
        "change_authority" => {
            let mut p = |t: &mut Trident| -> Option<Pubkey> {
                if pct(t, 25) {
                    Some(if pct(t, 60) { t.random_pubkey() } else { wallet(t, w) })
                } else {
                    None
                }
            };
            let d = ChangeAuthorityData {
                admin: p(t),
                validator_manager: p(t),
                operational_sol_account: p(t),
                treasury_msol_account: if pct(t, 25) { Some(if pct(t, 50) { u.msol } else { t.random_pubkey() }) } else { None },
                pause_authority: p(t),
            };
            ixs::change_authority(&a, d)
        }
        "pause" => ixs::pause(&a),
        "resume" => ixs::resume(&a),
        "realloc_validator_list" => {
            let cap = match rnd(t, 5) {
                0 => 0,
                1 => u32::MAX,
                2 => 100_000,
                3 => (vals.len() as u32).saturating_sub(1),
                _ => rnd(t, 64) as u32,
            };
            ixs::realloc_validator_list(&a, &u.key, cap)
        }
        "realloc_stake_list" => {
            let cap = match rnd(t, 5) {
                0 => 0,
                1 => u32::MAX,
                2 => 100_000,
                3 => (recs.len() as u32).saturating_sub(1),
                _ => rnd(t, 64) as u32,
            };
            ixs::realloc_stake_list(&a, &u.key, cap)
        }
        "finalize_delinquent_upgrade" => ixs::finalize_delinquent_upgrade(&a, amount(t, 10).min(u32::MAX as u64) as u32),
        other => panic!("unknown ix {other}"),
    };
    Some(Built { ix, new_accounts })
}

// ---------------------------------------------------------------------------- execution

fn describe(ix: &Ix, muts: &[Mutation], w: &World, t: &mut Trident) -> String {
    let mut s = format!("ix {} data {}\n", ix.name, hex::encode(&ix.data));
    for (i, sl) in ix.slots.iter().enumerate() {
        s += &format!(
            "  #{i:<2} {:<18} {:?} {} {}{} {:?}\n",
            sl.name,
            sl.role,
            sl.meta.pubkey,
            if sl.meta.is_signer { "S" } else { "-" },
            if sl.meta.is_writable { "W" } else { "-" },
            w.tags_of(t, &sl.meta.pubkey)
        );
    }
    for m in muts {
        s += &format!("  mutation: slot {} {} {}\n", m.slot, m.kind, m.detail);
    }
    s
}

fn lamports_sum(t: &mut Trident, keys: &[Pubkey]) -> u128 {
    keys.iter().map(|k| lam(t, k) as u128).sum()
}

fn hash_b(w: &World, t: &mut Trident) -> Vec<u8> {
    let b = &w.b;
    let mut h = vec![];
    for k in [b.state, b.stake_list, b.validator_list, b.msol_mint, b.lp_mint, b.msol_leg, b.treasury_msol] {
        let a = t.get_account(&k);
        h.extend_from_slice(&a.lamports().to_le_bytes());
        h.extend_from_slice(a.owner().as_ref());
        h.extend_from_slice(a.data());
    }
    h
}

/// Execute one instruction (valid, possibly mutated) and judge the outcome.
pub fn run_ix(w: &mut World, t: &mut Trident, name: &str, mutate_pct: u32) {
    let Some(st) = w.state(t, &w.a.clone()) else { return };
    refresh_authorities(w, &st);
    let Some(Built { ix: orig, new_accounts }) = build(w, t, name, &st) else { return };
    for k in &new_accounts {
        w.track(t, *k);
    }
    for s in &orig.slots {
        w.track(t, s.meta.pubkey);
    }
    let mut ix = orig.clone();
    let mut muts = vec![];
    if pct(t, mutate_pct) {
        let n = if pct(t, 75) { 1 } else { 2 + rnd(t, 2) };
        muts = mutate::mutate(w, t, &mut ix, n);
        for s in ix.slots.clone() {
            w.track(t, s.meta.pubkey);
        }
    }
    let pre = invariants::take(w, t);
    let keys: Vec<Pubkey> = w.tracked.iter().copied().collect();
    let before = lamports_sum(t, &keys);
    let b_before = hash_b(w, t);
    let reserve_before = lam(t, &w.a.reserve);

    let out = w.send(t, &ix);
    let mutated = !muts.is_empty();
    let kind = if mutated { "mut" } else { "valid" };
    if out.panicked {
        let p = LAST_PANIC.with(|p| p.borrow().clone());
        let ctx = format!("{}\n{}\nlogs:\n{}", describe(&ix, &muts, w, t), p, out.logs);
        stat(format!("{name}\t{kind}\tPANIC"));
        finding("program_panic", name, format!("program panicked: {p}"), ctx);
        return;
    }
    if !out.ok {
        let class = match out.code {
            Some(c) => format!("custom({c})"),
            None => out.err.chars().take(60).collect(),
        };
        stat(format!("{name}\t{kind}\tERR {class}"));
        return;
    }
    stat(format!("{name}\t{kind}\tOK"));
    let ctx = describe(&ix, &muts, w, t);

    // -- substitution must not succeed
    if mutated {
        if let Some(reason) = mutate::must_fail(w, t, &orig, &ix) {
            finding("substitution_succeeded", name, reason, ctx.clone());
        }
    }
    // -- authority gate: the *pre-state* authority must have signed
    if let Some(p) = &pre {
        let auth = match ix.gate {
            Gate::None => None,
            Gate::Admin => Some(p.st.admin_authority),
            Gate::Vmgr => Some(p.st.validator_system.manager_authority),
            Gate::Pauser => Some(p.st.pause_authority),
        };
        if let Some(auth) = auth {
            if !ix.slots.iter().any(|s| s.meta.pubkey == auth && s.meta.is_signer) {
                finding("authority_bypass", name, format!("{name} succeeded although {:?} authority {auth} did not sign", ix.gate), ctx.clone());
            }
        }
    }
    // -- bookkeeping of legitimate side effects
    for (o, f) in orig.slots.iter().zip(&ix.slots) {
        if o.meta.pubkey != f.meta.pubkey && f.meta.pubkey == w.a.reserve {
            w.donated = true;
        }
    }
    for k in &new_accounts {
        w.track(t, *k);
    }
    if name == "order_unstake" {
        w.tickets.push(new_accounts[0]);
    }
    if name == "stake_reserve" || name == "deactivate_stake" || name == "partial_unstake" || name == "withdraw_stake_account" {
        // the new account may now be a stake account (split / fresh): nothing to do, tracked above
    }
    let _ = reserve_before;
    // -- conservation of lamports over every account that took part
    let mut keys: Vec<Pubkey> = w.tracked.iter().copied().collect();
    keys.sort();
    let after = lamports_sum(t, &keys);
    if after != before {
        finding("lamports_not_conserved", name, format!("sum over {} tracked accounts changed: {} -> {}", keys.len(), before, after), ctx.clone());
    }
    // -- instance B must be untouched by a transaction addressed to A
    if hash_b(w, t) != b_before {
        finding("cross_instance_write", name, "instance B state/list/mint/leg accounts were modified by a tx for instance A".into(), ctx.clone());
    }
    // -- accounting invariants
    for v in invariants::check(w, t, name, pre.as_ref()) {
        finding("invariant", name, v, ctx.clone());
    }
}

/// instruction-data garbage: valid discriminator (or random), random bytes, random accounts.
/// Oracle: no panic, and invariants hold on success.
pub fn run_garbage(w: &mut World, t: &mut Trident) {
    let name = ALL_IX[rnd(t, ALL_IX.len())];
    let st = match w.state(t, &w.a.clone()) {
        Some(s) => s,
        None => return,
    };
    refresh_authorities(w, &st);
    let Some(Built { ix: mut ix, .. }) = build(w, t, name, &st) else { return };
    let len = rnd(t, 48);
    let mut junk = vec![0u8; len];
    t.random_bytes(&mut junk);
    match rnd(t, 4) {
        0 => ix.data.truncate(rnd(t, ix.data.len() + 1)),
        1 => ix.data.extend_from_slice(&junk),
        2 => {
            let keep = ix.data.len().min(8);
            ix.data.truncate(keep);
            ix.data.extend_from_slice(&junk);
        }
        _ => ix.data = junk,
    }
    // drop or add accounts
    match rnd(t, 3) {
        0 if !ix.slots.is_empty() => {
            let i = rnd(t, ix.slots.len());
            ix.slots.remove(i);
        }
        1 => {
            let extra = w.pool[rnd(t, w.pool.len())];
            let mut s = ix.slots[0].clone();
            s.meta.pubkey = extra;
            s.meta.is_signer = false;
            ix.slots.push(s);
        }
        _ => {}
    }
    for s in &ix.slots {
        w.track(t, s.meta.pubkey);
    }
    let pre = invariants::take(w, t);
    let keys: Vec<Pubkey> = w.tracked.iter().copied().collect();
    let before = lamports_sum(t, &keys);
    let out = w.send(t, &ix);
    if out.panicked {
        let p = LAST_PANIC.with(|p| p.borrow().clone());
        stat(format!("{name}\tgarbage\tPANIC"));
        finding("program_panic", name, format!("garbage input panicked: {p}"), format!("{}\n{}", describe(&ix, &[], w, t), out.logs));
        return;
    }
    stat(format!("{name}\tgarbage\t{}", if out.ok { "OK".to_string() } else { "ERR".to_string() }));
    if out.ok {
        let after = lamports_sum(t, &keys);
        let ctx = describe(&ix, &[], w, t);
        if after != before {
            finding("lamports_not_conserved", name, format!("garbage tx changed lamports {before} -> {after}"), ctx.clone());
        }
        for v in invariants::check(w, t, name, pre.as_ref()) {
            finding("invariant", name, v, ctx.clone());
        }
    }
}

// ---------------------------------------------------------------------------- world actions

pub fn world_action(w: &mut World, t: &mut Trident, mode: Mode) {
    let e = w.epoch(t);
    let a = w.a.clone();
    match rnd(t, 10) {
        0..=2 => {
            // new epoch, usually in the stake-delta window of that epoch
            let n = 1 + rnd(t, 2) as u64;
            let before_end = if pct(t, 70) { rnd(t, 2_000) as u64 } else { 1_000_000 };
            w.set_clock(t, e + n, before_end);
            stat("world\tadvance_epoch".into());
        }
        3..=4 => {
            let before_end = rnd(t, 5_000) as u64;
            w.set_clock(t, e, before_end);
            stat("world\tset_slot".into());
        }
        5..=6 => {
            // staking rewards on one or all stakes
            let recs = w.stakes(t, &a);
            for r in recs {
                if pct(t, 60) {
                    let l = r.last_update_delegated_lamports;
                    let d = (l / 1_000).max(1) as i64 * (1 + rnd(t, 10) as i64);
                    adjust_stake(t, &r.stake_account, d);
                }
            }
            stat("world\trewards".into());
        }
        7 => {
            // unrecorded lamports sent straight to a stake account
            let recs = w.stakes(t, &a);
            if !recs.is_empty() {
                let r = recs[rnd(t, recs.len())];
                let acc = t.get_account(&r.stake_account);
                let l = acc.lamports() + 1 + rnd(t, 100_000) as u64;
                put(t, &r.stake_account, l, acc.data().to_vec(), &stake_id());
            }
            stat("world\tstake_donation".into());
        }
        8 => {
            if env_u("FUZZ_DONATE", 1) == 1 && pct(t, 40) {
                // lamports straight to the reserve PDA (known: later stake_reserve / claim may overflow)
                let l = lam(t, &a.reserve);
                put(t, &a.reserve, l + 1 + rnd(t, 10 * SOL as usize) as u64, vec![], &system_id());
                w.donated = true;
                stat("world\treserve_donation".into());
            }
        }
        _ => {
            if mode == Mode::Flow && env_u("FUZZ_SLASH", 1) == 1 && pct(t, 30) {
                let recs = w.stakes(t, &a);
                if !recs.is_empty() {
                    let r = recs[rnd(t, recs.len())];
                    let l = r.last_update_delegated_lamports;
                    if adjust_stake(t, &r.stake_account, -((l / 100).max(1) as i64)) {
                        w.slashed = true;
                    }
                }
                stat("world\tslash".into());
            }
        }
    }
}

// ---------------------------------------------------------------------------- scripted warm-up

pub fn warmup(w: &mut World, t: &mut Trident) {
    let a = w.a.clone();
    let e = w.epoch(t);
    // late in the epoch: users deposit and add liquidity
    w.set_clock(t, e, 5_000_000);
    for ui in 0..w.users.len() {
        let u = w.users[ui].clone();
        let n = 50 + rnd(t, 200) as u64;
        let _ = w.send(t, &ixs::deposit(&a, &u.key, &u.msol, n * SOL));
        let _ = w.send(t, &ixs::add_liquidity(&a, &u.key, &u.lp, (5 + rnd(t, 20) as u64) * SOL));
    }
    // inside the stake-delta window: stake into every validator
    w.set_clock(t, e, 100);
    let nvals = w.validators(t, &a).len();
    for vi in 0..nvals {
        let vote = w.validators(t, &a)[vi].validator_account;
        let k = t.random_pubkey();
        let payer = w.users[0].key;
        let o = w.send(t, &ixs::stake_reserve(&a, vi as u32, &vote, &k, &payer));
        if o.ok {
            w.track(t, k);
        }
    }
    // roll into the next epoch(s) so stake is active; some unstake orders
    w.set_clock(t, e + 1, 100);
    let (u1, u2) = (w.users[1].clone(), w.users[2].clone());
    for u in [u1, u2] {
        if pct(t, 60) {
            let k = t.random_pubkey();
            let l = rent_min(t, TICKET_SIZE);
            put(t, &k, l, vec![0u8; TICKET_SIZE], &program_id());
            let bal = token_amount(t, &u.msol);
            let o = w.send(t, &ixs::order_unstake(&a, &u.msol, &u.key, &k, bal / 4));
            if o.ok {
                w.tickets.push(k);
                w.track(t, k);
            }
        }
    }
    // rewards on the active stakes
    for r in w.stakes(t, &a) {
        adjust_stake(t, &r.stake_account, (r.last_update_delegated_lamports / 500) as i64);
    }
    let tracked: Vec<Pubkey> = w.tracked.iter().copied().collect();
    for k in tracked {
        let _ = lam(t, &k);
    }
    w.prev = invariants::take(w, t);
    // sanity: invariants hold after a valid warm-up
    let v = invariants::check(w, t, "warmup", None);
    assert!(v.is_empty(), "invariants violated by the scripted warm-up (harness bug?): {v:?}");
}

// ---------------------------------------------------------------------------- driver

pub fn setup(t: &mut Trident, fx: &mut Fx) {
    let mut w = World::build(t);
    warmup(&mut w, t);
    fx.world = Some(w);
    fx.steps = 0;
}

pub fn step(t: &mut Trident, fx: &mut Fx, mode: Mode) {
    let Some(w) = fx.world.as_mut() else { return };
    fx.steps += 1;
    let mutate_pct = match mode {
        Mode::Ix => env_u("FUZZ_MUTATE_PCT", 65) as u32,
        Mode::Flow => env_u("FUZZ_MUTATE_PCT", 3) as u32,
    };
    let focus = only();
    let r = rnd(t, 100);
    let world_pct = if mode == Mode::Flow { 25 } else { 12 };
    if r < world_pct {
        world_action(w, t, mode);
        return;
    }
    if mode == Mode::Ix && r < world_pct + 3 {
        run_garbage(w, t);
        return;
    }
    let name = match &focus {
        // focused run: 70% the target instruction, 30% a random driver instruction that evolves state
        Some(f) if pct(t, 70) => f[rnd(t, f.len())].clone(),
        _ => ALL_IX[rnd(t, ALL_IX.len())].to_string(),
    };
    let is_driver = focus.as_ref().map(|f| !f.contains(&name)).unwrap_or(false);
    run_ix(w, t, &name, if is_driver { 0 } else { mutate_pct });
}
