//! Pure property tests (no SVM) of the program's arithmetic: `calc::{proportional, shares_from_value,
//! value_from_shares}`, `state::fee::{Fee, FeeCents}`, `LiqPool::linear_fee` and the `State` price helpers
//! (`total_virtual_staked_lamports`, `msol_to_sol`, `calc_msol_from_lamports`, `stake_delta`).
//!
//! Inputs cover the full u64 range (edge-biased: 0, 1, 2, powers of two, u64::MAX and neighbours, ...).
//! Documented limits (not bugs) are written down where a property needs a precondition:
//! * `proportional(_, _, 0)` returns `amount` (a zero denominator is "1:1"); `shares_from_value` with no shares is 1:1.
//! * results above u64::MAX are `CalculationFailure`, never a wrap or a panic.
//! * pool totals are assumed to sum within u64 (`tvs + value`, `supply + shares`): the program adds them with overflow
//!   checks, and the total SOL supply (~6e17 lamports) is 30x below u64::MAX.
//! * `Fee::apply` floors (fee <= exact, the user saves < 1 lamport per op); it is only exact for validated fees
//!   (`check()` in every setter, except `max_stake_moved_per_epoch`).
use anchor_lang::error::Error;
use marinade_finance::calc::{proportional, shares_from_value, value_from_shares};
use marinade_finance::error::MarinadeError as ME;
use marinade_finance::state::fee::{Fee, FeeCents};
use marinade_finance::state::liq_pool::LiqPool;
use marinade_finance::State;
use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(4000)
}

fn cfg() -> ProptestConfig {
    ProptestConfig { cases: cases(), failure_persistence: None, ..ProptestConfig::default() }
}

/// edge-biased u64
fn u() -> BoxedStrategy<u64> {
    prop_oneof![
        3 => Just(0u64),
        3 => Just(1u64),
        2 => Just(2u64),
        2 => Just(u64::MAX),
        2 => Just(u64::MAX - 1),
        3 => 0u64..1000,
        3 => (0u32..64).prop_map(|b| 1u64 << b),
        3 => (0u32..64).prop_map(|b| (1u64 << b).wrapping_sub(1)),
        3 => (0u32..64).prop_map(|b| (1u64 << b).wrapping_add(1)),
        4 => 1u64..=1_000_000_000_000_000_000,
        8 => any::<u64>(),
    ]
    .boxed()
}

/// u64 for pool totals: positive and with room to add another u64-sized value without overflowing u128 products
fn pool() -> BoxedStrategy<u64> {
    prop_oneof![
        2 => Just(1u64),
        3 => 1u64..1000,
        3 => (1u32..63).prop_map(|b| 1u64 << b),
        4 => 1_000_000u64..=700_000_000_000_000_000,
        6 => 1u64..=u64::MAX / 2,
    ]
    .boxed()
}

fn code(e: &Error) -> Option<u32> {
    match e {
        Error::AnchorError(a) => Some(a.error_code_number),
        _ => None,
    }
}

/// a*b as (hi, lo) 256-bit
fn wide_mul(a: u128, b: u128) -> (u128, u128) {
    const M: u128 = u64::MAX as u128;
    let (al, ah, bl, bh) = (a & M, a >> 64, b & M, b >> 64);
    let (ll, lh, hl, hh) = (al * bl, al * bh, ah * bl, ah * bh);
    let mid = (ll >> 64) + (lh & M) + (hl & M);
    let lo = (ll & M) | (mid << 64);
    let hi = hh + (lh >> 64) + (hl >> 64) + (mid >> 64);
    (hi, lo)
}

/// a*b >= c*d
fn mul_ge(a: u128, b: u128, c: u128, d: u128) -> bool {
    wide_mul(a, b) >= wide_mul(c, d)
}

// ---------------------------------------------------------------------------------------------- proportional

proptest! {
    #![proptest_config(cfg())]

    /// exact floor(amount*num/den), or CalculationFailure iff it does not fit u64; zero denominator -> amount
    #[test]
    fn proportional_is_exact_floor(amount in u(), num in u(), den in u()) {
        let r = proportional(amount, num, den);
        if den == 0 {
            prop_assert_eq!(r.unwrap(), amount);
        } else {
            let exact = amount as u128 * num as u128 / den as u128;
            match r {
                Ok(v) => { prop_assert!(exact <= u64::MAX as u128); prop_assert_eq!(v as u128, exact); }
                Err(e) => { prop_assert!(exact > u64::MAX as u128); prop_assert_eq!(code(&e), Some(u32::from(ME::CalculationFailure))); }
            }
        }
    }

    #[test]
    fn proportional_trivial_cases(a in u(), n in u(), d in 1u64..) {
        prop_assert_eq!(proportional(a, 0, d).unwrap(), 0);
        prop_assert_eq!(proportional(0, n, d).unwrap(), 0);
        prop_assert_eq!(proportional(a, d, d).unwrap(), a);
        // a fraction <= 1 never exceeds the amount (and therefore never fails)
        if n <= d { prop_assert!(proportional(a, n, d).unwrap() <= a); }
    }

    #[test]
    fn proportional_is_monotone(a1 in u(), a2 in u(), n in u(), d in 1u64..) {
        let (lo, hi) = if a1 <= a2 { (a1, a2) } else { (a2, a1) };
        match (proportional(lo, n, d), proportional(hi, n, d)) {
            (Ok(x), Ok(y)) => prop_assert!(x <= y),
            (Err(_), Ok(_)) => prop_assert!(false, "smaller amount failed, larger succeeded"),
            _ => {}
        }
        // numerator
        let (n1, n2) = if a1 <= a2 { (a1, a2) } else { (a2, a1) };
        match (proportional(n, n1, d), proportional(n, n2, d)) {
            (Ok(x), Ok(y)) => prop_assert!(x <= y),
            (Err(_), Ok(_)) => prop_assert!(false, "smaller numerator failed, larger succeeded"),
            _ => {}
        }
    }

    #[test]
    fn proportional_is_antitone_in_denominator(a in u(), n in u(), d1 in 1u64.., d2 in 1u64..) {
        let (lo, hi) = if d1 <= d2 { (d1, d2) } else { (d2, d1) };
        match (proportional(a, n, lo), proportional(a, n, hi)) {
            (Ok(x), Ok(y)) => prop_assert!(x >= y),
            (Ok(_), Err(_)) => prop_assert!(false, "larger denominator failed, smaller succeeded"),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------- shares <-> value

proptest! {
    #![proptest_config(cfg())]

    /// first mint is 1:1; otherwise shares <= exact value*shares/total_value (never mints more than the pool earned)
    #[test]
    fn shares_from_value_rounds_down(v in u(), tv in u(), ts in u()) {
        let r = shares_from_value(v, tv, ts);
        if ts == 0 {
            prop_assert_eq!(r.unwrap(), v);
        } else if tv == 0 {
            // documented: zero total value with live shares mints 1:1 (the existing shares are worthless)
            prop_assert_eq!(r.unwrap(), v);
        } else {
            let exact = v as u128 * ts as u128 / tv as u128;
            match r {
                Ok(s) => prop_assert_eq!(s as u128, exact),
                Err(e) => { prop_assert!(exact > u64::MAX as u128); prop_assert_eq!(code(&e), Some(u32::from(ME::CalculationFailure))); }
            }
        }
    }

    #[test]
    fn value_from_shares_rounds_down(s in u(), tv in u(), ts in u()) {
        let r = value_from_shares(s, tv, ts);
        if ts == 0 {
            prop_assert_eq!(r.unwrap(), s); // documented: no shares outstanding
        } else {
            let exact = s as u128 * tv as u128 / ts as u128;
            match r {
                Ok(v) => {
                    prop_assert_eq!(v as u128, exact);
                    // redeeming at most the whole supply never pays more than the pool holds
                    if s <= ts { prop_assert!(v <= tv); }
                }
                Err(e) => { prop_assert!(exact > u64::MAX as u128); prop_assert_eq!(code(&e), Some(u32::from(ME::CalculationFailure))); }
            }
        }
    }

    /// deposit `v`, mint shares, redeem them at the post-deposit price: you never get back more than you put in
    #[test]
    fn deposit_redeem_round_trip_never_creates_value(v in pool(), tv in pool(), ts in pool()) {
        prop_assume!(tv.checked_add(v).is_some());
        let Ok(s) = shares_from_value(v, tv, ts) else { return Ok(()) };
        prop_assume!(ts.checked_add(s).is_some());
        let back = value_from_shares(s, tv + v, ts + s).unwrap();
        prop_assert!(back <= v, "deposit {v} at {tv}/{ts} minted {s}, redeemed for {back}");
    }

    /// the share price (value per share) never decreases when minting for a deposit
    #[test]
    fn mint_never_dilutes(v in pool(), tv in pool(), ts in pool()) {
        let Ok(s) = shares_from_value(v, tv, ts) else { return Ok(()) };
        // (tv+v)/(ts+s) >= tv/ts  <=>  (tv+v)*ts >= tv*(ts+s)
        prop_assert!(mul_ge(tv as u128 + v as u128, ts as u128, tv as u128, ts as u128 + s as u128), "price fell: mint {s} shares for {v} at {tv}/{ts}");
    }

    /// redeeming shares for value never lowers the price of the remaining shares
    #[test]
    fn redeem_never_dilutes(sf in 0u32..=1_000_000, tv in pool(), ts in pool()) {
        let s = (ts as u128 * sf as u128 / 1_000_000) as u64; // any share count up to the whole supply
        let Ok(w) = value_from_shares(s, tv, ts) else { return Ok(()) };
        // (tv-w)/(ts-s) >= tv/ts  <=>  (tv-w)*ts >= tv*(ts-s)
        prop_assert!(w <= tv);
        prop_assert!(mul_ge((tv - w) as u128, ts as u128, tv as u128, (ts - s) as u128), "price fell: redeemed {s} for {w} at {tv}/{ts}");
    }

    /// shares -> value -> shares (selling then buying back at the same price) does not create shares
    #[test]
    fn redeem_then_mint_never_creates_shares(sf in 0u32..=1_000_000, tv in pool(), ts in pool()) {
        let s = (ts as u128 * sf as u128 / 1_000_000) as u64;
        let Ok(w) = value_from_shares(s, tv, ts) else { return Ok(()) };
        let Ok(s2) = shares_from_value(w, tv, ts) else { return Ok(()) };
        prop_assert!(s2 <= s, "redeem {s} -> {w} -> mint {s2} at {tv}/{ts}");
    }

    /// splitting a deposit in two does not mint more shares than one deposit
    #[test]
    fn splitting_a_deposit_never_helps(v1 in pool(), v2 in pool(), tv in pool(), ts in pool()) {
        prop_assume!(tv.checked_add(v1).and_then(|x| x.checked_add(v2)).is_some() && v1.checked_add(v2).is_some());
        let Ok(whole) = shares_from_value(v1 + v2, tv, ts) else { return Ok(()) };
        let Ok(s1) = shares_from_value(v1, tv, ts) else { return Ok(()) };
        prop_assume!(ts.checked_add(s1).is_some());
        let Ok(s2) = shares_from_value(v2, tv + v1, ts + s1) else { return Ok(()) };
        prop_assert!(s1 as u128 + s2 as u128 <= whole as u128, "split {s1}+{s2} > whole {whole}");
    }

    /// more value -> more shares, more shares -> more value
    #[test]
    fn conversions_are_monotone(a in pool(), b in pool(), tv in pool(), ts in pool()) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        if let (Ok(x), Ok(y)) = (shares_from_value(lo, tv, ts), shares_from_value(hi, tv, ts)) { prop_assert!(x <= y); }
        if let (Ok(x), Ok(y)) = (value_from_shares(lo, tv, ts), value_from_shares(hi, tv, ts)) { prop_assert!(x <= y); }
    }
}

// ---------------------------------------------------------------------------------------------- fees

proptest! {
    #![proptest_config(cfg())]

    #[test]
    fn fee_check_boundary(bp in any::<u32>()) {
        prop_assert_eq!(Fee::from_basis_points(bp).check().is_ok(), bp <= 10_000);
        prop_assert_eq!(FeeCents::from_bp_cents(bp).check().is_ok(), bp <= 1_000_000);
        if let Err(e) = Fee::from_basis_points(bp).check() { prop_assert_eq!(code(&e), Some(u32::from(ME::BasisPointsOverflow))); }
        if let Err(e) = FeeCents::from_bp_cents(bp).check() { prop_assert_eq!(code(&e), Some(u32::from(ME::BasisPointCentsOverflow))); }
    }

    /// valid fees: exact floor, never above the amount, no panic on any u64, `amount - fee` cannot underflow
    #[test]
    fn fee_apply_is_floor_and_bounded(l in u(), bp in 0u32..=10_000, cents in 0u32..=1_000_000) {
        let f = Fee::from_basis_points(bp).apply(l);
        prop_assert_eq!(f as u128, l as u128 * bp as u128 / 10_000);
        prop_assert!(f <= l);
        let c = FeeCents::from_bp_cents(cents).apply(l);
        prop_assert_eq!(c as u128, l as u128 * cents as u128 / 1_000_000);
        prop_assert!(c <= l);
        // fee <= exact: the user never pays MORE than the nominal fee (flooring favours the payer by < 1 lamport)
        prop_assert!((l as u128 * bp as u128) - (f as u128 * 10_000) < 10_000);
    }

    #[test]
    fn fee_apply_is_monotone(l1 in u(), l2 in u(), b1 in 0u32..=10_000, b2 in 0u32..=10_000) {
        let (lo, hi) = if l1 <= l2 { (l1, l2) } else { (l2, l1) };
        prop_assert!(Fee::from_basis_points(b1).apply(lo) <= Fee::from_basis_points(b1).apply(hi));
        let (blo, bhi) = if b1 <= b2 { (b1, b2) } else { (b2, b1) };
        prop_assert!(Fee::from_basis_points(blo).apply(l1) <= Fee::from_basis_points(bhi).apply(l1));
        prop_assert!(FeeCents::from_bp_cents(blo * 100).apply(l1) <= FeeCents::from_bp_cents(bhi * 100).apply(l1));
    }

    /// splitting one operation into two never pays MORE fee than the whole (floors add up to <= floor of the sum):
    /// the rounding loss is at most 1 lamport per split, so splitting to dodge fees saves < 1 lamport per tx
    #[test]
    fn fee_split_saves_at_most_one_lamport(l1 in 0u64..=u64::MAX / 2, l2 in 0u64..=u64::MAX / 2, bp in 0u32..=10_000) {
        let f = Fee::from_basis_points(bp);
        let (a, b, w) = (f.apply(l1), f.apply(l2), f.apply(l1 + l2));
        prop_assert!(a + b <= w);
        prop_assert!(w - (a + b) <= 1);
    }

    /// documented limit: unvalidated fees (`max_stake_moved_per_epoch` is never `check()`ed) can exceed the amount and
    /// even wrap the `as u64` cast. This pins the behaviour so a change is noticed.
    #[test]
    fn fee_apply_unvalidated_is_the_truncated_exact(l in u(), bp in 10_001u32..) {
        let exact = l as u128 * bp as u128 / 10_000;
        prop_assert_eq!(Fee::from_basis_points(bp).apply(l), exact as u64);
    }
}

// ---------------------------------------------------------------------------------------------- liq pool fee curve

fn pool_with(min_bp: u32, max_bp: u32, target: u64) -> LiqPool {
    LiqPool {
        lp_mint: Default::default(),
        lp_mint_authority_bump_seed: 0,
        sol_leg_bump_seed: 0,
        msol_leg_authority_bump_seed: 0,
        msol_leg: Default::default(),
        lp_liquidity_target: target,
        lp_max_fee: Fee::from_basis_points(max_bp),
        lp_min_fee: Fee::from_basis_points(min_bp),
        treasury_cut: Fee::from_basis_points(2500),
        lp_supply: 0,
        lent_from_sol_leg: 0,
        liquidity_sol_cap: u64::MAX,
    }
}

proptest! {
    #![proptest_config(cfg())]

    /// for every parameter set `validate()` accepts: min <= fee <= max, non-increasing in liquidity, min at/above target
    #[test]
    fn linear_fee_stays_in_range_and_is_monotone(
        min_bp in 0u32..=1000, extra in 0u32..=1000, target_sol in 50u64..=10_000_000, l1 in u(), l2 in u()
    ) {
        let max_bp = (min_bp + extra).min(1000);
        prop_assume!(min_bp <= max_bp);
        let target = target_sol * 1_000_000_000;
        let p = pool_with(min_bp, max_bp, target);
        prop_assert!(p.validate().is_ok());
        let (lo, hi) = if l1 <= l2 { (l1, l2) } else { (l2, l1) };
        let (flo, fhi) = (p.linear_fee(lo), p.linear_fee(hi));
        prop_assert!(flo.basis_points >= min_bp && flo.basis_points <= max_bp);
        prop_assert!(fhi.basis_points >= min_bp && fhi.basis_points <= max_bp);
        prop_assert!(flo >= fhi, "fee increased with more liquidity: {} at {lo} < {} at {hi}", flo, fhi);
        prop_assert_eq!(p.linear_fee(0).basis_points, max_bp);
        if hi >= target { prop_assert_eq!(fhi.basis_points, min_bp); }
    }

    #[test]
    fn lp_validate_matches_its_rules(min_bp in 0u32..12_000, max_bp in 0u32..12_000, target in u(), cut in 0u32..12_000) {
        let mut p = pool_with(min_bp, max_bp, target);
        p.treasury_cut = Fee::from_basis_points(cut);
        let ok = min_bp <= 10_000 && max_bp <= 10_000 && cut <= 10_000 && max_bp <= 1000 && max_bp >= min_bp && target >= 50_000_000_000 && cut <= 7500;
        prop_assert_eq!(p.validate().is_ok(), ok);
    }
}

// ---------------------------------------------------------------------------------------------- State price helpers

fn state(active: u64, delayed: u64, emergency: u64, reserve_avail: u64, tickets: u64, supply: u64) -> State {
    // State has no Default; the program itself builds a zeroed one in `serialized_len` (all fields are plain data)
    let mut st: State = unsafe { std::mem::MaybeUninit::<State>::zeroed().assume_init() };
    st.validator_system.total_active_balance = active;
    st.stake_system.delayed_unstake_cooling_down = delayed;
    st.emergency_cooling_down = emergency;
    st.available_reserve_balance = reserve_avail;
    st.circulating_ticket_balance = tickets;
    st.msol_supply = supply;
    st
}

/// realistic component range: each part below ~2^60 so the four-way sum (checked in the program) fits u64
fn part() -> BoxedStrategy<u64> {
    prop_oneof![Just(0u64), Just(1u64), 0u64..1_000_000, 1u64..=700_000_000_000_000_000, 0u64..=(1u64 << 60)].boxed()
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    fn total_virtual_staked_is_saturating(a in part(), d in part(), e in part(), r in part(), t in u(), s in part()) {
        let st = state(a, d, e, r, t, s);
        let tlc = st.total_lamports_under_control();
        prop_assert_eq!(tlc, a + d + e + r);
        prop_assert_eq!(st.total_virtual_staked_lamports(), tlc.saturating_sub(t));
    }

    /// mSOL -> SOL never exceeds the exact value and never exceeds the pool when redeeming <= supply
    #[test]
    fn msol_to_sol_rounds_down(a in part(), d in part(), e in part(), r in part(), t in part(), s in pool(), m in u()) {
        let st = state(a, d, e, r, t, s);
        let tvs = st.total_virtual_staked_lamports();
        match st.msol_to_sol(m) {
            Ok(v) => {
                prop_assert_eq!(v as u128, m as u128 * tvs as u128 / s as u128);
                if m <= s { prop_assert!(v <= tvs); }
            }
            Err(e) => prop_assert_eq!(code(&e), Some(u32::from(ME::CalculationFailure))),
        }
    }

    /// deposit `l` (after the fee) then sell the minted mSOL at the post-deposit state: no free lamports
    #[test]
    fn deposit_then_unstake_never_creates_lamports(a in part(), d in part(), e in part(), r in part(), t in part(), s in pool(), l in part()) {
        let st = state(a, d, e, r, t, s);
        prop_assume!(st.total_virtual_staked_lamports() > 0);
        let Ok(minted) = st.calc_msol_from_lamports(l) else { return Ok(()) };
        prop_assume!(s.checked_add(minted).is_some() && r.checked_add(l).is_some() && (a + d + e + r).checked_add(l).is_some());
        // all of the deposit goes to the reserve (sol_deposited), the mSOL is minted: both totals grow
        let mut st2 = state(a, d, e, r + l, t, s + minted);
        st2.validator_system.total_active_balance = a;
        let back = st2.msol_to_sol(minted).unwrap();
        prop_assert!(back <= l, "deposited {l}, minted {minted} mSOL, sold for {back} (price {} / {})", st.total_virtual_staked_lamports(), s);
    }

    /// stake_delta: exact when the raw value is >= 0; when negative it is capped at 0 and never below raw
    #[test]
    fn stake_delta_is_well_formed(a in part(), d in part(), e in part(), reserve in part(), t in part(), rent in 0u64..10_000_000) {
        let mut st = state(a, d, e, 0, t, 1);
        st.rent_exempt_for_token_acc = rent;
        let raw = reserve.saturating_sub(rent) as i128 + d as i128 - t as i128;
        let got = st.stake_delta(reserve);
        if raw >= 0 {
            prop_assert_eq!(got, raw);
        } else {
            prop_assert!(got <= 0, "negative raw {raw} turned positive: {got}");
            prop_assert!(got >= raw);
            prop_assert_eq!(got, (raw + e as i128).min(0));
        }
    }
}
