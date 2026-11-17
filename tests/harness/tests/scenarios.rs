//! One instruction family per test, run on HARNESS_TARGET (default: HARNESS_BASE, else mainnet profile/builtin).
//! Invariants (invariants.rs) are checked after every op.
mod common;
use common::scenario;
use harness::KeypairExt;
use harness::convert::to_addr;
use harness::env::{sol, Activation};
use marinade_finance::instructions::admin::ConfigLpParams;
use marinade_finance::state::fee::Fee;

#[test]
fn deposit_and_liquid_unstake() {
    scenario("deposit_and_liquid_unstake", 1, |f| {
        // liquidity first, otherwise there is nothing to unstake into
        let r = f.m.add_liquidity(&mut f.env, &f.users[1], sol(500));
        f.ok("add_liquidity", r);
        let r = f.m.deposit(&mut f.env, &f.users[0], sol(100));
        f.ok("deposit", r);
        let msol = f.env.token_balance(&f.users[0].msol);
        assert!(msol > sol(99), "msol {msol}");
        let sol_before = f.env.lamports(&f.users[0].pubkey());
        let r = f.m.liquid_unstake(&mut f.env, &f.users[0], sol(10));
        f.ok("liquid_unstake", r);
        let got = f.env.lamports(&f.users[0].pubkey()) - sol_before;
        assert!(got < sol(10) && got > sol(9), "fee applied, got {got}");
        assert!(f.env.token_balance(&f.m.treasury_msol) > 0, "treasury cut");
        // after the liq pool got msol, a new deposit buys it back from the pool (swap path)
        let r = f.m.deposit(&mut f.env, &f.users[2], sol(5));
        f.ok("deposit_swap", r);
        Ok(())
    });
}

#[test]
fn add_and_remove_liquidity() {
    scenario("add_and_remove_liquidity", 1, |f| {
        let r = f.m.add_liquidity(&mut f.env, &f.users[0], sol(200));
        f.ok("add_liquidity", r);
        let lp = f.env.token_balance(&f.users[0].lp);
        assert!(lp > 0);
        let r = f.m.remove_liquidity(&mut f.env, &f.users[0], lp / 2);
        f.ok("remove_liquidity", r);
        let all = f.env.token_balance(&f.users[0].lp);
        let r = f.m.remove_liquidity(&mut f.env, &f.users[0], all);
        f.ok("remove_liquidity_all", r);
        assert_eq!(f.env.token_balance(&f.users[0].lp), 0);
        let r = f.m.config_lp(&mut f.env, ConfigLpParams { max_fee: Some(Fee::from_basis_points(400)), ..Default::default() });
        f.ok("config_lp", r);
        Ok(())
    });
}

#[test]
fn stake_reserve_rewards_update_active() {
    scenario("stake_reserve_rewards_update_active", 2, |f| {
        f.deposit_and_stake(0, sol(1000))?;
        let stakes = f.m.indexed_stakes(&f.env);
        assert_eq!(stakes.len(), 2, "one stake per validator");
        let st = f.m.state(&f.env);
        assert!(st.validator_system.total_active_balance > sol(990));
        let price0 = f.m.msol_price(&f.env);
        f.env.warp_epochs(1);
        for s in &stakes {
            f.env.reward_stake(&to_addr(&s.record.stake_account), sol(2));
        }
        f.env.add_mev(&to_addr(&stakes[0].record.stake_account), sol(1));
        let treasury0 = f.env.token_balance(&f.m.treasury_msol);
        f.crank(|c, env| c.update_all(env))?;
        let price1 = f.m.msol_price(&f.env);
        assert!(price1 > price0, "price {price0} -> {price1}");
        assert!(f.env.token_balance(&f.m.treasury_msol) > treasury0, "protocol fee minted to treasury");
        let viol = harness::invariants::check_fully_updated(&f.env, &f.m);
        assert!(viol.is_empty(), "{viol:?}");
        Ok(())
    });
}

/// Two accounts per validator over two epochs, then canonical + merge.
fn two_accounts_per_validator(f: &mut common::Fixture) -> anyhow::Result<()> {
    f.deposit_and_stake(0, sol(600))?;
    f.env.warp_epochs(1);
    f.crank(|c, env| c.update_all(env))?;
    f.deposit_and_stake(1, sol(400))?;
    f.env.warp_epochs(1);
    assert_eq!(f.m.indexed_stakes(&f.env).len(), 4);
    Ok(())
}

#[test]
fn merge_stakes() {
    scenario("merge_stakes", 2, |f| {
        two_accounts_per_validator(f)?;
        for s in f.m.indexed_stakes(&f.env) {
            f.env.reward_stake(&to_addr(&s.record.stake_account), sol(1));
        }
        let (_, rep) = f.crank(|c, env| c.start_of_epoch(env))?;
        assert_eq!(rep.count("create_canonical_stake"), 2, "{rep:?}");
        assert_eq!(rep.count("merge_stakes"), 2, "{rep:?}");
        assert_eq!(f.m.indexed_stakes(&f.env).len(), 2);
        let viol = harness::invariants::check_fully_updated(&f.env, &f.m);
        assert!(viol.is_empty(), "{viol:?}");
        Ok(())
    });
}

#[test]
fn order_unstake_deactivate_update_deactivated_claim() {
    scenario("delayed_unstake", 2, |f| {
        f.deposit_and_stake(0, sol(1000))?;
        f.env.warp_epochs(1);
        f.crank(|c, env| c.update_all(env))?;
        let msol = f.env.token_balance(&f.users[0].msol);
        let ticket = f.m.order_unstake(&mut f.env, &f.users[0], msol / 4);
        let ticket = f.ok("order_unstake", ticket);
        let t = f.m.ticket(&f.env, &ticket).unwrap();
        assert!(t.lamports_amount > sol(240));
        // too early
        let r = f.m.claim(&mut f.env, &ticket, &f.users[0].pubkey());
        assert!(r.is_err());
        f.crank(|c, env| c.stake_delta(env))?;
        assert!(f.m.state(&f.env).stake_system.delayed_unstake_cooling_down > 0, "deactivate_stake ran");
        f.env.warp_epochs(1);
        let (_, rep) = f.crank(|c, env| c.update_all(env))?;
        assert!(rep.count("update_deactivated") > 0, "{rep:?}");
        let before = f.env.lamports(&f.users[0].pubkey());
        let r = f.m.claim(&mut f.env, &ticket, &f.users[0].pubkey());
        f.ok("claim", r);
        assert!(f.env.lamports(&f.users[0].pubkey()) >= before + t.lamports_amount);
        assert_eq!(f.m.state(&f.env).circulating_ticket_count, 0);
        Ok(())
    });
}

#[test]
fn deposit_stake_account() {
    scenario("deposit_stake_account", 2, |f| {
        f.deposit_and_stake(0, sol(100))?;
        let (vote, vi) = f.validators[0];
        let user_key = harness::Keypair::new();
        let _ = user_key;
        let stake = {
            let u = &f.users[1];
            f.env.create_delegated_stake(&u.key, &vote, sol(50))?
        };
        let before = f.env.token_balance(&f.users[1].msol);
        let r = f.m.deposit_stake_account(&mut f.env, &f.users[1], &stake, vi);
        f.ok("deposit_stake_account", r);
        let minted = f.env.token_balance(&f.users[1].msol) - before;
        assert!(minted > sol(49) && minted < sol(50) + 1, "minted {minted}");
        let v = f.env.stake_view(&stake).unwrap();
        assert_eq!(v.staker, f.m.deposit_auth);
        assert_eq!(v.withdrawer, f.m.withdraw_auth);
        assert!(f.m.stake_index(&f.env, &stake).is_some());
        Ok(())
    });
}

#[test]
fn withdraw_stake_account() {
    scenario("withdraw_stake_account", 2, |f| {
        f.deposit_and_stake(0, sol(1000))?;
        f.env.warp_epochs(1);
        f.crank(|c, env| c.update_all(env))?;
        let stakes = f.m.indexed_stakes(&f.env);
        let s0 = &stakes[0];
        let vote = s0.record.stake_account;
        let _ = vote;
        let v = f.env.stake_view(&to_addr(&s0.record.stake_account)).unwrap();
        let vi = f.m.validator_index(&f.env, &v.delegation.unwrap().voter).unwrap();
        let beneficiary = harness::Keypair::new();
        let msol = sol(100);
        let op = f.m.withdraw_stake_account(&mut f.env, &f.users[0], s0.index, vi, msol, &beneficiary.pubkey_addr());
        let op = f.ok("withdraw_stake_account", op);
        let split = op.new_stake.unwrap();
        let sv = f.env.stake_view(&split).unwrap();
        assert_eq!(sv.staker, beneficiary.pubkey_addr());
        assert_eq!(sv.withdrawer, beneficiary.pubkey_addr());
        assert!(sv.delegation.unwrap().stake > sol(99));
        Ok(())
    });
}

#[test]
fn partial_unstake() {
    scenario("partial_unstake", 2, |f| {
        f.deposit_and_stake(0, sol(1000))?;
        f.env.warp_epochs(1);
        f.crank(|c, env| c.update_all(env))?;
        // make validator 0 over-staked relative to its score
        let (vote0, vi0) = f.validators[0];
        let r = f.m.set_validator_score(&mut f.env, vi0, &vote0, 20);
        f.ok("set_validator_score", r);
        let stake = f.m.indexed_stakes(&f.env).into_iter().find(|s| {
            f.env.stake_view(&to_addr(&s.record.stake_account)).unwrap().delegation.unwrap().voter == vote0
        });
        let stake = stake.unwrap();
        let op = f.m.partial_unstake(&mut f.env, stake.index, vi0, sol(100));
        let op = f.ok("partial_unstake", op);
        assert!(op.new_stake.is_some(), "split created");
        assert!(f.m.state(&f.env).emergency_cooling_down >= sol(100));
        Ok(())
    });
}

#[test]
fn emergency_unstake() {
    scenario("emergency_unstake", 2, |f| {
        f.deposit_and_stake(0, sol(1000))?;
        f.env.warp_epochs(1);
        f.crank(|c, env| c.update_all(env))?;
        let (vote0, vi0) = f.validators[0];
        // requires score 0 first
        let stake = f.m.indexed_stakes(&f.env).into_iter().find(|s| {
            f.env.stake_view(&to_addr(&s.record.stake_account)).unwrap().delegation.unwrap().voter == vote0
        }).unwrap();
        let r = f.m.emergency_unstake(&mut f.env, stake.index, vi0);
        assert!(r.is_err(), "non-zero score must be refused");
        let r = f.m.set_validator_score(&mut f.env, vi0, &vote0, 0);
        f.ok("set_validator_score", r);
        let r = f.m.emergency_unstake(&mut f.env, stake.index, vi0);
        f.ok("emergency_unstake", r);
        assert!(f.m.state(&f.env).emergency_cooling_down > 0);
        f.env.warp_epochs(1);
        let d = f.env.stake_view(&to_addr(&stake.record.stake_account)).unwrap().delegation.unwrap();
        assert_eq!(f.env.activation(&d), Activation::Deactivated);
        let (_, rep) = f.crank(|c, env| c.update_all(env))?;
        assert!(rep.count("update_deactivated") >= 1);
        assert_eq!(f.m.state(&f.env).emergency_cooling_down, 0);
        Ok(())
    });
}
