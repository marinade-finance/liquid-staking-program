//! Accounting / authority invariants, evaluated natively on deserialized program state after every
//! successful transaction. Relations follow tests/harness/src/invariants.rs (same program code paths).

use crate::util::*;
use crate::world::World;
use anchor_lang::solana_program::stake::state::StakeStateV2;
use marinade_finance::state::stake_system::StakeStatus;
use marinade_finance::State;

#[derive(Clone)]
pub struct Snap {
    pub st: State,
    pub tvs: u64,
    pub supply: u64,
    pub reserve: u64,
}

pub fn take(w: &World, t: &mut Trident) -> Option<Snap> {
    let st = w.state(t, &w.a)?;
    let reserve = lam(t, &w.a.reserve);
    Some(Snap { tvs: st.total_virtual_staked_lamports(), supply: st.msol_supply, reserve, st })
}

/// Returns the list of violated invariants (empty = fine).
pub fn check(w: &mut World, t: &mut Trident, ix: &str, pre: Option<&Snap>) -> Vec<String> {
    let mut v = vec![];
    let a = w.a.clone();
    let Some(st) = w.state(t, &a) else {
        v.push("state of A no longer deserializes".into());
        return v;
    };
    let r = st.rent_exempt_for_token_acc;

    // 1. mSOL supply
    match read_mint(t, &a.msol_mint) {
        Some(m) if m.supply != st.msol_supply => v.push(format!("msol supply: mint {} != state {}", m.supply, st.msol_supply)),
        None => v.push("msol mint unreadable".into()),
        _ => {}
    }
    // 2. LP supply
    match read_mint(t, &a.lp_mint) {
        Some(m) if m.supply != st.liq_pool.lp_supply => v.push(format!("lp supply: mint {} != state {}", m.supply, st.liq_pool.lp_supply)),
        None => v.push("lp mint unreadable".into()),
        _ => {}
    }
    // 3. reserve
    let reserve = lam(t, &a.reserve);
    let want = st.available_reserve_balance as u128 + r as u128;
    if w.donated {
        if (reserve as u128) < want {
            v.push(format!("reserve {} < available {} + rent {}", reserve, st.available_reserve_balance, r));
        }
    } else if reserve as u128 != want {
        v.push(format!("reserve lamports {} != available_reserve_balance {} + rent {}", reserve, st.available_reserve_balance, r));
    }
    // 4. sol leg
    let sol_leg = lam(t, &a.sol_leg);
    if sol_leg < r {
        v.push(format!("sol leg {sol_leg} below rent {r}"));
    }
    // 5. validator sums
    let vals = w.validators(t, &a);
    if vals.len() as u32 != st.validator_system.validator_count() {
        v.push("validator list shorter than count".into());
    }
    let sum_active: u128 = vals.iter().map(|x| x.active_balance as u128).sum();
    if sum_active != st.validator_system.total_active_balance as u128 {
        v.push(format!("total_active_balance {} != sum {}", st.validator_system.total_active_balance, sum_active));
    }
    let sum_score: u64 = vals.iter().map(|x| x.score as u64).sum();
    if sum_score != st.validator_system.total_validator_score as u64 {
        v.push(format!("total_validator_score {} != sum {}", st.validator_system.total_validator_score, sum_score));
    }
    // 6/7. stake records vs real stake accounts
    let recs = w.stakes(t, &a);
    let mut per_val: std::collections::HashMap<Pubkey, u128> = Default::default();
    let (mut delayed, mut emergency) = (0u128, 0u128);
    let mut seen = std::collections::HashSet::new();
    let mut stake_surplus: i128 = 0;
    for (i, rec) in recs.iter().enumerate() {
        if !seen.insert(rec.stake_account) {
            v.push(format!("stake list: duplicate account {}", rec.stake_account));
        }
        let acc = t.get_account(&rec.stake_account);
        if *acc.owner() != stake_id() {
            v.push(format!("stake #{i} {} not owned by stake program", rec.stake_account));
            continue;
        }
        let Some(StakeStateV2::Stake(meta, stake, _)) = read_stake(t, &rec.stake_account) else {
            v.push(format!("stake #{i} {} not a delegated stake", rec.stake_account));
            continue;
        };
        if meta.authorized.staker != a.deposit_auth || meta.authorized.withdrawer != a.withdraw_auth {
            v.push(format!("stake #{i} authorities changed: staker {} withdrawer {}", meta.authorized.staker, meta.authorized.withdrawer));
        }
        let rent = stake_rent(t);
        if acc.lamports() < rent {
            v.push(format!("stake #{i} below rent: {} < {}", acc.lamports(), rent));
        }
        stake_surplus += acc.lamports() as i128 - rent as i128;
        let l = rec.last_update_delegated_lamports as u128;
        match rec.last_update_status {
            StakeStatus::Active => {
                *per_val.entry(stake.delegation.voter_pubkey).or_default() += l;
                if stake.delegation.deactivation_epoch != u64::MAX && !w.slashed {
                    v.push(format!("stake #{i} recorded Active but deactivating in the stake program"));
                }
            }
            StakeStatus::Deactivating => {
                if rec.is_emergency_unstaking {
                    emergency += l
                } else {
                    delayed += l
                }
            }
            StakeStatus::Unknown => v.push(format!("stake #{i} status Unknown")),
        }
    }
    if recs.len() as u32 != st.stake_system.stake_count() {
        v.push("stake list shorter than count".into());
    }
    for val in &vals {
        let got = per_val.remove(&val.validator_account).unwrap_or(0);
        if got != val.active_balance as u128 {
            v.push(format!("validator {} active_balance {} != sum of active stake records {}", val.validator_account, val.active_balance, got));
        }
    }
    for (k, l) in per_val {
        if l != 0 {
            v.push(format!("active stake records for unknown validator {k}: {l}"));
        }
    }
    if delayed != st.stake_system.delayed_unstake_cooling_down as u128 {
        v.push(format!("delayed_unstake_cooling_down {} != sum {}", st.stake_system.delayed_unstake_cooling_down, delayed));
    }
    if emergency != st.emergency_cooling_down as u128 {
        v.push(format!("emergency_cooling_down {} != sum {}", st.emergency_cooling_down, emergency));
    }
    // 8. tickets
    let (mut tbal, mut tcnt) = (0u128, 0u64);
    for k in w.tickets.clone() {
        if let Some(td) = w.ticket(t, &k) {
            if td.state_address == a.state && owner_of(t, &k) == program_id() {
                tbal += td.lamports_amount as u128;
                tcnt += 1;
            }
        }
    }
    if tbal != st.circulating_ticket_balance as u128 {
        v.push(format!("circulating_ticket_balance {} != sum of tickets {}", st.circulating_ticket_balance, tbal));
    }
    if tcnt != st.circulating_ticket_count {
        v.push(format!("circulating_ticket_count {} != tickets {}", st.circulating_ticket_count, tcnt));
    }
    // 10. upgrader
    if !st.delinquent_upgrader.is_done() {
        v.push("delinquent_upgrader not Done".into());
    }
    // 11. solvency
    if !w.slashed {
        let backing = (reserve.saturating_sub(r)) as i128 + stake_surplus;
        if backing < st.total_lamports_under_control() as i128 {
            v.push(format!("insolvent: reserve-R {} + stake surplus {} < total_lamports_under_control {}", reserve.saturating_sub(r), stake_surplus, st.total_lamports_under_control()));
        }
    }
    // price monotonic and identity fields
    if let Some(p) = pre {
        if !w.slashed && p.supply > 0 && st.msol_supply > 0 {
            let tvs = st.total_virtual_staked_lamports();
            // tvs_now/supply_now >= tvs_prev/supply_prev up to one lamport of rounding
            let lhs = tvs as u128 * p.supply as u128 + tvs.max(p.tvs) as u128;
            let rhs = p.tvs as u128 * st.msol_supply as u128;
            if lhs < rhs {
                v.push(format!("mSOL price decreased: {}/{} -> {}/{} (ix {ix})", p.tvs, p.supply, tvs, st.msol_supply));
            }
        }
        let o = &p.st;
        let immut = o.msol_mint == st.msol_mint
            && o.liq_pool.lp_mint == st.liq_pool.lp_mint
            && o.liq_pool.msol_leg == st.liq_pool.msol_leg
            && o.reserve_bump_seed == st.reserve_bump_seed
            && o.msol_mint_authority_bump_seed == st.msol_mint_authority_bump_seed
            && o.rent_exempt_for_token_acc == st.rent_exempt_for_token_acc
            && o.stake_system.stake_list.account == st.stake_system.stake_list.account
            && o.validator_system.validator_list.account == st.validator_system.validator_list.account;
        if !immut {
            v.push("immutable identity field of State changed".into());
        }
        if ix != "change_authority" {
            let same = o.admin_authority == st.admin_authority
                && o.validator_system.manager_authority == st.validator_system.manager_authority
                && o.pause_authority == st.pause_authority
                && o.operational_sol_account == st.operational_sol_account
                && o.treasury_msol_account == st.treasury_msol_account;
            if !same {
                v.push(format!("authority field changed by {ix}"));
            }
        }
        if ix != "pause" && ix != "resume" && o.paused != st.paused {
            v.push(format!("paused flag flipped by {ix}"));
        }
    }
    v
}
