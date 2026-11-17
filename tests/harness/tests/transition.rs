//! Long lifecycle: build state on BASE over several epochs, switch the SAME svm to TARGET at an epoch
//! boundary, then run the full crank cycle and the user flows on top of state created before the switch.
//! With BASE == TARGET it is a plain long-lifecycle test.
//!
//! Regression recipe (SIMD-0437 rent drop, see ARCHITECTURE.md):
//!   HARNESS_BASE='builtin,-set_lamports_per_byte_to_6333,-set_lamports_per_byte_to_5080' HARNESS_TARGET=builtin
//! HARNESS_LENIENT=1 keeps going after a failing step and lists every failure at the end.
mod common;
use common::Fixture;
use harness::convert::to_addr;
use harness::env::sol;
use harness::profile;

fn lifecycle(f: &mut Fixture, target: &profile::Runtime) -> anyhow::Result<()> {
    let (v1, vi1) = f.validators[0];
    let (v2, vi2) = f.validators[1];
    let _ = (vi1, vi2);

    // ---- E0: liquidity, first deposit, first stake accounts (on BASE)
    let r = f.m.add_liquidity(&mut f.env, &f.users[1], sol(300));
    f.step("E0 add_liquidity", r);
    f.deposit_and_stake(0, sol(600))?;
    assert_eq!(f.m.indexed_stakes(&f.env).len(), 2, "E0: one account per validator");

    // ---- E1: update, second deposit -> second account per validator
    f.env.warp_epochs(1);
    f.crank(|c, env| c.update_all(env))?;
    f.deposit_and_stake(0, sol(400))?;
    assert_eq!(f.m.indexed_stakes(&f.env).len(), 4, "E1: extra account per validator");

    // ---- E2: rewards, canonical stake for v1 (merges for B stay pending), pending delayed unstake
    f.env.warp_epochs(1);
    for s in f.m.indexed_stakes(&f.env) {
        f.env.reward_stake(&to_addr(&s.record.stake_account), sol(2));
    }
    f.crank(|c, env| c.update_all(env))?;
    let first_v1 = f
        .m
        .indexed_stakes(&f.env)
        .into_iter()
        .find(|s| f.env.stake_view(&to_addr(&s.record.stake_account)).unwrap().delegation.unwrap().voter == v1)
        .unwrap();
    let r = f.m.create_canonical_stake(&mut f.env, first_v1.index, vi1);
    f.step("E2 create_canonical_stake(v1)", r);
    let msol = f.env.token_balance(&f.users[0].msol);
    let ticket = f.m.order_unstake(&mut f.env, &f.users[0], msol / 5);
    let ticket = f.step("E2 order_unstake", ticket);
    f.crank(|c, env| c.stake_delta(env))?; // deactivate_stake: cooling-down accounts created on BASE
    let cooling = f.m.state(&f.env).stake_system.delayed_unstake_cooling_down;
    assert!(cooling > 0 || f.lenient, "E2: a deactivation was expected");

    // ---- epoch boundary: the cluster switches runtime
    f.env.warp_epochs(1);
    let before_rent = f.env.stake_rent();
    f.env.transition(target)?;
    eprintln!("[transition] stake rent {} -> {}; stake program {:?}", before_rent, f.env.stake_rent(), f.env.program_sha256(&profile::addr(profile::STAKE_ID)));
    f.check("transition");

    // ---- E3 (TARGET): update everything, claim the old ticket, stake again under the new rent
    f.crank(|c, env| c.update_all(env))?;
    if let Some(t) = ticket {
        let r = f.m.claim(&mut f.env, &t, &f.users[0].pubkey());
        f.step("E3 claim(pre-transition ticket)", r);
    }
    f.deposit_and_stake(2, sol(300))?;
    let n_new = f.m.indexed_stakes(&f.env).len();
    eprintln!("[transition] stake accounts after E3 stake_delta: {n_new}");

    // ---- E4: canonical for v2 from a post-transition account (old merges INTO new), then merge everything
    f.env.warp_epochs(1);
    f.crank(|c, env| c.update_all(env))?;
    let new_v2 = f.m.indexed_stakes(&f.env).into_iter().rev().find(|s| {
        let v = f.env.stake_view(&to_addr(&s.record.stake_account)).unwrap();
        v.delegation.map(|d| d.voter == v2).unwrap_or(false)
            && v.lamports < sol(1000) // keep it simple: any v2 account created last
    });
    if f.m.stake_index(&f.env, &f.m.canonical_stake(&v2)).is_none() {
        if let Some(s) = new_v2 {
            let r = f.m.create_canonical_stake(&mut f.env, s.index, vi2);
            f.step("E4 create_canonical_stake(v2 from newest account)", r);
        }
    }
    let (_, rep) = f.crank(|c, env| c.canonical_and_merge(env))?;
    eprintln!("[transition] E4 merges: {}", rep.count("merge_stakes"));
    assert!(rep.count("merge_stakes") >= 1 || f.lenient, "{rep:?}");

    // user stake account created AFTER the transition, then deposited
    let stake = f.env.create_delegated_stake(&f.users[2].key, &v1, sol(40))?;
    let r = f.m.deposit_stake_account(&mut f.env, &f.users[2], &stake, vi1);
    f.step("E4 deposit_stake_account(post-transition user stake)", r);

    // delayed unstake on post-transition state
    let msol = f.env.token_balance(&f.users[2].msol);
    let ticket2 = f.m.order_unstake(&mut f.env, &f.users[2], msol / 2);
    let ticket2 = f.step("E4 order_unstake", ticket2);
    f.crank(|c, env| c.stake_delta(env))?;

    // ---- E5: update_deactivated on post-transition accounts, claim, final consistency
    f.env.warp_epochs(1);
    f.crank(|c, env| c.update_all(env))?;
    if let Some(t) = ticket2 {
        let r = f.m.claim(&mut f.env, &t, &f.users[2].pubkey());
        f.step("E5 claim(post-transition ticket)", r);
    }
    f.crank(|c, env| c.canonical_and_merge(env))?;
    let viol = harness::invariants::check_fully_updated(&f.env, &f.m);
    if !viol.is_empty() {
        f.findings.push(format!("not fully updated: {viol:?}"));
    }
    f.finish();
    Ok(())
}

#[test]
fn lifecycle_base_to_target() {
    let base = profile::base_spec();
    let target = profile::target_spec();
    let target_rt = profile::resolve(&target).unwrap();
    let sha = profile::marinade_so(&profile::resolve(&base).unwrap()).map(|b| b.sha256).unwrap_or_default();
    harness::report::run_reported("transition::lifecycle_base_to_target", &base, &target, &sha, || {
        let mut f = common::fixture(&base, 2, 3)?;
        lifecycle(&mut f, &target_rt)
    });
}
