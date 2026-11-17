//! One "epoch crank" in the spirit of marcrank's `do-work`: update every stake, create canonical stakes
//! and merge, then (inside the stake-delta window) stake the reserve / unstake for tickets.
//!
//! Failures are recorded in the [`CrankReport`] (or returned immediately with `strict`). A hook runs after
//! every executed op (invariant checks).

use crate::convert::to_addr;
use crate::env::{Activation, StakeView, TestEnv, TxFailure};
use crate::marinade::Marinade;
use anyhow::{anyhow, Result};
use solana_address::Address;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct OpLog {
    pub name: String,
    pub ok: bool,
    pub marinade_code: Option<u32>,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CrankReport {
    pub ops: Vec<OpLog>,
}

impl CrankReport {
    pub fn count(&self, prefix: &str) -> usize {
        self.ops.iter().filter(|o| o.ok && o.name.starts_with(prefix)).count()
    }
    pub fn failures(&self) -> Vec<&OpLog> {
        self.ops.iter().filter(|o| !o.ok).collect()
    }
}

pub type Hook<'a> = Box<dyn FnMut(&TestEnv, &str) -> Result<(), String> + 'a>;

pub struct Crank<'a> {
    pub m: &'a Marinade,
    pub report: CrankReport,
    pub strict: bool,
    pub hook: Option<Hook<'a>>,
}

impl<'a> Crank<'a> {
    pub fn new(m: &'a Marinade) -> Self {
        Crank { m, report: CrankReport::default(), strict: true, hook: None }
    }
    pub fn lenient(mut self) -> Self {
        self.strict = false;
        self
    }
    pub fn with_hook(mut self, h: impl FnMut(&TestEnv, &str) -> Result<(), String> + 'a) -> Self {
        self.hook = Some(Box::new(h));
        self
    }

    /// run one op; returns Ok(Some(v)) on success, Ok(None) on a recorded (non strict) failure.
    fn run<R>(&mut self, env: &mut TestEnv, name: String, f: impl FnOnce(&mut TestEnv) -> Result<R, TxFailure>) -> Result<Option<R>> {
        let r = f(env);
        match r {
            Ok(v) => {
                self.report.ops.push(OpLog { name: name.clone(), ok: true, marinade_code: None, error: None });
                if let Some(h) = self.hook.as_mut() {
                    h(env, &name).map_err(|e| anyhow!("invariant violated after {name}: {e}"))?;
                }
                Ok(Some(v))
            }
            Err(e) => {
                self.report.ops.push(OpLog { name: name.clone(), ok: false, marinade_code: e.marinade_code(), error: Some(e.to_string()) });
                if self.strict {
                    Err(anyhow!("crank op {name} failed: {e}"))
                } else {
                    Ok(None)
                }
            }
        }
    }

    // ---------------------------------------------------------------- update

    /// update_active / update_deactivated on every tracked stake (indices shift on removal, so rescan).
    pub fn update_all(&mut self, env: &mut TestEnv) -> Result<()> {
        let mut done: HashSet<Address> = HashSet::new();
        loop {
            let m = self.m;
            let recs = m.indexed_stakes(env);
            let mut next = None;
            for r in recs {
                let addr = to_addr(&r.record.stake_account);
                if done.contains(&addr) {
                    continue;
                }
                let Some(v) = env.stake_view(&addr) else {
                    done.insert(addr);
                    continue;
                };
                let Some(d) = v.delegation.clone() else {
                    done.insert(addr);
                    continue;
                };
                let Some(vi) = m.validator_index(env, &d.voter) else {
                    done.insert(addr);
                    continue;
                };
                let kind = if d.deactivation_epoch != u64::MAX {
                    match env.activation(&d) {
                        Activation::Deactivated => 'd',
                        _ => 'x',
                    }
                } else {
                    'a'
                };
                if kind == 'x' {
                    done.insert(addr);
                    continue;
                }
                next = Some((addr, r.index, vi, kind));
                break;
            }
            let Some((addr, si, vi, kind)) = next else { return Ok(()) };
            done.insert(addr);
            if kind == 'a' {
                self.run(env, format!("update_active[{si}]"), |e| m.update_active(e, si, vi))?;
            } else {
                self.run(env, format!("update_deactivated[{si}]"), |e| m.update_deactivated(e, si, vi))?;
            }
        }
    }

    fn ensure_updated(&mut self, env: &mut TestEnv, addr: &Address) -> Result<bool> {
        let m = self.m;
        let Some(si) = m.stake_index(env, addr) else { return Ok(false) };
        let rec = &m.stake_records(env)[si as usize];
        let Some(v) = env.stake_view(addr) else { return Ok(false) };
        let Some(d) = v.delegation.clone() else { return Ok(false) };
        // a deactivating stake (e.g. the canonical stake after a partial/emergency unstake) is never "updated" via update_active
        if d.deactivation_epoch != u64::MAX || rec.last_update_status != marinade_finance::state::stake_system::StakeStatus::Active {
            return Ok(false);
        }
        let rent = env.stake_rent();
        if rec.last_update_delegated_lamports == d.stake && v.lamports == d.stake + rent {
            return Ok(true);
        }
        let Some(vi) = m.validator_index(env, &d.voter) else { return Ok(false) };
        Ok(self.run(env, format!("update_active[{si}]"), |e| m.update_active(e, si, vi))?.is_some())
    }

    // ---------------------------------------------------------------- canonical + merge

    /// credits + kind that the stake program needs to be equal to merge (like marcrank's mergeable_kind)
    fn mergeable(env: &TestEnv, v: &StakeView) -> Option<(Activation, u64)> {
        let d = v.delegation.as_ref()?;
        if d.deactivation_epoch != u64::MAX {
            return None;
        }
        match env.activation(d) {
            Activation::Activating => Some((Activation::Activating, d.credits_observed)),
            Activation::FullyActive => Some((Activation::FullyActive, d.credits_observed)),
            _ => None,
        }
    }

    pub fn canonical_and_merge(&mut self, env: &mut TestEnv) -> Result<()> {
        let m = self.m;
        let vals = m.validator_records(env);
        for (vi, vrec) in vals.iter().enumerate() {
            let vi = vi as u32;
            let vote = to_addr(&vrec.validator_account);
            let canonical = m.canonical_stake(&vote);
            // create canonical from a fully active, non-emergency stake
            if m.stake_index(env, &canonical).is_none() {
                let mut source = None;
                for r in m.indexed_stakes(env) {
                    let addr = to_addr(&r.record.stake_account);
                    if r.record.is_emergency_unstaking || r.record.last_update_status != marinade_finance::state::stake_system::StakeStatus::Active {
                        continue;
                    }
                    if let Some(v) = env.stake_view(&addr) {
                        if let Some(d) = &v.delegation {
                            if d.voter == vote && d.deactivation_epoch == u64::MAX && env.activation(d) == Activation::FullyActive {
                                source = Some(addr);
                                break;
                            }
                        }
                    }
                }
                if let Some(src) = source {
                    if self.ensure_updated(env, &src)? {
                        let si = m.stake_index(env, &src).unwrap();
                        self.run(env, format!("create_canonical_stake[{si}]"), |e| m.create_canonical_stake(e, si, vi))?;
                    }
                }
            }
            // merge everything mergeable into the canonical stake
            let mut skip: HashSet<Address> = HashSet::new();
            loop {
                let Some(ci) = m.stake_index(env, &canonical) else { break };
                if !self.ensure_updated(env, &canonical)? {
                    break;
                }
                let Some(cv) = env.stake_view(&canonical) else { break };
                let Some(ck) = Self::mergeable(env, &cv) else { break };
                let _ = ci;
                let mut pick = None;
                for r in m.indexed_stakes(env) {
                    let addr = to_addr(&r.record.stake_account);
                    if addr == canonical || skip.contains(&addr) || r.record.is_emergency_unstaking {
                        continue;
                    }
                    if r.record.last_update_status != marinade_finance::state::stake_system::StakeStatus::Active {
                        continue;
                    }
                    let Some(v) = env.stake_view(&addr) else { continue };
                    if v.delegation.as_ref().map(|d| d.voter) != Some(vote) {
                        continue;
                    }
                    if Self::mergeable(env, &v) == Some(ck) {
                        pick = Some(addr);
                        break;
                    }
                }
                let Some(src) = pick else { break };
                skip.insert(src);
                if !self.ensure_updated(env, &src)? {
                    continue;
                }
                let ci = m.stake_index(env, &canonical).unwrap();
                let si = m.stake_index(env, &src).unwrap();
                self.run(env, format!("merge_stakes[{ci}<-{si}]"), |e| m.merge_stakes(e, ci, si, vi))?;
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- stake delta

    /// Warp into the stake-delta window if needed, then stake the reserve or unstake for tickets.
    pub fn stake_delta(&mut self, env: &mut TestEnv) -> Result<()> {
        let m = self.m;
        env.warp_to_stake_window(m.slots_for_stake_delta);
        let st = m.state(env);
        let delta = st.stake_delta(env.lamports(&m.reserve));
        let nvals = st.validator_system.validator_count();
        if delta > 0 && delta >= st.stake_system.min_stake as i128 {
            if st.stake_system.extra_stake_delta_runs < nvals {
                self.run(env, "config_validator_system".into(), |e| m.config_validator_system(e, nvals))?;
            }
            for pass in 0..2 {
                for vi in 0..nvals {
                    let st = m.state(env);
                    if st.stake_delta(env.lamports(&m.reserve)) < st.stake_system.min_stake as i128 {
                        return Ok(());
                    }
                    if m.validator_records(env)[vi as usize].score == 0 {
                        continue;
                    }
                    self.run(env, format!("stake_reserve[v{vi},pass{pass}]"), |e| m.stake_reserve(e, vi))?;
                }
            }
        } else if delta < 0 {
            let mut tried: HashSet<Address> = HashSet::new();
            loop {
                let st = m.state(env);
                if st.stake_delta(env.lamports(&m.reserve)) >= 0 {
                    break;
                }
                let mut pick = None;
                for r in m.indexed_stakes(env) {
                    let addr = to_addr(&r.record.stake_account);
                    if tried.contains(&addr)
                        || r.record.is_emergency_unstaking
                        || r.record.last_update_status != marinade_finance::state::stake_system::StakeStatus::Active
                    {
                        continue;
                    }
                    pick = Some((r.index, addr, r.record.last_update_delegated_lamports));
                    break;
                }
                let Some((si, addr, _)) = pick else { break };
                tried.insert(addr);
                if !self.ensure_updated(env, &addr)? {
                    continue;
                }
                let Some(v) = env.stake_view(&addr) else { continue };
                let Some(vi) = v.delegation.as_ref().and_then(|d| m.validator_index(env, &d.voter)) else { continue };
                self.run(env, format!("deactivate_stake[{si}]"), |e| m.deactivate_stake(e, si, vi))?;
            }
        }
        Ok(())
    }

    /// Start of an epoch: update everything, create canonical stakes, merge.
    pub fn start_of_epoch(&mut self, env: &mut TestEnv) -> Result<()> {
        self.m.operational_top_up(env); // marcrank runs top-up-stake-rent first (HARNESS_RENT_TOPUP=1)
        self.update_all(env)?;
        self.canonical_and_merge(env)?;
        Ok(())
    }

    /// marcrank do-work over the epoch: start-of-epoch work, then the stake-delta window (warps inside the epoch).
    pub fn do_work(&mut self, env: &mut TestEnv) -> Result<()> {
        self.m.operational_top_up(env);
        self.start_of_epoch(env)?;
        self.stake_delta(env)?;
        Ok(())
    }
}

/// Convenience: warp to the next epoch and run the full do-work cycle.
pub fn next_epoch_work(env: &mut TestEnv, c: &mut Crank) -> Result<()> {
    env.warp_epochs(1);
    c.do_work(env)
}
