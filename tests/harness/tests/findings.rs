//! Minimal standalone repros of program behaviours found by the stateful fuzzer (`tests/fuzz.rs`).
//!
//! Every test asserts the CORRECT behaviour, so each one FAILS on the current program and is `#[ignore]`d (run them with
//! `cargo test --test findings -- --ignored`). When the program is fixed, remove the `#[ignore]`.
//! The fuzzer treats each of these as a known, counted outcome (`KNOWN BUG` / `known limit` in its coverage output).
mod common;
use common::fixture;
use harness::convert::to_addr;
use harness::env::sol;
use harness::profile;
use marinade_finance::instructions::admin::ConfigMarinadeParams;

/// FINDING 1 (low/medium): anyone can make `stake_reserve` panic by sending lamports to the reserve PDA.
///
/// `stake_reserve` (crank/stake_reserve.rs) sizes the stake from the REAL reserve balance (`stake_delta(reserve_pda.lamports())`),
/// transfers `stake_target` out of the reserve and then books it with `on_transfer_from_reserve(stake_target)`
/// (state/mod.rs: `available_reserve_balance -= amount`), but `available_reserve_balance` is only the VIRTUAL balance: it does not
/// include lamports sent directly to the PDA until some `update_active`/`update_deactivated` re-syncs it (`UpdateCommon::begin`).
/// When the instruction stakes the whole delta (the "take the remainder" branch, i.e. whenever the rest is below `min_stake`),
/// `stake_target = available + donation > available` and the subtraction overflows (`attempt to subtract with overflow`,
/// state/mod.rs:281). `claim` has the same shape (`on_transfer_from_reserve(lamports)` after checking the REAL balance).
/// Impact: a 1-lamport transfer makes stake_reserve revert (as a panic) until the next update_* call re-syncs; permissionless griefing
/// of staking/claims, no loss of funds. Shrunk fuzz sequence (cold genesis): `Deposit(a0, 1 SOL); Donate(Reserve, 1 lamport)` -> crank.
/// Fix: size by `available_reserve_balance` (or re-sync first), and use saturating/checked arithmetic.
#[test]
#[ignore = "known program bug: reserve donation makes stake_reserve panic"]
fn reserve_donation_makes_stake_reserve_panic() {
    let mut f = fixture(&profile::target_spec(), 3, 1).unwrap();
    f.m.deposit(&mut f.env, &f.users[0], sol(1)).unwrap();
    f.env.fund(&f.m.reserve, 1); // a plain system transfer of 1 lamport to the reserve PDA
    let n = f.m.slots_for_stake_delta;
    f.env.warp_to_stake_window(n);
    let r = f.m.stake_reserve(&mut f.env, 0);
    assert!(r.as_ref().map(|_| true).unwrap_or_else(|e| !e.is_panic()), "stake_reserve panicked: {}", r.unwrap_err());
}

/// FINDING 2 (low): with `min_withdraw = 0` (allowed: only an upper bound is enforced) `order_unstake` accepts an order worth 0
/// lamports (e.g. 0 mSOL). It creates a ticket with `lamports_amount == 0` and increments `circulating_ticket_count`, but such a
/// ticket can never be claimed (`claim` rejects it with ReusingDelayedUnstakeTicket, so it is never closed): the count drifts forever.
/// Fix: require a non-zero payout in order_unstake (or let claim close zero tickets).
#[test]
#[ignore = "known program bug: zero-value ticket is created and can never be claimed"]
fn order_unstake_creates_unclaimable_zero_ticket() {
    let mut f = fixture(&profile::target_spec(), 1, 1).unwrap();
    let cfg = ConfigMarinadeParams { min_withdraw: Some(0), ..Default::default() };
    f.m.config_marinade(&mut f.env, cfg).unwrap();
    f.m.deposit(&mut f.env, &f.users[0], sol(10)).unwrap();
    let before = f.m.state(&f.env).circulating_ticket_count;
    let r = f.m.order_unstake(&mut f.env, &f.users[0], 0);
    assert!(r.is_err(), "an order worth 0 lamports must be rejected, but a ticket was created");
    assert_eq!(f.m.state(&f.env).circulating_ticket_count, before);
}

/// FINDING 3 (low, liveness): `deactivate_stake` reverts when the amount it must unstake is smaller than the stake program's minimum
/// delegation (1 SOL on the mainnet runtime). The partial branch splits `min(unstake_from_validator, total_unstake_delta)`
/// lamports without checking it against the minimum delegation (it only compares the REMAINDER with `2 * min_stake`), so the
/// stake program's Split fails with InsufficientDelegation (Custom 12). A delayed-unstake ticket smaller than 1 SOL (or a validator
/// whose surplus over its target is < 1 SOL) therefore cannot be served by deactivation until deposits/rewards refill the reserve.
/// The other branches of this function return Ok(()) as a no-op for conditions the bot cannot act on; this one hard-fails.
/// Shrunk fuzz sequence: warm genesis; `OrderUnstake(a0, 0.1%)`; crank (do_work) -> deactivate_stake: Custom(12).
#[test]
#[ignore = "known program limit: deactivate_stake split below the stake program minimum delegation reverts"]
fn deactivate_stake_split_below_minimum_delegation_reverts() {
    let mut f = fixture(&profile::target_spec(), 2, 2).unwrap();
    f.deposit_and_stake(0, sol(1000)).unwrap();
    f.env.warp_epochs(1);
    f.crank(|c, env| c.update_all(env)).unwrap();
    // a 0.1 SOL delayed unstake: far below the stake program's minimum delegation
    let msol = sol(1) / 10;
    f.m.order_unstake(&mut f.env, &f.users[0], msol).unwrap();
    let n = f.m.slots_for_stake_delta;
    f.env.warp_to_stake_window(n);
    let stakes = f.m.indexed_stakes(&f.env);
    let s = &stakes[0];
    let v = f.env.stake_view(&to_addr(&s.record.stake_account)).unwrap();
    let vi = f.m.validator_index(&f.env, &v.delegation.unwrap().voter).unwrap();
    let r = f.m.deactivate_stake(&mut f.env, s.index, vi);
    assert!(r.is_ok(), "deactivate_stake reverted: {}", r.unwrap_err());
}

/// FINDING 4 (low, liveness): `stake_reserve` over-stakes when `delayed_unstake_cooling_down > circulating_ticket_balance`.
/// `stake_delta` counts the cooling-down SOL as if it were already in the reserve (`raw = reserve + cooling - tickets`), but
/// `stake_reserve` then transfers `delta`-sized chunks out of the reserve. If a ticket was paid from fresh deposits (so the reserve
/// holds SOL but the deactivated stake has not been `update_deactivated`) the transfer fails with a system InsufficientFunds
/// (Custom 1). It resolves once update_deactivated moved the cooling SOL into the reserve, so bots that update first never see it.
/// Shrunk fuzz sequence (warm genesis, ticket pending): `Deposit(122 SOL); Warp(1); Claim(t0); StakeReserve(window)`.
#[test]
#[ignore = "known program limit: stake_reserve reverts while cooling-down SOL is counted as available"]
fn stake_reserve_reverts_when_cooling_down_exceeds_tickets() {
    let mut f = fixture(&profile::target_spec(), 2, 3).unwrap();
    f.deposit_and_stake(0, sol(1000)).unwrap();
    f.env.warp_epochs(1);
    f.crank(|c, env| c.update_all(env)).unwrap();
    let msol = f.env.token_balance(&f.users[0].msol);
    let t = f.m.order_unstake(&mut f.env, &f.users[0], msol / 5).unwrap();
    f.crank(|c, env| c.stake_delta(env)).unwrap(); // deactivates ~200 SOL (cooling down)
    f.env.warp_epochs(1);
    // fresh deposits pay the ticket before the cooled-down stake was withdrawn into the reserve
    f.m.deposit(&mut f.env, &f.users[1], sol(300)).unwrap();
    f.m.claim(&mut f.env, &t, &f.users[0].pubkey()).unwrap();
    let n = f.m.slots_for_stake_delta;
    f.env.warp_to_stake_window(n);
    let r = f.m.stake_reserve(&mut f.env, 0);
    assert!(r.is_ok(), "stake_reserve reverted: {}", r.unwrap_err());
}

/// FINDING 5 (low, operational): `remove_validator` only requires `active_balance == 0`, but stake accounts of that validator that
/// are still cooling down stay in the stake list. `update_deactivated` looks the validator up in the list (`get_checked` ->
/// WrongValidatorAccountOrIndex), so the cooling SOL (and the tickets it backs) cannot be moved to the reserve until the manager
/// re-adds the same vote account. Recoverable by the manager; the fuzzer re-adds such validators before its final crank.
#[test]
#[ignore = "known program limit: removing a validator with deactivating stakes blocks update_deactivated"]
fn removed_validator_blocks_update_deactivated() {
    let mut f = fixture(&profile::target_spec(), 2, 2).unwrap();
    f.deposit_and_stake(0, sol(1000)).unwrap();
    f.env.warp_epochs(1);
    f.crank(|c, env| c.update_all(env)).unwrap();
    // drop validator 0 to score 0 and order a delayed unstake large enough that its whole stake is deactivated
    let (vote0, vi0) = f.validators[0];
    f.m.set_validator_score(&mut f.env, vi0, &vote0, 0).unwrap();
    let msol = f.env.token_balance(&f.users[0].msol);
    f.m.order_unstake(&mut f.env, &f.users[0], msol * 55 / 100).unwrap();
    f.crank(|c, env| c.stake_delta(env)).unwrap();
    let st = f.m.state(&f.env);
    assert!(st.stake_system.delayed_unstake_cooling_down > 0);
    assert_eq!(f.m.validator_records(&f.env)[vi0 as usize].active_balance, 0, "scenario needs validator 0 fully cooling down");
    let cooling = f
        .m
        .indexed_stakes(&f.env)
        .into_iter()
        .find(|r| f.env.stake_view(&to_addr(&r.record.stake_account)).unwrap().delegation.unwrap().voter == vote0)
        .unwrap();
    // the manager removes the validator while its stake account is still cooling down
    f.m.remove_validator(&mut f.env, vi0, &vote0).unwrap();
    f.env.warp_epochs(1);
    for vi in 0..f.m.state(&f.env).validator_system.validator_count() {
        let r = f.m.update_deactivated(&mut f.env, cooling.index, vi);
        assert!(r.is_ok(), "update_deactivated of a cooled-down stake failed after its validator was removed (validator index {vi}): {}", r.unwrap_err());
    }
}
