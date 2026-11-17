//! Stateful fuzzing of the Marinade program on the LiteSVM harness (proptest, plain strategies over `Vec<Op>`).
//!
//! * A case is `Case { warm, transition_at, ops }`. `warm` selects a pre-populated genesis (several epochs of
//!   deposits, stake accounts on 3 validators, rewards, a pending delayed unstake) over an empty one.
//!   Both genesis envs are built once and cloned per case (LiteSVM is `Clone`).
//! * Every op is executed against the real program; after EVERY op the invariants (`harness::invariants`) and the
//!   fuzz-level checks (token conservation, per-op "no free lamports" position check) run.
//! * A failing tx is accepted only if the op's [`Judge`] explains it (explicit per-op allow-lists with the reason).
//!   Certain failures (e.g. deposit below `min_deposit`) must NOT succeed.
//! * At the end of every case: resume/clear windows, full crank in the next epoch + `check_fully_updated`, then
//!   every actor exits (liquid unstake / delayed unstake + claim) and `out <= in + rewards` is checked.
//!
//! Environment: `PROPTEST_CASES` (cases), `FUZZ_MAX_OPS` (max ops per case, default 40), `FUZZ_SEED` (u64, default
//! fixed: runs are deterministic; the seed is printed), `FUZZ_SHRINK_ITERS`, `FUZZ_NO_PERSIST=1` (no regression file),
//! `FUZZ_REGRESSIONS=<path>` (regression file, default `tests/harness/proptest-regressions/fuzz.txt`),
//! `HARNESS_BASE` / `HARNESS_TARGET` / `MARINADE_SO` as for all harness tests (they differ => one `transition`).
use harness::convert::to_addr;
use harness::crank::Crank;
use harness::env::{sol, Activation, TestEnv, TxFailure, TxResult, LAMPORTS_PER_SOL};
use harness::invariants::Checker;
use harness::marinade::{ix, Marinade, SetupOpts, User, TICKET_SIZE};
use harness::profile::{self, Runtime};
use harness::Signer;
use marinade_finance::error::MarinadeError as ME;
use marinade_finance::instructions::admin::{ConfigLpParams, ConfigMarinadeParams};
use marinade_finance::state::fee::{Fee, FeeCents};
use marinade_finance::state::stake_system::{StakeRecord, StakeStatus};
use marinade_finance::State;
use proptest::prelude::*;
use proptest::strategy::Union;
use proptest::test_runner::{Config, FileFailurePersistence, RngAlgorithm, RngSeed, TestError, TestRng, TestRunner};
use solana_address::Address;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

const NA: usize = 4; // actors
const NV: usize = 5; // vote accounts in the pool (3 listed at genesis)
const ACTOR_SOL: u64 = 100_000_000; // SOL per actor (1e17 lamports; 4 actors stay far below u64::MAX)
const KEEP: u64 = 10 * LAMPORTS_PER_SOL; // `Frac` amounts never touch the last 10 SOL of an actor

// ===================================================================================== ops

/// Amount specs, resolved against the live state when the op runs (so replays are deterministic and shrinks readable).
#[derive(Clone, Copy, Debug)]
enum Amt {
    Zero,
    One,
    Sol(u32),
    Milli(u32),
    /// permille of the base (balance minus 10 SOL for lamports, token balance for tokens)
    Frac(u16),
    MinDeposit(i8),
    MinStake(i8),
    MinWithdraw(i8),
    /// base + 1
    BasePlus1,
    MaxU64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Lamports,
    Tokens,
}

fn resolve(a: Amt, base: u64, unit: Unit, st: &State) -> u64 {
    let off = |v: u64, d: i8| (v as i128 + d as i128).clamp(0, u64::MAX as i128) as u64;
    match a {
        Amt::Zero => 0,
        Amt::One => 1,
        Amt::Sol(n) => n as u64 * LAMPORTS_PER_SOL,
        Amt::Milli(n) => n as u64 * 1_000_000,
        Amt::Frac(pm) => {
            let b = if unit == Unit::Lamports { base.saturating_sub(KEEP) } else { base };
            (b as u128 * pm as u128 / 1000) as u64
        }
        Amt::MinDeposit(d) => off(st.min_deposit, d),
        Amt::MinStake(d) => off(st.stake_system.min_stake, d),
        Amt::MinWithdraw(d) => {
            let l = off(st.min_withdraw, d);
            if unit == Unit::Tokens {
                st.calc_msol_from_lamports(l).unwrap_or(l)
            } else {
                l
            }
        }
        Amt::BasePlus1 => base.saturating_add(1),
        Amt::MaxU64 => u64::MAX,
    }
}

#[derive(Clone, Copy, Debug)]
enum Extra {
    None,
    OneLamport,
    /// up to the ceiling the program accepts (legacy rent - live rent)
    ToCeiling,
    OverCeiling,
}

#[derive(Clone, Copy, Debug)]
enum RewardKind {
    OneLamport,
    /// ~0.045% (an epoch of mainnet staking rewards)
    Epoch,
    /// 5% (stress)
    Big,
}

#[derive(Clone, Copy, Debug)]
enum DonateTo {
    Reserve,
    SolLeg,
    Stake(u8),
}

#[derive(Clone, Copy, Debug)]
enum Cfg {
    MinDeposit(u64),
    MinWithdraw(u64),
    MinStake(u64),
    RewardsFeeBp(u32),
    DelayedUnstakeFee(u32),
    DepositSolFee(u32),
    DepositStakeFee(u32),
    WithdrawStakeFee(u32),
    WithdrawEnabled(bool),
    /// 0 = unlimited, 1 = current total_lamports_under_control, 2 = tlc + 1000 SOL
    StakingCap(u8),
    LiquidityCap(u8),
    MaxStakeMovedBp(u32),
}

#[derive(Clone, Copy, Debug)]
struct LpCfg {
    min_fee: Option<u32>,
    max_fee: Option<u32>,
    target_sol: Option<u32>,
    treasury_cut: Option<u32>,
}

#[derive(Clone, Debug)]
enum Op {
    // ---- users
    Deposit { a: u8, amt: Amt },
    LiquidUnstake { a: u8, amt: Amt },
    AddLiquidity { a: u8, amt: Amt },
    RemoveLiquidity { a: u8, amt: Amt },
    OrderUnstake { a: u8, amt: Amt },
    Claim { t: u8 },
    DepositStake { a: u8, v: u8, amt: Amt, extra: Extra, redeposit: bool },
    WithdrawStake { a: u8, s: u8, amt: Amt, to: u8 },
    // ---- crank
    StakeReserve { v: u8, window: bool },
    UpdateActive { s: u8 },
    UpdateDeactivated { s: u8 },
    UpdateAll,
    CanonicalMerge,
    CreateCanonical { s: u8 },
    MergeRaw { v: u8, s: u8 },
    DeactivateStake { s: u8, window: bool },
    PartialUnstake { s: u8, amt: Amt },
    EmergencyUnstake { s: u8 },
    DoWork,
    // ---- environment
    Warp { n: u8 },
    /// warp `n` epochs and open the partitioned-rewards window at the start of the new epoch for `len` ops
    WarpWithRewardsWindow { n: u8, len: u8 },
    WarpToWindow,
    Reward { s: u8, kind: RewardKind },
    Mev { s: u8, amt: Amt },
    Slash { s: u8, permille: u16 },
    RewardsWindow { len: u8 },
    Donate { to: DonateTo, amt: Amt },
    // ---- management / admin
    SetScore { v: u8, score: u32 },
    AddValidator { v: u8, score: u32 },
    RemoveValidator { v: u8 },
    Config(Cfg),
    ConfigLp(LpCfg),
    Pause,
    Resume,
}

#[derive(Clone)]
struct Case {
    warm: bool,
    /// the runtime switches (BASE -> TARGET) right before executing op `transition_at` (>= len: after the last op);
    /// ignored when BASE == TARGET
    transition_at: usize,
    ops: Vec<Op>,
}

impl std::fmt::Debug for Case {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Case {{ warm: {}, transition_at: {} }} [", self.warm, self.transition_at)?;
        for (i, o) in self.ops.iter().enumerate() {
            if i == self.transition_at {
                writeln!(f, "  ---- TRANSITION BASE -> TARGET ----")?;
            }
            writeln!(f, "  {i:>2}: {o:?}")?;
        }
        if self.transition_at >= self.ops.len() {
            writeln!(f, "  ---- TRANSITION BASE -> TARGET ----")?;
        }
        write!(f, "]")
    }
}

// ===================================================================================== strategies

fn amt_s() -> BoxedStrategy<Amt> {
    prop_oneof![
        10 => (1u32..=1000).prop_map(Amt::Sol),
        12 => (1u16..=1000).prop_map(Amt::Frac),
        6 => (1u32..=2000).prop_map(Amt::Milli),
        2 => Just(Amt::Zero),
        2 => Just(Amt::One),
        3 => (-1i8..=1).prop_map(Amt::MinDeposit),
        3 => (-1i8..=1).prop_map(Amt::MinStake),
        2 => (-1i8..=1).prop_map(Amt::MinWithdraw),
        1 => Just(Amt::BasePlus1),
        1 => Just(Amt::MaxU64),
    ]
    .boxed()
}

fn sel() -> impl Strategy<Value = u8> {
    0u8..16
}
fn actor() -> impl Strategy<Value = u8> {
    0u8..NA as u8
}

fn cfg_s() -> BoxedStrategy<Cfg> {
    prop_oneof![
        3 => prop_oneof![Just(0u64), Just(1), Just(1_000_000), Just(10_000_000), Just(LAMPORTS_PER_SOL)].prop_map(Cfg::MinDeposit),
        3 => prop_oneof![Just(0u64), Just(1), Just(1_000_000), Just(10_000_000), Just(LAMPORTS_PER_SOL / 10), Just(LAMPORTS_PER_SOL / 10 + 1)].prop_map(Cfg::MinWithdraw),
        3 => prop_oneof![Just(LAMPORTS_PER_SOL / 100 - 1), Just(LAMPORTS_PER_SOL / 100), Just(LAMPORTS_PER_SOL), Just(5 * LAMPORTS_PER_SOL), Just(100 * LAMPORTS_PER_SOL)].prop_map(Cfg::MinStake),
        2 => prop_oneof![Just(0u32), Just(100), Just(300), Just(1000), Just(1001)].prop_map(Cfg::RewardsFeeBp),
        2 => fee_cents().prop_map(Cfg::DelayedUnstakeFee),
        2 => fee_cents().prop_map(Cfg::DepositSolFee),
        2 => fee_cents().prop_map(Cfg::DepositStakeFee),
        2 => fee_cents().prop_map(Cfg::WithdrawStakeFee),
        1 => any::<bool>().prop_map(Cfg::WithdrawEnabled),
        2 => (0u8..3).prop_map(Cfg::StakingCap),
        2 => (0u8..3).prop_map(Cfg::LiquidityCap),
        2 => prop_oneof![Just(0u32), Just(50), Just(1000), Just(10_000)].prop_map(Cfg::MaxStakeMovedBp),
    ]
    .boxed()
}

fn fee_cents() -> impl Strategy<Value = u32> {
    prop_oneof![Just(0u32), Just(1), Just(500), Just(1000), Just(2000), Just(2001)]
}

fn lp_cfg_s() -> impl Strategy<Value = LpCfg> {
    (
        prop::option::of(prop_oneof![Just(0u32), Just(30), Just(100), Just(300), Just(10_001)]),
        prop::option::of(prop_oneof![Just(0u32), Just(100), Just(300), Just(1000), Just(1001)]),
        prop::option::of(prop_oneof![Just(49u32), Just(50), Just(100), Just(10_000)]),
        prop::option::of(prop_oneof![Just(0u32), Just(2500), Just(7500), Just(7501)]),
    )
        .prop_map(|(min_fee, max_fee, target_sol, treasury_cut)| LpCfg { min_fee, max_fee, target_sol, treasury_cut })
}

fn op_s() -> BoxedStrategy<Op> {
    let w: Vec<(u32, BoxedStrategy<Op>)> = vec![
        (14, (actor(), amt_s()).prop_map(|(a, amt)| Op::Deposit { a, amt }).boxed()),
        (7, (actor(), amt_s()).prop_map(|(a, amt)| Op::LiquidUnstake { a, amt }).boxed()),
        (6, (actor(), amt_s()).prop_map(|(a, amt)| Op::AddLiquidity { a, amt }).boxed()),
        (4, (actor(), amt_s()).prop_map(|(a, amt)| Op::RemoveLiquidity { a, amt }).boxed()),
        (6, (actor(), amt_s()).prop_map(|(a, amt)| Op::OrderUnstake { a, amt }).boxed()),
        (6, sel().prop_map(|t| Op::Claim { t }).boxed()),
        (
            6,
            (
                actor(),
                sel(),
                amt_s(),
                prop_oneof![8 => Just(Extra::None), 1 => Just(Extra::OneLamport), 1 => Just(Extra::ToCeiling), 1 => Just(Extra::OverCeiling)],
                prop_oneof![5 => Just(false), 1 => Just(true)],
            )
                .prop_map(|(a, v, amt, extra, redeposit)| Op::DepositStake { a, v, amt, extra, redeposit })
                .boxed(),
        ),
        (4, (actor(), sel(), amt_s(), actor()).prop_map(|(a, s, amt, to)| Op::WithdrawStake { a, s, amt, to }).boxed()),
        (6, (sel(), prop_oneof![3 => Just(true), 1 => Just(false)]).prop_map(|(v, window)| Op::StakeReserve { v, window }).boxed()),
        (4, sel().prop_map(|s| Op::UpdateActive { s }).boxed()),
        (4, sel().prop_map(|s| Op::UpdateDeactivated { s }).boxed()),
        (4, Just(Op::UpdateAll).boxed()),
        (3, Just(Op::CanonicalMerge).boxed()),
        (2, sel().prop_map(|s| Op::CreateCanonical { s }).boxed()),
        (2, (sel(), sel()).prop_map(|(v, s)| Op::MergeRaw { v, s }).boxed()),
        (4, (sel(), prop_oneof![3 => Just(true), 1 => Just(false)]).prop_map(|(s, window)| Op::DeactivateStake { s, window }).boxed()),
        (3, (sel(), amt_s()).prop_map(|(s, amt)| Op::PartialUnstake { s, amt }).boxed()),
        (2, sel().prop_map(|s| Op::EmergencyUnstake { s }).boxed()),
        (5, Just(Op::DoWork).boxed()),
        (8, (1u8..=3).prop_map(|n| Op::Warp { n }).boxed()),
        (3, (1u8..=2, 1u8..=4).prop_map(|(n, len)| Op::WarpWithRewardsWindow { n, len }).boxed()),
        (2, Just(Op::WarpToWindow).boxed()),
        (
            5,
            (sel(), prop_oneof![1 => Just(RewardKind::OneLamport), 6 => Just(RewardKind::Epoch), 1 => Just(RewardKind::Big)])
                .prop_map(|(s, kind)| Op::Reward { s, kind })
                .boxed(),
        ),
        (3, (sel(), amt_s()).prop_map(|(s, amt)| Op::Mev { s, amt }).boxed()),
        (1, (sel(), 1u16..=1000).prop_map(|(s, permille)| Op::Slash { s, permille }).boxed()),
        (2, (1u8..=4).prop_map(|len| Op::RewardsWindow { len }).boxed()),
        (
            3,
            (prop_oneof![Just(DonateTo::Reserve), Just(DonateTo::SolLeg), sel().prop_map(DonateTo::Stake)], amt_s())
                .prop_map(|(to, amt)| Op::Donate { to, amt })
                .boxed(),
        ),
        (3, (sel(), prop_oneof![Just(0u32), Just(1), Just(100), Just(1000), Just(1_000_000)]).prop_map(|(v, score)| Op::SetScore { v, score }).boxed()),
        (2, (sel(), prop_oneof![Just(0u32), Just(100), Just(1000)]).prop_map(|(v, score)| Op::AddValidator { v, score }).boxed()),
        (2, sel().prop_map(|v| Op::RemoveValidator { v }).boxed()),
        (4, cfg_s().prop_map(Op::Config).boxed()),
        (2, lp_cfg_s().prop_map(Op::ConfigLp).boxed()),
        (1, Just(Op::Pause).boxed()),
        (4, Just(Op::Resume).boxed()),
    ];
    Union::new_weighted(w).boxed()
}

fn max_ops() -> usize {
    std::env::var("FUZZ_MAX_OPS").ok().and_then(|v| v.parse().ok()).unwrap_or(40)
}

fn case_s() -> impl Strategy<Value = Case> {
    let n = max_ops();
    (prop_oneof![7 => Just(true), 3 => Just(false)], 0..=n, prop::collection::vec(op_s(), 1..=n))
        .prop_map(|(warm, transition_at, ops)| Case { warm, transition_at, ops })
}

// ===================================================================================== judge

struct Judge {
    allowed: Vec<(u32, String, &'static str)>,
    certain: Vec<(u32, String, &'static str)>,
    stake_err: Vec<&'static str>,
    anchor_err: Vec<&'static str>,
    /// KNOWN program bugs (panics): (log needle, description). Counted, not failed, so the fuzzer keeps exploring.
    known_panics: Vec<(&'static str, &'static str)>,
}

fn code_name(e: ME) -> String {
    format!("{e:?}")
}

impl Judge {
    fn new() -> Self {
        Judge { allowed: vec![], certain: vec![], stake_err: vec![], anchor_err: vec![], known_panics: vec![] }
    }
    /// `e` is a legitimate outcome in this state (necessary, not sufficient condition).
    fn may(mut self, cond: bool, e: ME, why: &'static str) -> Self {
        if cond {
            self.allowed.push((u32::from(e), code_name(e), why));
        }
        self
    }
    /// the op MUST fail (with one of the `must` codes): `cond` is an exact failure condition.
    fn must(mut self, cond: bool, e: ME, why: &'static str) -> Self {
        if cond {
            self.allowed.push((u32::from(e), code_name(e), why));
            self.certain.push((u32::from(e), code_name(e), why));
        }
        self
    }
    /// an error raised by the stake program (CPI) is legitimate
    fn stake(mut self, cond: bool, why: &'static str) -> Self {
        if cond {
            self.stake_err.push(why);
        }
        self
    }
    /// a panic whose log contains `needle` is a KNOWN program bug (tests/harness/tests/findings.rs), counted not failed
    fn known(mut self, cond: bool, needle: &'static str, why: &'static str) -> Self {
        if cond {
            self.known_panics.push((needle, why));
        }
        self
    }
    /// an anchor (non-Marinade) account constraint error is legitimate
    fn anchor(mut self, cond: bool, why: &'static str) -> Self {
        if cond {
            self.anchor_err.push(why);
        }
        self
    }
    fn describe(&self) -> String {
        let mut v: Vec<String> = self.allowed.iter().map(|(c, n, w)| format!("{n}({c}): {w}")).collect();
        v.extend(self.stake_err.iter().map(|w| format!("<stake program error>: {w}")));
        v.extend(self.anchor_err.iter().map(|w| format!("<anchor error>: {w}")));
        if v.is_empty() {
            "nothing (op is expected to succeed)".into()
        } else {
            v.join(" | ")
        }
    }
}

enum Outcome {
    Ok,
    Rejected(String),
}

type Res<T> = Result<T, String>;

// ===================================================================================== sim

#[derive(Clone)]
struct Actor {
    user: User,
    /// stake accounts received through withdraw_stake_account (still in the actor's hands)
    owned: Vec<Address>,
    /// tickets ordered and not yet claimed
    tickets: Vec<Address>,
}

#[derive(Clone)]
struct Genesis {
    env: TestEnv,
    m: Marinade,
    actors: Vec<Actor>,
    votes: Vec<Address>,
    /// rewards injected during the warm-up
    rewards: i128,
    /// smallest delegation the stake program accepts (probed)
    min_deleg: u64,
}

#[derive(Clone)]
struct Sim {
    env: TestEnv,
    m: Marinade,
    chk: Checker,
    actors: Vec<Actor>,
    votes: Vec<Address>,
    window_left: u32,
    /// net lamports injected as rewards / donations (negative: slashed)
    rewards: i128,
    /// sum of per-op tolerances used (rounding slack allowed in the final accounting)
    slack: u128,
    transitioned: bool,
    trace: Vec<String>,
    nops: u64,
    min_deleg: u64,
    /// crank deactivate_stake failures caused by the stake program's minimum delegation (known limit, see findings)
    min_deleg_hits: u64,
    price_overflow_hits: u64,
}

type Stats = BTreeMap<String, u64>;

static STATS: Mutex<Stats> = Mutex::new(BTreeMap::new());

fn stat(k: String) {
    *STATS.lock().unwrap().entry(k).or_default() += 1;
}

struct Rec {
    idx: u32,
    addr: Address,
    rec: StakeRecord,
    view: harness::env::StakeView,
    vi: Option<u32>,
}

impl Rec {
    fn deact(&self) -> bool {
        self.view.delegation.as_ref().map(|d| d.deactivation_epoch != u64::MAX).unwrap_or(false)
    }
    fn updated(&self) -> bool {
        self.view.delegation.as_ref().map(|d| d.stake == self.rec.last_update_delegated_lamports).unwrap_or(false)
    }
    fn active(&self) -> bool {
        self.rec.last_update_status == StakeStatus::Active
    }
}

impl Sim {
    fn fork(g: &Genesis) -> Sim {
        let chk = Checker::new(&g.env, &g.m);
        Sim {
            env: g.env.clone(),
            m: g.m.clone(),
            chk,
            actors: g.actors.clone(),
            votes: g.votes.clone(),
            window_left: 0,
            rewards: g.rewards,
            slack: 0,
            transitioned: false,
            trace: vec![],
            nops: 0,
            min_deleg: g.min_deleg,
            min_deleg_hits: 0,
            price_overflow_hits: 0,
        }
    }

    fn st(&self) -> State {
        self.m.state(&self.env)
    }
    fn bal(&self, a: usize) -> u64 {
        self.env.lamports(&self.actors[a].user.pubkey())
    }
    fn msol(&self, a: usize) -> u64 {
        self.env.token_balance(&self.actors[a].user.msol)
    }
    fn lp(&self, a: usize) -> u64 {
        self.env.token_balance(&self.actors[a].user.lp)
    }
    fn window_active(&self) -> bool {
        self.window_left > 0
    }
    /// the reserve holds more than `available_reserve_balance + rent` (someone donated lamports; no update_* ran since)
    fn reserve_desynced(&self) -> bool {
        let st = self.st();
        self.env.lamports(&self.m.reserve) > st.available_reserve_balance + st.rent_exempt_for_token_acc
    }
    /// `update_msol_price` stores `msol_to_sol(2^32)`: it overflows u64 once one mSOL unit is worth more than 2^32 lamports
    /// (only after nearly the whole supply exited while fees stayed in the pool), and then update_* revert (known limit)
    fn price_overflow(&self) -> bool {
        let st = self.st();
        st.msol_supply > 0 && (State::PRICE_DENOMINATOR as u128) * st.total_virtual_staked_lamports() as u128 / st.msol_supply as u128 > u64::MAX as u128
    }
    fn in_window(&self) -> bool {
        self.env.in_stake_window(self.m.slots_for_stake_delta)
    }

    fn recs(&self) -> Vec<Rec> {
        self.m
            .indexed_stakes(&self.env)
            .into_iter()
            .filter_map(|r| {
                let addr = to_addr(&r.record.stake_account);
                let view = self.env.stake_view(&addr)?;
                let vi = view.delegation.as_ref().and_then(|d| self.m.validator_index(&self.env, &d.voter));
                Some(Rec { idx: r.index, addr, rec: r.record, view, vi })
            })
            .collect()
    }

    fn pick<T>(v: Vec<T>, s: u8) -> Option<T> {
        if v.is_empty() {
            None
        } else {
            let i = s as usize % v.len();
            v.into_iter().nth(i)
        }
    }

    // ------------------------------------------------------------ position accounting

    fn price_ceil(&self) -> u128 {
        let st = self.st();
        let (t, s) = (st.total_virtual_staked_lamports() as u128, st.msol_supply as u128);
        if s == 0 {
            1
        } else {
            (t + s - 1) / s
        }
    }

    /// Sum over actors of everything they hold, in lamports at the current mSOL / LP price (floor):
    /// SOL + owned stake accounts + unclaimed tickets (face + ticket account rent) + mSOL value + LP value.
    fn position(&self) -> u128 {
        let st = self.st();
        let lp_supply = self.env.mint_supply(&self.m.lp_mint) as u128;
        let sol_avail = self.env.lamports(&self.m.sol_leg).saturating_sub(st.rent_exempt_for_token_acc) as u128;
        let leg_value = st.msol_to_sol(self.env.token_balance(&self.m.msol_leg)).unwrap_or(0) as u128;
        let pool_value = sol_avail + leg_value;
        let mut tot: u128 = 0;
        for (i, a) in self.actors.iter().enumerate() {
            tot += self.bal(i) as u128;
            for s in &a.owned {
                tot += self.env.lamports(s) as u128;
            }
            for t in &a.tickets {
                if let Some(td) = self.m.ticket(&self.env, t) {
                    tot += td.lamports_amount as u128 + self.env.lamports(t) as u128;
                }
            }
            tot += st.msol_to_sol(self.msol(i)).unwrap_or(0) as u128;
            if lp_supply > 0 {
                tot += self.lp(i) as u128 * pool_value / lp_supply;
            }
        }
        // value nobody owns yet (donations / leftovers in an empty pool) goes to the next depositor by design
        if lp_supply == 0 {
            tot += pool_value;
        }
        if st.msol_supply == 0 {
            tot += st.total_virtual_staked_lamports() as u128;
        }
        tot
    }

    /// Run a user op: snapshot the position, run, and require that nobody extracted value (beyond rounding).
    fn user_op(&mut self, name: &str, j: Judge, run: impl FnOnce(&mut Sim) -> TxResult) -> Res<Outcome> {
        let before = self.position();
        let tol = 16 + 4 * self.price_ceil();
        let r = run(self);
        let out = self.judge(name, j, r)?;
        if matches!(out, Outcome::Ok) {
            let after = self.position();
            self.slack += tol;
            if after > before + tol {
                return Err(format!(
                    "[free_lamports] {name}: actors' combined position grew by {} lamports (tolerance {tol}): {before} -> {after}",
                    after - before
                ));
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------ judging

    fn judge(&mut self, name: &str, j: Judge, r: TxResult) -> Res<Outcome> {
        match r {
            Ok(_) => {
                if let Some((c, n, why)) = j.certain.first() {
                    return Err(format!("[accepted_invalid] {name} succeeded although it must fail with {n} ({c}): {why}"));
                }
                stat(format!("{name}: ok"));
                Ok(Outcome::Ok)
            }
            Err(f) => self.judge_failure(name, &j, &f),
        }
    }

    fn judge_failure(&mut self, name: &str, j: &Judge, f: &TxFailure) -> Res<Outcome> {
        if f.is_panic() {
            let logs = f.logs.join("\n");
            if let Some((_, why)) = j.known_panics.iter().find(|(n, _)| logs.contains(n)) {
                stat(format!("{name}: KNOWN BUG panic ({why})"));
                return Ok(Outcome::Rejected(format!("known panic: {why}")));
            }
            return Err(format!("[panic] {name}: program panicked: {f}"));
        }
        if let Some(code) = f.marinade_code() {
            if j.allowed.iter().any(|(c, _, _)| *c == code) {
                let n = f.marinade_name().unwrap_or("?").to_string();
                stat(format!("{name}: rejected {n}"));
                return Ok(Outcome::Rejected(n));
            }
            // HARNESS_RENT_TOPUP=1 on the binary live on mainnet: failures the crank bot cannot prevent (topup.rs)
            if harness::topup::waive(&self.env.marinade_so.sha256, "stateful_fuzz", name, &f.to_string()).is_some() {
                stat(format!("{name}: KNOWN-LIVE-LIMITATION {}", f.marinade_name().unwrap_or("?")));
                return Ok(Outcome::Rejected(format!("known live limitation: {}", f.marinade_name().unwrap_or("?"))));
            }
            // a certain failure must be reported with one of the certain codes
            return Err(format!("[unexpected_error] {name}: {f}\n    allowed here: {}", j.describe()));
        }
        match &f.class {
            harness::env::ErrClass::Anchor { name: n, .. } if !j.anchor_err.is_empty() => {
                stat(format!("{name}: rejected anchor {}", n.clone().unwrap_or_default()));
                return Ok(Outcome::Rejected(format!("anchor {n:?}")));
            }
            harness::env::ErrClass::Custom { .. } | harness::env::ErrClass::Other(_) if !j.stake_err.is_empty() => {
                stat(format!("{name}: rejected by stake program"));
                return Ok(Outcome::Rejected("stake program".into()));
            }
            _ => {}
        }
        Err(format!("[unexpected_error] {name}: {f}\n    allowed here: {}", j.describe()))
    }

    // ------------------------------------------------------------ post-op checks

    fn check(&mut self, what: &str) -> Res<()> {
        self.chk.check_after(&self.env, &self.m, what)?;
        // token conservation
        let st = self.st();
        let actors_msol: u128 = (0..NA).map(|i| self.msol(i) as u128).sum();
        let others = self.env.token_balance(&self.m.msol_leg) as u128 + self.env.token_balance(&self.m.treasury_msol) as u128;
        let mint = self.env.mint_supply(&self.m.msol_mint) as u128;
        if actors_msol + others != mint {
            return Err(format!("[msol_tokens] after {what}: actors {actors_msol} + leg/treasury {others} != mint supply {mint}"));
        }
        let lp_sum: u128 = (0..NA).map(|i| self.lp(i) as u128).sum();
        if lp_sum != self.env.mint_supply(&self.m.lp_mint) as u128 {
            return Err(format!("[lp_tokens] after {what}: actors {lp_sum} != lp mint supply {}", self.env.mint_supply(&self.m.lp_mint)));
        }
        // tickets known to the sim == tickets on chain
        let mut chain: Vec<Address> = self.m.tickets(&self.env).into_iter().map(|(k, _)| k).collect();
        let mut known: Vec<Address> = self.actors.iter().flat_map(|a| a.tickets.iter().cloned()).collect();
        chain.sort();
        known.sort();
        if chain != known {
            return Err(format!("[tickets] after {what}: on-chain tickets {chain:?} != expected {known:?}"));
        }
        let _ = st;
        Ok(())
    }

    // ------------------------------------------------------------ ops

    fn exec(&mut self, op: &Op) -> Res<String> {
        let desc = format!("{op:?}");
        stat(format!("op generated: {}", desc.split(|c: char| c == ' ' || c == '(').next().unwrap_or("?")));
        let mut note = String::new();
        match op.clone() {
            Op::Deposit { a, amt } => {
                let a = a as usize;
                let st = self.st();
                let l = resolve(amt, self.bal(a), Unit::Lamports, &st);
                let tvs = st.total_virtual_staked_lamports();
                let net = l - st.deposit_sol_fee.apply(l);
                let overflow = st.msol_supply != 0 && tvs != 0 && (net as u128 * st.msol_supply as u128 / tvs as u128) > u64::MAX as u128;
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(l < st.min_deposit, ME::DepositAmountIsTooLow, "below min_deposit")
                    .must(l > self.bal(a), ME::NotEnoughUserFunds, "more than the actor owns")
                    .must(overflow, ME::CalculationFailure, "minted shares would not fit u64 (price < 1 and a huge amount)")
                    .may(st.staking_sol_cap != u64::MAX, ME::StakingIsCapped, "staking cap configured");
                let o = self.user_op("deposit", j, |s| s.m.deposit(&mut s.env, &s.actors[a].user, l))?;
                note = format!("{l} lamports -> {}", out_s(&o));
            }
            Op::LiquidUnstake { a, amt } => {
                let a = a as usize;
                let st = self.st();
                let msol = resolve(amt, self.msol(a), Unit::Tokens, &st);
                let avail = self.env.lamports(&self.m.sol_leg).saturating_sub(st.rent_exempt_for_token_acc);
                let value = st.msol_to_sol(msol);
                let (insufficient, too_low) = match &value {
                    Ok(v) if msol <= self.msol(a) => {
                        let fee_bp = if *v >= avail { st.liq_pool.lp_max_fee } else { st.liq_pool.linear_fee(avail - *v) };
                        let msol_fee = fee_bp.apply(msol);
                        let working = st.msol_to_sol(msol - msol_fee).unwrap_or(0);
                        (working + st.rent_exempt_for_token_acc > self.env.lamports(&self.m.sol_leg), working < st.min_withdraw)
                    }
                    _ => (false, false),
                };
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(msol > self.msol(a), ME::NotEnoughUserFunds, "more mSOL than the actor owns")
                    .must(value.is_err(), ME::CalculationFailure, "value overflow")
                    .must(insufficient, ME::InsufficientLiquidity, "liquid unstake beyond pool liquidity")
                    .must(too_low, ME::WithdrawAmountIsTooLow, "payout below min_withdraw");
                let o = self.user_op("liquid_unstake", j, |s| s.m.liquid_unstake(&mut s.env, &s.actors[a].user, msol))?;
                note = format!("{msol} mSOL -> {}", out_s(&o));
            }
            Op::AddLiquidity { a, amt } => {
                let a = a as usize;
                let st = self.st();
                let l = resolve(amt, self.bal(a), Unit::Lamports, &st);
                let leg = self.env.lamports(&self.m.sol_leg);
                let lp_supply = self.env.mint_supply(&self.m.lp_mint);
                let value = (leg - st.rent_exempt_for_token_acc) as u128 + st.msol_to_sol(self.env.token_balance(&self.m.msol_leg)).unwrap_or(0) as u128;
                let overflow = lp_supply != 0 && value != 0 && (l as u128 * lp_supply as u128 / value) > u64::MAX as u128;
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(l < st.min_deposit, ME::DepositAmountIsTooLow, "below min_deposit")
                    .must(l > self.bal(a), ME::NotEnoughUserFunds, "more than the actor owns")
                    .must(overflow, ME::CalculationFailure, "LP shares would not fit u64")
                    .may(st.liq_pool.liquidity_sol_cap != u64::MAX, ME::LiquidityIsCapped, "liquidity cap configured");
                let o = self.user_op("add_liquidity", j, |s| s.m.add_liquidity(&mut s.env, &s.actors[a].user, l))?;
                note = format!("{l} lamports -> {}", out_s(&o));
            }
            Op::RemoveLiquidity { a, amt } => {
                let a = a as usize;
                let st = self.st();
                let tokens = resolve(amt, self.lp(a), Unit::Tokens, &st);
                let supply = self.env.mint_supply(&self.m.lp_mint);
                let sol_part = if supply == 0 { tokens } else { (tokens as u128 * (self.env.lamports(&self.m.sol_leg) - st.rent_exempt_for_token_acc) as u128 / supply as u128) as u64 };
                let msol_part = if supply == 0 { tokens } else { (tokens as u128 * self.env.token_balance(&self.m.msol_leg) as u128 / supply as u128) as u64 };
                let total = sol_part as u128 + st.msol_to_sol(msol_part).unwrap_or(0) as u128;
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(tokens > self.lp(a), ME::NotEnoughUserFunds, "more LP than the actor owns")
                    .must(tokens <= self.lp(a) && total < st.min_withdraw as u128, ME::WithdrawAmountIsTooLow, "payout below min_withdraw");
                let o = self.user_op("remove_liquidity", j, |s| s.m.remove_liquidity(&mut s.env, &s.actors[a].user, tokens))?;
                note = format!("{tokens} LP -> {}", out_s(&o));
            }
            Op::OrderUnstake { a, amt } => {
                let a = a as usize;
                let st = self.st();
                let msol = resolve(amt, self.msol(a), Unit::Tokens, &st);
                let value = st.msol_to_sol(msol);
                let lamports_for_user = value.as_ref().map(|v| v - st.delayed_unstake_fee.apply(*v)).unwrap_or(0);
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(msol > self.msol(a), ME::NotEnoughUserFunds, "more mSOL than the actor owns")
                    .must(value.is_err(), ME::CalculationFailure, "value overflow")
                    .must(msol <= self.msol(a) && value.is_ok() && lamports_for_user < st.min_withdraw, ME::WithdrawAmountIsTooLow, "ticket value below min_withdraw");
                let before = self.position();
                let tol = 16 + 4 * self.price_ceil();
                let r = self.order_unstake(a, msol);
                let (res, ticket) = match r {
                    Ok(t) => (Ok(harness::env::TxMeta { logs: vec![], cu: 0, fee: 0 }), Some(t)),
                    Err(f) => (Err(f), None),
                };
                let o = self.judge("order_unstake", j, res)?;
                if ticket.is_some() && lamports_for_user == 0 {
                    stat("order_unstake: KNOWN LIMIT zero-value ticket created (unclaimable, counted forever in circulating_ticket_count)".into());
                }
                if let Some(t) = ticket {
                    self.actors[a].tickets.push(t);
                    self.slack += tol;
                    let after = self.position();
                    if after > before + tol {
                        return Err(format!("[free_lamports] order_unstake: position grew by {} (tol {tol})", after - before));
                    }
                }
                note = format!("{msol} mSOL -> {}", out_s(&o));
            }
            Op::Claim { t } => {
                let all: Vec<(usize, Address)> = self.actors.iter().enumerate().flat_map(|(i, a)| a.tickets.iter().map(move |t| (i, *t))).collect();
                let Some((owner, ticket)) = Self::pick(all, t) else { return Ok(format!("{desc}: skipped (no ticket)")) };
                let st = self.st();
                let td = self.m.ticket(&self.env, &ticket).expect("ticket");
                let clock = self.env.clock();
                let not_due = clock.epoch < td.created_epoch + 1;
                let not_ready = clock.epoch == td.created_epoch + 1 && clock.unix_timestamp - clock.epoch_start_timestamp < 30 * 60;
                let reserve_avail = self.env.lamports(&self.m.reserve).saturating_sub(st.rent_exempt_for_token_acc);
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(td.lamports_amount == 0, ME::ReusingDelayedUnstakeTicket, "zero-value ticket (already used)")
                    .must(not_due, ME::TicketNotDue, "claim before the ticket is due")
                    .must(!not_due && not_ready, ME::TicketNotReady, "first 30 minutes of the due epoch")
                    .must(!not_due && !not_ready && td.lamports_amount > reserve_avail, ME::TicketNotReady, "reserve has not received the unstaked SOL yet")
                    .known(self.reserve_desynced(), "state/mod.rs:281", "claim: reserve donation makes `available_reserve_balance -= amount` underflow");
                let before = self.bal(owner);
                let tick_lamports = self.env.lamports(&ticket);
                let pb = self.position();
                let o = self.user_op("claim", j, |s| s.m.claim(&mut s.env, &ticket, &s.actors[owner].user.pubkey()))?;
                if matches!(o, Outcome::Ok) {
                    self.actors[owner].tickets.retain(|x| *x != ticket);
                    let got = self.bal(owner) - before;
                    if got != td.lamports_amount + tick_lamports {
                        return Err(format!("[claim_amount] claim paid {got}, expected ticket {} + ticket rent {tick_lamports}", td.lamports_amount));
                    }
                    let pa = self.position();
                    if pa != pb {
                        return Err(format!("[claim_position] claim changed the combined position {pb} -> {pa}"));
                    }
                }
                note = format!("ticket {} lamports -> {}", td.lamports_amount, out_s(&o));
            }
            Op::DepositStake { a, v, amt, extra, redeposit } => {
                note = self.op_deposit_stake(a as usize, v, amt, extra, redeposit)?;
            }
            Op::WithdrawStake { a, s, amt, to } => {
                note = self.op_withdraw_stake(a as usize, s, amt, to as usize)?;
            }
            Op::StakeReserve { v, window } => {
                if window {
                    let n = self.m.slots_for_stake_delta;
                    self.env.warp_to_stake_window(n);
                }
                let st = self.st();
                let vals = self.m.validator_records(&self.env);
                let Some((vi, _)) = Self::pick(vals.iter().enumerate().collect(), v) else { return Ok(format!("{desc}: skipped (no validator)")) };
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .may(!self.in_window(), ME::TooEarlyForStakeDelta, "outside the stake-delta window (not certain: several early returns come first and succeed as no-ops)")
                    .known(self.reserve_desynced(), "state/mod.rs:281", "stake_reserve: reserve donation makes `available_reserve_balance -= stake_target` underflow")
                    .stake(st.stake_system.min_stake < self.min_deleg, "known limit: admin set min_stake below the stake program's minimum delegation, delegating a small stake reverts")
                    .stake(st.stake_system.delayed_unstake_cooling_down + st.emergency_cooling_down > 0, "known limit: stake_delta counts cooling-down SOL that is not in the reserve yet; the transfer to the new stake reverts (system InsufficientFunds) until update_deactivated ran")
                    .stake(self.window_active(), "partitioned rewards window: stake program rejects every instruction");
                let r = self.m.stake_reserve(&mut self.env, vi as u32).map(|o| o.meta);
                let o = self.judge("stake_reserve", j, r)?;
                note = out_s(&o);
            }
            Op::UpdateActive { s } => {
                let Some(r) = Self::pick(self.recs(), s) else { return Ok(format!("{desc}: skipped (no stake)")) };
                let st = self.st();
                let nval = st.validator_system.validator_count();
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(r.vi.is_none() && nval > 0, ME::WrongValidatorAccountOrIndex, "validator of this stake was removed from the list")
                    .must(r.vi.is_none() && nval == 0, ME::ListIndexOutOfBounds, "no validators listed")
                    .must(r.deact() || !r.active(), ME::RequiredActiveStake, "deactivating stake")
                    .may(self.price_overflow(), ME::CalculationFailure, "known limit: msol price overflows u64 when supply is tiny")
                    .stake(self.window_active(), "partitioned rewards window (stake withdraw of extra lamports)");
                let res = self.m.update_active(&mut self.env, r.idx, r.vi.unwrap_or(0));
                let o = self.judge("update_active", j, res)?;
                note = out_s(&o);
            }
            Op::UpdateDeactivated { s } => {
                let cands: Vec<Rec> = self.recs().into_iter().filter(|r| r.deact()).collect();
                let Some(r) = Self::pick(cands, s) else { return Ok(format!("{desc}: skipped (no deactivating stake)")) };
                let st = self.st();
                let nval = st.validator_system.validator_count();
                let cooled = r.view.delegation.as_ref().map(|d| self.env.activation(d) == Activation::Deactivated).unwrap_or(false);
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(r.vi.is_none() && nval > 0, ME::WrongValidatorAccountOrIndex, "validator of this stake was removed from the list")
                    .must(r.vi.is_none() && nval == 0, ME::ListIndexOutOfBounds, "no validators listed")
                    .may(self.price_overflow(), ME::CalculationFailure, "known limit: msol price overflows u64 when supply is tiny")
                    .stake(!cooled, "stake program refuses to withdraw a stake that is still cooling down")
                    .stake(self.window_active(), "partitioned rewards window");
                let res = self.m.update_deactivated(&mut self.env, r.idx, r.vi.unwrap_or(0));
                let o = self.judge("update_deactivated", j, res)?;
                note = out_s(&o);
            }
            Op::UpdateAll => note = self.run_crank("update_all", |c, e| c.update_all(e))?,
            Op::CanonicalMerge => note = self.run_crank("canonical_and_merge", |c, e| c.canonical_and_merge(e))?,
            Op::DoWork => note = self.run_crank("do_work", |c, e| c.do_work(e))?,
            Op::CreateCanonical { s } => {
                let Some(r) = Self::pick(self.recs(), s) else { return Ok(format!("{desc}: skipped (no stake)")) };
                let Some(vi) = r.vi else { return Ok(format!("{desc}: skipped (validator not listed)")) };
                let st = self.st();
                let vote = to_addr(&self.m.validator_records(&self.env)[vi as usize].validator_account);
                let canonical = self.m.canonical_stake(&vote);
                let exists = self.env.get_account(&canonical).map(|a| a.owner == harness::profile::addr(harness::profile::STAKE_ID)).unwrap_or(false);
                let d = r.view.delegation.clone().unwrap();
                let rent = self.env.stake_rent();
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(exists, ME::CanonicalStakeAccountAlreadyCreated, "canonical stake already exists")
                    .must(d.deactivation_epoch != u64::MAX, ME::SourceStakeMustNotBeDeactivating, "source is deactivating")
                    .must(!r.updated(), ME::SourceStakeMustBeUpdated, "source not updated this epoch")
                    .must(r.view.lamports < d.stake + rent, ME::SourceStakeMustBeUpdated, "source holds less than delegation + rent")
                    .stake(self.env.activation(&d) != Activation::FullyActive, "stake program refuses to split a not fully active stake")
                    .stake(self.chk.opts.allow_loss, "slash model: a slashed stake can fall below the stake program's minimum delegation")
                    .stake(self.window_active(), "partitioned rewards window");
                let res = self.m.create_canonical_stake(&mut self.env, r.idx, vi);
                let o = self.judge("create_canonical_stake", j, res)?;
                note = out_s(&o);
            }
            Op::MergeRaw { v, s } => {
                let vals = self.m.validator_records(&self.env);
                let Some((vi, vrec)) = Self::pick(vals.iter().enumerate().collect(), v) else { return Ok(format!("{desc}: skipped (no validator)")) };
                let vote = to_addr(&vrec.validator_account);
                let canonical = self.m.canonical_stake(&vote);
                let recs = self.recs();
                let Some(dest) = recs.iter().find(|r| r.addr == canonical) else { return Ok(format!("{desc}: skipped (no canonical stake)")) };
                let (dest_idx, dest_updated, dest_deact, dest_active, dest_view) = (dest.idx, dest.updated(), dest.deact(), dest.active(), dest.view.clone());
                let srcs: Vec<&Rec> = recs.iter().filter(|r| r.addr != canonical).collect();
                let Some(src) = Self::pick(srcs, s) else { return Ok(format!("{desc}: skipped (no source)")) };
                let st = self.st();
                let rent = self.env.stake_rent();
                let sd = src.view.delegation.clone().unwrap();
                let dd = dest_view.delegation.clone().unwrap();
                let mergeable = {
                    let k = |d: &harness::env::DelegationView| (self.env.activation(d), d.credits_observed);
                    sd.deactivation_epoch == u64::MAX && dd.deactivation_epoch == u64::MAX && k(&sd) == k(&dd)
                };
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(!dest_active || dest_deact, ME::DestinationStakeMustNotBeDeactivating, "destination deactivating")
                    .must(!dest_updated, ME::DestinationStakeMustBeUpdated, "destination not updated")
                    .must(!src.active() || sd.deactivation_epoch != u64::MAX, ME::SourceStakeMustNotBeDeactivating, "source deactivating")
                    .must(!src.updated() || src.view.lamports != sd.stake + rent, ME::SourceStakeMustBeUpdated, "source not updated / has surplus lamports (merge needs exact stake + rent)")
                    .must(sd.voter != vote, ME::InvalidSourceStakeDelegation, "source delegated to another validator")
                    .stake(!mergeable, "stake program: incompatible stakes (activation state or credits differ)")
                    .stake(self.chk.opts.allow_loss, "slash model: a slashed stake can fall below the stake program's minimum delegation")
                    .stake(self.window_active(), "partitioned rewards window");
                let res = self.m.merge_stakes(&mut self.env, dest_idx, src.idx, vi as u32);
                let o = self.judge("merge_stakes", j, res)?;
                note = out_s(&o);
            }
            Op::DeactivateStake { s, window } => {
                if window {
                    let n = self.m.slots_for_stake_delta;
                    self.env.warp_to_stake_window(n);
                }
                let Some(r) = Self::pick(self.recs(), s) else { return Ok(format!("{desc}: skipped (no stake)")) };
                let st = self.st();
                let nval = st.validator_system.validator_count();
                let delta = st.stake_delta(self.env.lamports(&self.m.reserve));
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(!r.active(), ME::RequiredActiveStake, "stake is not Active in the list")
                    .must(r.active() && r.rec.is_emergency_unstaking, ME::StakeAccountIsEmergencyUnstaking, "emergency unstaking")
                    .must(r.active() && !self.in_window(), ME::TooEarlyForStakeDelta, "outside the stake-delta window")
                    .must(r.active() && self.in_window() && delta >= 0, ME::UnstakingOnPositiveDelta, "stake delta is not negative")
                    .must(r.active() && !r.updated(), ME::StakeAccountNotUpdatedYet, "stake not updated")
                    .must(r.vi.is_none() && nval > 0, ME::WrongValidatorAccountOrIndex, "validator removed")
                    .must(r.vi.is_none() && nval == 0, ME::ListIndexOutOfBounds, "no validators")
                    .stake(r.active() && self.in_window() && delta < 0, "partial split below the stake program's minimum delegation")
                    .stake(self.chk.opts.allow_loss, "slash model: a slashed stake can fall below the stake program's minimum delegation")
                    .stake(self.window_active(), "partitioned rewards window");
                let res = self.m.deactivate_stake(&mut self.env, r.idx, r.vi.unwrap_or(0)).map(|o| o.meta);
                let o = self.judge("deactivate_stake", j, res)?;
                note = out_s(&o);
            }
            Op::PartialUnstake { s, amt } => {
                let Some(r) = Self::pick(self.recs(), s) else { return Ok(format!("{desc}: skipped (no stake)")) };
                let st = self.st();
                let nval = st.validator_system.validator_count();
                // below min_stake the program `assert!`s (a manager input error, not a state-dependent failure): clamp
                let desired = resolve(amt, r.rec.last_update_delegated_lamports, Unit::Tokens, &st).max(st.stake_system.min_stake);
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(!r.active(), ME::RequiredActiveStake, "stake is not Active in the list")
                    .must(r.active() && r.rec.is_emergency_unstaking, ME::StakeAccountIsEmergencyUnstaking, "emergency unstaking")
                    .must(r.active() && !r.updated(), ME::StakeAccountNotUpdatedYet, "stake not updated")
                    .must(r.vi.is_none() && nval > 0, ME::WrongValidatorAccountOrIndex, "validator removed")
                    .must(r.vi.is_none() && nval == 0, ME::ListIndexOutOfBounds, "no validators")
                    .may(st.max_stake_moved_per_epoch.basis_points < 10_000, ME::MovingStakeIsCapped, "per-epoch stake move cap configured")
                    .stake(r.active(), "split below the stake program's minimum delegation (surplus over target < min delegation)")
                    .stake(self.chk.opts.allow_loss, "slash model: a slashed stake can fall below the stake program's minimum delegation")
                    .stake(self.window_active(), "partitioned rewards window");
                let res = self.m.partial_unstake(&mut self.env, r.idx, r.vi.unwrap_or(0), desired).map(|o| o.meta);
                let o = self.judge("partial_unstake", j, res)?;
                note = format!("desired {desired} -> {}", out_s(&o));
            }
            Op::EmergencyUnstake { s } => {
                let Some(r) = Self::pick(self.recs(), s) else { return Ok(format!("{desc}: skipped (no stake)")) };
                let st = self.st();
                let nval = st.validator_system.validator_count();
                let score = r.vi.map(|vi| self.m.validator_records(&self.env)[vi as usize].score);
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(!r.active(), ME::RequiredActiveStake, "stake is not Active in the list")
                    .must(r.active() && r.rec.is_emergency_unstaking, ME::StakeAccountIsEmergencyUnstaking, "already emergency unstaking")
                    .must(r.active() && score.map(|x| x != 0).unwrap_or(false), ME::EmergencyUnstakingFromNonZeroScoredValidator, "validator score is not 0")
                    .must(r.active() && !r.updated(), ME::StakeAccountNotUpdatedYet, "stake not updated")
                    .must(r.vi.is_none() && nval > 0, ME::WrongValidatorAccountOrIndex, "validator removed")
                    .must(r.vi.is_none() && nval == 0, ME::ListIndexOutOfBounds, "no validators")
                    .may(st.max_stake_moved_per_epoch.basis_points < 10_000, ME::MovingStakeIsCapped, "per-epoch stake move cap configured")
                    .stake(self.window_active(), "partitioned rewards window");
                let res = self.m.emergency_unstake(&mut self.env, r.idx, r.vi.unwrap_or(0));
                let o = self.judge("emergency_unstake", j, res)?;
                note = out_s(&o);
            }
            Op::Warp { n } => {
                self.env.warp_epochs(n as u64);
            }
            Op::WarpWithRewardsWindow { n, len } => {
                self.env.warp_epochs(n as u64);
                self.env.set_epoch_rewards_active(true);
                self.window_left = len as u32 + 1;
                note = format!("epoch start; EpochRewards active for the next {len} ops");
            }
            Op::WarpToWindow => {
                let n = self.m.slots_for_stake_delta;
                self.env.warp_to_stake_window(n);
            }
            Op::Reward { s, kind } => {
                let cands: Vec<Rec> = self
                    .recs()
                    .into_iter()
                    .filter(|r| r.view.delegation.as_ref().map(|d| d.deactivation_epoch == u64::MAX && self.env.activation(d) == Activation::FullyActive).unwrap_or(false))
                    .collect();
                let Some(r) = Self::pick(cands, s) else { return Ok(format!("{desc}: skipped (no fully active stake)")) };
                let stake = r.view.delegation.as_ref().unwrap().stake;
                let amount = match kind {
                    RewardKind::OneLamport => 1,
                    RewardKind::Epoch => stake / 2200,
                    RewardKind::Big => stake / 20,
                };
                self.env.reward_stake(&r.addr, amount);
                self.rewards += amount as i128;
                note = format!("{amount} lamports to {}", r.addr);
            }
            Op::Mev { s, amt } => {
                let Some(r) = Self::pick(self.recs(), s) else { return Ok(format!("{desc}: skipped (no stake)")) };
                let st = self.st();
                let amount = resolve(amt, r.view.lamports, Unit::Tokens, &st).min(1_000_000 * LAMPORTS_PER_SOL);
                self.env.add_mev(&r.addr, amount);
                self.rewards += amount as i128;
                note = format!("{amount} lamports to {}", r.addr);
            }
            Op::Slash { s, permille } => {
                let cands: Vec<Rec> = self.recs().into_iter().filter(|r| r.view.delegation.is_some()).collect();
                let Some(r) = Self::pick(cands, s) else { return Ok(format!("{desc}: skipped (no stake)")) };
                let stake = r.view.delegation.as_ref().unwrap().stake;
                let amount = (stake as u128 * permille as u128 / 1000) as u64;
                self.env.slash_stake(&r.addr, amount);
                self.rewards -= amount as i128;
                self.chk.opts.allow_loss = true;
                note = format!("slashed {amount} lamports from {}", r.addr);
            }
            Op::RewardsWindow { len } => {
                self.env.set_epoch_rewards_active(true);
                self.window_left = len as u32 + 1; // decremented at the end of this very op
                note = format!("next {len} ops run with EpochRewards active");
            }
            Op::Donate { to, amt } => {
                let st = self.st();
                let (target, base) = match to {
                    DonateTo::Reserve => (self.m.reserve, self.env.lamports(&self.m.reserve)),
                    DonateTo::SolLeg => (self.m.sol_leg, self.env.lamports(&self.m.sol_leg)),
                    DonateTo::Stake(s) => match Self::pick(self.recs(), s) {
                        Some(r) => (r.addr, r.view.lamports),
                        None => return Ok(format!("{desc}: skipped (no stake)")),
                    },
                };
                let amount = resolve(amt, base, Unit::Tokens, &st).min(1_000_000 * LAMPORTS_PER_SOL);
                self.env.fund(&target, amount);
                self.rewards += amount as i128;
                if matches!(to, DonateTo::Reserve) {
                    self.chk.opts.exact_reserve = false;
                }
                note = format!("{amount} lamports to {target}");
            }
            Op::SetScore { v, score } => {
                let vals = self.m.validator_records(&self.env);
                let Some((vi, vr)) = Self::pick(vals.iter().enumerate().collect(), v) else { return Ok(format!("{desc}: skipped (no validator)")) };
                let st = self.st();
                let vote = to_addr(&vr.validator_account);
                let j = Judge::new().must(st.paused, ME::ProgramIsPaused, "paused");
                let r = self.m.set_validator_score(&mut self.env, vi as u32, &vote, score);
                let o = self.judge("set_validator_score", j, r)?;
                note = out_s(&o);
            }
            Op::AddValidator { v, score } => {
                let listed: Vec<Address> = self.m.validator_records(&self.env).iter().map(|x| to_addr(&x.validator_account)).collect();
                let unlisted: Vec<Address> = self.votes.iter().cloned().filter(|x| !listed.contains(x)).collect();
                let Some(vote) = Self::pick(unlisted, v) else { return Ok(format!("{desc}: skipped (all validators listed)")) };
                let st = self.st();
                let j = Judge::new().must(st.paused, ME::ProgramIsPaused, "paused");
                let r = self.m.add_validator(&mut self.env, &vote, score).map(|_| harness::env::TxMeta { logs: vec![], cu: 0, fee: 0 });
                let o = self.judge("add_validator", j, r)?;
                note = out_s(&o);
            }
            Op::RemoveValidator { v } => {
                let vals = self.m.validator_records(&self.env);
                let Some((vi, vr)) = Self::pick(vals.iter().enumerate().collect(), v) else { return Ok(format!("{desc}: skipped (no validator)")) };
                let st = self.st();
                let vote = to_addr(&vr.validator_account);
                let j = Judge::new()
                    .must(st.paused, ME::ProgramIsPaused, "paused")
                    .must(vr.active_balance > 0, ME::RemovingValidatorWithBalance, "validator still has active stake");
                let r = self.m.remove_validator(&mut self.env, vi as u32, &vote);
                let o = self.judge("remove_validator", j, r)?;
                note = out_s(&o);
            }
            Op::Config(c) => note = self.op_config(c)?,
            Op::ConfigLp(c) => note = self.op_config_lp(c)?,
            Op::Pause => {
                let st = self.st();
                let j = Judge::new().must(st.paused, ME::AlreadyPaused, "already paused");
                let r = self.m.pause(&mut self.env);
                let o = self.judge("pause", j, r)?;
                note = out_s(&o);
            }
            Op::Resume => {
                let st = self.st();
                let j = Judge::new().must(!st.paused, ME::NotPaused, "not paused");
                let r = self.m.resume(&mut self.env);
                let o = self.judge("resume", j, r)?;
                note = out_s(&o);
            }
        }
        Ok(if note.is_empty() { desc } else { format!("{desc}: {note}") })
    }

    fn order_unstake(&mut self, a: usize, msol: u64) -> Result<Address, TxFailure> {
        // the actor pays the ticket rent (and gets it back at claim), so position accounting stays exact
        let ticket = harness::Keypair::new();
        let rent = self.env.rent_exempt(TICKET_SIZE);
        let u = &self.actors[a].user;
        let ixs = [
            ix::create_ticket_account(&self.m, &u.pubkey(), &ticket.pubkey(), rent),
            ix::order_unstake(&self.m, &u.msol, &u.pubkey(), &ticket.pubkey(), msol),
        ];
        let key = harness::env::clone_keypair(&u.key);
        self.env.send(&ixs, &[&ticket, &key])?;
        Ok(ticket.pubkey())
    }

    fn op_deposit_stake(&mut self, a: usize, v: u8, amt: Amt, extra: Extra, redeposit: bool) -> Res<String> {
        // measured before the actor funds the stake account: creating + depositing it is ONE value-conserving step
        let before = self.position();
        let tol = 16 + 4 * self.price_ceil();
        let st = self.st();
        let vals = self.m.validator_records(&self.env);
        let rent = self.env.stake_rent();
        let (stake, voter, delegated) = if redeposit {
            let owned = self.actors[a].owned.clone();
            let Some(s) = Self::pick(owned, v) else { return Ok("DepositStake(redeposit): skipped (actor owns no stake)".into()) };
            let Some(view) = self.env.stake_view(&s) else { return Ok("skipped".into()) };
            let Some(d) = view.delegation.clone() else { return Ok("skipped (owned stake not delegated)".into()) };
            (s, d.voter, d.stake)
        } else {
            let pool = self.votes.clone();
            let Some(vote) = Self::pick(pool, v) else { unreachable!() };
            let delegated = resolve(amt, self.bal(a), Unit::Lamports, &st);
            if delegated == 0 || delegated > self.bal(a).saturating_sub(rent + 2 * KEEP) || self.window_active() {
                return Ok(format!("DepositStake: skipped (cannot create a {delegated} lamport stake)"));
            }
            let key = harness::env::clone_keypair(&self.actors[a].user.key);
            let s = match self.env.create_delegated_stake_from(&key, &vote, delegated) {
                Ok(s) => s,
                Err(f) if f.is_panic() => return Err(format!("[panic] stake creation: {f}")),
                Err(f) => return Ok(format!("DepositStake: stake program refused to create the stake ({delegated}): {}", f.err)),
            };
            (s, vote, delegated)
        };
        // optional lamports above delegation + rent
        let ceiling = 2_282_880u64.saturating_sub(rent);
        let extra_l = match extra {
            Extra::None => 0,
            Extra::OneLamport => 1,
            Extra::ToCeiling => ceiling,
            Extra::OverCeiling => ceiling + 1,
        };
        if extra_l > 0 && !redeposit {
            let key = harness::env::clone_keypair(&self.actors[a].user.key);
            let i = solana_system_interface::instruction::transfer(&harness::env::p(&key.pubkey()), &harness::env::p(&stake), extra_l);
            let i = harness::convert_ix!(&i);
            self.env.send(&[i], &[&key]).map_err(|e| format!("harness: top-up transfer failed: {e}"))?;
        }
        let listed = vals.iter().position(|x| to_addr(&x.validator_account) == voter);
        let stake_lamports = self.env.lamports(&stake);
        let tvs = st.total_virtual_staked_lamports();
        let net = delegated - st.deposit_stake_account_fee.apply(delegated);
        let overflow = st.msol_supply != 0 && tvs != 0 && (net as u128 * st.msol_supply as u128 / tvs as u128) > u64::MAX as u128;
        let view = self.env.stake_view(&stake);
        let d = view.as_ref().and_then(|v| v.delegation.clone());
        let deactivating = d.as_ref().map(|d| d.deactivation_epoch != u64::MAX).unwrap_or(true);
        let ceil_l = delegated + rent.max(2_282_880);
        let j = Judge::new()
            .must(st.paused, ME::ProgramIsPaused, "paused")
            .must(listed.is_none(), ME::WrongValidatorAccountOrIndex, "validator not in the list")
            .must(vals.is_empty(), ME::ListIndexOutOfBounds, "no validators listed at all")
            .anchor(listed.is_none(), "validator not in the list (duplication flag / index constraints)")
            .must(delegated < st.stake_system.min_stake, ME::TooLowDelegationInDepositingStake, "delegation below min_stake")
            .must(deactivating, ME::RequiredActiveStake, "stake is deactivating")
            .must(stake_lamports > ceil_l || stake_lamports < delegated + rent, ME::WrongStakeBalance, "stake balance outside [delegation+rent, delegation+max(rent,legacy rent)]")
            .must(overflow, ME::CalculationFailure, "minted shares would not fit u64")
            .may(st.staking_sol_cap != u64::MAX, ME::StakingIsCapped, "staking cap configured")
            .stake(self.window_active(), "partitioned rewards window");
        let vi = listed.unwrap_or(0) as u32;
        let user_key = harness::env::clone_keypair(&self.actors[a].user.key);
        let msol_acc = self.actors[a].user.msol;
        let payer = self.env.payer.pubkey();
        let vote_for_flag = voter;
        let r = self.env.send(
            &[ix::deposit_stake_account(&self.m, &stake, &user_key.pubkey(), &vote_for_flag, &payer, &msol_acc, vi)],
            &[&user_key],
        );
        let o = self.judge("deposit_stake_account", j, r)?;
        if matches!(o, Outcome::Ok) {
            self.actors[a].owned.retain(|x| *x != stake);
            self.slack += tol;
            let after = self.position();
            if after > before + tol {
                return Err(format!("[free_lamports] deposit_stake_account: position grew by {} (tol {tol})", after - before));
            }
            // the stake must be tracked and owned by the program now
            if self.m.stake_index(&self.env, &stake).is_none() {
                return Err("[stake_list] deposited stake account is not in the stake list".into());
            }
        } else if !redeposit {
            // keep the stale stake out of the position (it is the actor's: count it as owned)
            self.actors[a].owned.push(stake);
        }
        Ok(format!("{delegated} lamports (+{extra_l} extra{}) -> {}", if redeposit { ", redeposit" } else { "" }, out_s(&o)))
    }

    fn op_withdraw_stake(&mut self, a: usize, s: u8, amt: Amt, to: usize) -> Res<String> {
        let st = self.st();
        let Some(r) = Self::pick(self.recs(), s) else { return Ok("WithdrawStake: skipped (no stake)".into()) };
        let msol = resolve(amt, self.msol(a), Unit::Tokens, &st);
        let nval = st.validator_system.validator_count();
        let value = st.msol_to_sol(msol);
        let (split, value_ok) = match &value {
            Ok(v) => (v - st.withdraw_stake_account_fee.apply(*v), true),
            Err(_) => (0, false),
        };
        let l = r.rec.last_update_delegated_lamports;
        let j = Judge::new()
            .must(st.paused, ME::ProgramIsPaused, "paused")
            .must(!st.withdraw_stake_account_enabled, ME::WithdrawStakeAccountIsNotEnabled, "withdraw_stake_account disabled by admin")
            .must(msol > self.msol(a), ME::NotEnoughUserFunds, "more mSOL than the actor owns")
            .must(!r.active(), ME::RequiredActiveStake, "stake is not Active in the list")
            .must(r.active() && r.rec.is_emergency_unstaking, ME::StakeAccountIsEmergencyUnstaking, "emergency unstaking")
            .must(r.active() && r.deact(), ME::RequiredActiveStake, "stake deactivating in the stake program")
            .must(r.active() && !r.updated(), ME::StakeAccountNotUpdatedYet, "stake not updated")
            .must(r.vi.is_none() && nval > 0, ME::WrongValidatorAccountOrIndex, "validator removed")
            .must(r.vi.is_none() && nval == 0, ME::ListIndexOutOfBounds, "no validators")
            .must(!value_ok, ME::CalculationFailure, "value overflow")
            .must(value_ok && value.as_ref().map(|v| *v < st.min_withdraw).unwrap_or(false), ME::WithdrawAmountIsTooLow, "value below min_withdraw")
            .must(value_ok && split < st.stake_system.min_stake, ME::WithdrawStakeLamportsIsTooLow, "withdrawn stake below min_stake")
            .must(value_ok && split > l, ME::SelectedStakeAccountHasNotEnoughFunds, "stake account holds less than requested")
            .must(value_ok && split <= l && l - split < st.stake_system.min_stake, ME::StakeAccountRemainderTooLow, "remainder below min_stake")
            .stake(r.view.delegation.as_ref().map(|d| self.env.activation(d) != Activation::FullyActive).unwrap_or(false), "stake program split of a not fully active stake")
            .stake(split < self.min_deleg || l.saturating_sub(split) < self.min_deleg, "split / remainder below the stake program's minimum delegation (Marinade min_stake may be lower)")
            .stake(self.chk.opts.allow_loss, "slash model: a slashed stake can fall below the stake program's minimum delegation")
            .stake(self.window_active(), "partitioned rewards window");
        let split_key = harness::Keypair::new();
        let rent = self.env.stake_rent();
        if self.bal(a) < rent + KEEP {
            return Ok("WithdrawStake: skipped (actor cannot pay the split rent)".into());
        }
        let user_key = harness::env::clone_keypair(&self.actors[a].user.key);
        let msol_acc = self.actors[a].user.msol;
        let beneficiary = self.actors[to].user.pubkey();
        let stake_addr = r.addr;
        let before = self.position();
        let tol = 16 + 4 * self.price_ceil();
        let res = self.env.send(
            &[ix::withdraw_stake_account(&self.m, &stake_addr, &split_key.pubkey(), &user_key.pubkey(), &msol_acc, &user_key.pubkey(), r.idx, r.vi.unwrap_or(0), msol, &beneficiary)],
            &[&split_key, &user_key],
        );
        let o = self.judge("withdraw_stake_account", j, res)?;
        if matches!(o, Outcome::Ok) {
            self.actors[to].owned.push(split_key.pubkey());
            self.slack += tol;
            let after = self.position();
            if after > before + tol {
                return Err(format!("[free_lamports] withdraw_stake_account: position grew by {} (tol {tol})", after - before));
            }
        }
        Ok(format!("{msol} mSOL from stake #{} -> {}", r.idx, out_s(&o)))
    }

    fn op_config(&mut self, c: Cfg) -> Res<String> {
        let st = self.st();
        let mut p = ConfigMarinadeParams::default();
        let mut j = Judge::new();
        let tlc = st.total_lamports_under_control();
        match c {
            Cfg::MinDeposit(v) => p.min_deposit = Some(v),
            Cfg::MinWithdraw(v) => {
                p.min_withdraw = Some(v);
                j = j.must(v > State::MAX_WITHDRAW_ATOM, ME::MinWithdrawIsTooHigh, "above 0.1 SOL");
            }
            Cfg::MinStake(v) => {
                p.min_stake = Some(v);
                j = j.must(v < State::MIN_STAKE_LOWER_LIMIT, ME::MinStakeIsTooLow, "below 0.01 SOL");
            }
            Cfg::RewardsFeeBp(v) => {
                p.rewards_fee = Some(Fee::from_basis_points(v));
                j = j.must(v > State::MAX_REWARD_FEE.basis_points, ME::RewardsFeeIsTooHigh, "above 10%");
            }
            Cfg::DelayedUnstakeFee(v) => {
                p.delayed_unstake_fee = Some(FeeCents::from_bp_cents(v));
                j = j.must(v > State::MAX_DELAYED_UNSTAKE_FEE.bp_cents, ME::DelayedUnstakeFeeIsTooHigh, "above 0.2%");
            }
            Cfg::DepositSolFee(v) => {
                p.deposit_sol_fee = Some(FeeCents::from_bp_cents(v));
                j = j.must(v > State::MAX_DEPOSIT_SOL_FEE.bp_cents, ME::DepositSolFeeIsTooHigh, "above 0.2%");
            }
            Cfg::DepositStakeFee(v) => {
                p.deposit_stake_account_fee = Some(FeeCents::from_bp_cents(v));
                j = j.must(v > State::MAX_DEPOSIT_STAKE_ACCOUNT_FEE.bp_cents, ME::DepositStakeAccountFeeIsTooHigh, "above 0.2%");
            }
            Cfg::WithdrawStakeFee(v) => {
                p.withdraw_stake_account_fee = Some(FeeCents::from_bp_cents(v));
                j = j.must(v > State::MAX_WITHDRAW_STAKE_ACCOUNT_FEE.bp_cents, ME::WithdrawStakeAccountFeeIsTooHigh, "above 0.2%");
            }
            Cfg::WithdrawEnabled(b) => p.withdraw_stake_account_enabled = Some(b),
            Cfg::StakingCap(k) => {
                p.staking_sol_cap = Some(match k {
                    0 => u64::MAX,
                    1 => tlc,
                    _ => tlc + 1000 * LAMPORTS_PER_SOL,
                })
            }
            Cfg::LiquidityCap(k) => {
                let leg = self.env.lamports(&self.m.sol_leg);
                p.liquidity_sol_cap = Some(match k {
                    0 => u64::MAX,
                    1 => leg,
                    _ => leg + 1000 * LAMPORTS_PER_SOL,
                })
            }
            Cfg::MaxStakeMovedBp(v) => p.max_stake_moved_per_epoch = Some(Fee::from_basis_points(v)),
        }
        let r = self.m.config_marinade(&mut self.env, p);
        let o = self.judge("config_marinade", j, r)?;
        Ok(format!("{c:?} -> {}", out_s(&o)))
    }

    fn op_config_lp(&mut self, c: LpCfg) -> Res<String> {
        let st = self.st();
        let lp = &st.liq_pool;
        let min_fee = c.min_fee.unwrap_or(lp.lp_min_fee.basis_points);
        let max_fee = c.max_fee.unwrap_or(lp.lp_max_fee.basis_points);
        let target = c.target_sol.map(|s| s as u64 * LAMPORTS_PER_SOL).unwrap_or(lp.lp_liquidity_target);
        let cut = c.treasury_cut.unwrap_or(lp.treasury_cut.basis_points);
        let j = Judge::new()
            .must(min_fee > 10_000 || max_fee > 10_000 || cut > 10_000, ME::BasisPointsOverflow, "fee above 100%")
            .must(max_fee > 1000, ME::LpMaxFeeIsTooHigh, "max fee above 10%")
            .must(max_fee < min_fee, ME::LpFeesAreWrongWayRound, "min fee above max fee")
            .must(target < marinade_finance::state::liq_pool::LiqPool::MIN_LIQUIDITY_TARGET, ME::LiquidityTargetTooLow, "target below 50 SOL")
            .must(cut > 7500, ME::TreasuryCutIsTooHigh, "treasury cut above 75%");
        let p = ConfigLpParams {
            min_fee: c.min_fee.map(Fee::from_basis_points),
            max_fee: c.max_fee.map(Fee::from_basis_points),
            liquidity_target: c.target_sol.map(|s| s as u64 * LAMPORTS_PER_SOL),
            treasury_cut: c.treasury_cut.map(Fee::from_basis_points),
        };
        let r = self.m.config_lp(&mut self.env, p);
        let o = self.judge("config_lp", j, r)?;
        Ok(format!("-> {}", out_s(&o)))
    }

    // ------------------------------------------------------------ cranks

    /// Run a crank body (lenient, invariants after every successful crank op). Any failing crank op is a finding
    /// unless the program is paused or the partitioned-rewards window is open (stake program rejects everything).
    fn run_crank(&mut self, name: &str, body: impl FnOnce(&mut Crank, &mut TestEnv) -> anyhow::Result<()>) -> Res<String> {
        let paused = self.st().paused;
        let window = self.window_active();
        let desync_before = self.reserve_desynced();
        let Sim { env, m, chk, .. } = self;
        let m: &Marinade = m;
        let mut inv: Option<String> = None;
        let (res, report) = {
            let mut c = Crank::new(m).lenient().with_hook(|e, op| match chk.check_after(e, m, op) {
                Err(msg) => {
                    inv = Some(msg.clone());
                    Err(msg)
                }
                ok => ok,
            });
            let r = body(&mut c, env);
            (r, c.report.clone())
        };
        if let Some(msg) = inv {
            return Err(msg);
        }
        if let Err(e) = res {
            return Err(format!("[crank_error] {name}: {e:#}"));
        }
        let mut bad = vec![];
        for f in report.failures() {
            let is_pause = f.marinade_code == Some(u32::from(ME::ProgramIsPaused));
            if (paused && is_pause) || window {
                stat(format!("{name}: op rejected (paused/rewards window)"));
                continue;
            }
            // known limit: a partial deactivate_stake splits `min(delta, validator surplus)`; when that is below the
            // stake program's minimum delegation the split reverts (docs/findings: tests/harness/tests/findings.rs)
            if f.error.as_deref().map(|e| e.contains("state/mod.rs:281")).unwrap_or(false) && (desync_before || self.reserve_desynced()) {
                stat(format!("{name}: KNOWN BUG panic (crank {}: reserve donation makes `available_reserve_balance -= x` underflow)", f.name));
                continue;
            }
            // a slashed (model) stake can fall below the stake program's minimum delegation: Split/Merge then refuse it
            if f.marinade_code == Some(u32::from(ME::CalculationFailure)) && self.price_overflow() {
                self.price_overflow_hits += 1;
                stat(format!("{name}: {} reverted: msol price (2^32 * tvs / supply) overflows u64 (known limit, tiny supply)", f.name));
                continue;
            }
            if self.chk.opts.allow_loss && f.error.as_deref().map(|e| e.contains("Custom(12)")).unwrap_or(false) {
                stat(format!("{name}: stake op refused for a slashed stake below the minimum delegation (slash model artifact)"));
                continue;
            }
            if f.name.starts_with("stake_reserve") && f.error.as_deref().map(|e| e.contains("insufficient lamports")).unwrap_or(false) && self.st().total_cooling_down() > 0 {
                stat(format!("{name}: stake_reserve over-stakes cooling-down SOL not yet in the reserve (known limit)"));
                continue;
            }
            if f.name.starts_with("stake_reserve") && f.error.as_deref().map(|e| e.contains("Custom(12)")).unwrap_or(false) && self.st().stake_system.min_stake < self.min_deleg {
                stat(format!("{name}: stake_reserve below stake-program minimum delegation (admin min_stake < min delegation, known limit)"));
                continue;
            }
            if f.name.starts_with("deactivate_stake") && f.error.as_deref().map(|e| e.contains("Custom(12)")).unwrap_or(false) {
                self.min_deleg_hits += 1;
                stat(format!("{name}: deactivate_stake split below stake-program minimum delegation (known limit)"));
                continue;
            }
            bad.push(format!("{}: {}", f.name, f.error.clone().unwrap_or_default()));
        }
        if !bad.is_empty() {
            return Err(format!("[crank_failure] {name}: crank ops failed in a healthy state:\n    {}", bad.join("\n    ")));
        }
        stat(format!("{name}: ok"));
        Ok(format!("{} crank ops", report.ops.len()))
    }

    // ------------------------------------------------------------ driver

    fn run_case(&mut self, case: &Case, target: Option<&Runtime>) -> Res<()> {
        for i in 0..=case.ops.len() {
            if i == case.transition_at.min(case.ops.len()) && !self.transitioned {
                if let Some(t) = target {
                    self.do_transition(t)?;
                }
            }
            if i == case.ops.len() {
                break;
            }
            let op = &case.ops[i];
            let line = match self.exec(op) {
                Ok(l) => l,
                Err(e) => {
                    self.trace.push(format!("{i:>2}: {op:?}  <-- FAILED"));
                    return Err(e);
                }
            };
            self.nops += 1;
            self.trace.push(format!("{i:>2}: {line}"));
            if self.window_left > 0 {
                self.window_left -= 1;
                if self.window_left == 0 {
                    self.env.set_epoch_rewards_active(false);
                }
            }
            self.check(&format!("op #{i} {op:?}")).map_err(|e| {
                self.trace.push(format!("{i:>2}: <-- invariant check failed"));
                e
            })?;
        }
        self.finish()
    }

    fn do_transition(&mut self, target: &Runtime) -> Res<()> {
        self.env.set_epoch_rewards_active(false);
        self.window_left = 0;
        self.env.warp_epochs(1);
        let frozen: Vec<u64> = self.recs().iter().map(|r| r.view.rent_exempt_reserve).collect();
        self.env.transition(target).map_err(|e| format!("transition failed: {e:#}"))?;
        // A rent DROP releases lamports: the stake program freezes Meta.rent_exempt_reserve, the program derives the
        // rent part from the live Rent and sweeps the rest into the reserve like MEV (update_active / update_deactivated /
        // merge). It is a windfall for mSOL holders, so it counts as a reward in the final accounting.
        let live = self.env.stake_rent();
        let windfall: u64 = frozen.iter().map(|f| f.saturating_sub(live)).sum();
        self.rewards += windfall as i128;
        self.trace.push(format!("transition: rent windfall from {} tracked stakes: {windfall} lamports", frozen.len()));
        self.transitioned = true;
        self.trace.push(format!("---- TRANSITION: stake rent now {} ----", self.env.stake_rent()));
        self.check("transition")
    }

    /// Full crank in the next epoch, `check_fully_updated`, every actor exits, `out <= in + rewards`.
    fn finish(&mut self) -> Res<()> {
        self.trace.push("== end of case ==".into());
        self.env.set_epoch_rewards_active(false);
        self.window_left = 0;
        if self.st().paused {
            self.m.resume(&mut self.env).map_err(|e| format!("resume at end: {e}"))?;
        }
        // restore permissive limits so the exit is not blocked by a hostile configuration the fuzzer set
        let p = ConfigMarinadeParams {
            staking_sol_cap: Some(u64::MAX),
            liquidity_sol_cap: Some(u64::MAX),
            max_stake_moved_per_epoch: Some(Fee::from_basis_points(10_000)),
            withdraw_stake_account_enabled: Some(true),
            ..Default::default()
        };
        self.m.config_marinade(&mut self.env, p).map_err(|e| format!("end config: {e}"))?;
        // stakes of removed validators can only be updated through a listed validator: re-add them (manager recovery)
        self.readd_orphaned_validators()?;
        self.check("end: restore")?;

        self.env.warp_epochs(1);
        self.run_crank("end do_work", |c, e| c.do_work(e))?;
        self.check("end: do_work")?;
        if !self.chk.opts.allow_loss && self.price_overflow_hits == 0 && std::env::var("FUZZ_SKIP_FULLY_UPDATED").is_err() {
            let viol = harness::invariants::check_fully_updated(&self.env, &self.m);
            if !viol.is_empty() {
                return Err(format!("[fully_updated] after a full crank: {viol:?}"));
            }
        }
        self.exit_all()?;
        // final accounting
        let injected_in: u128 = NA as u128 * sol(ACTOR_SOL) as u128;
        let mut out: u128 = 0;
        for (i, a) in self.actors.iter().enumerate() {
            out += self.bal(i) as u128;
            for s in &a.owned {
                out += self.env.lamports(s) as u128;
            }
        }
        let allowed = injected_in as i128 + self.rewards + self.slack as i128;
        self.trace.push(format!("exit: out {out} in {injected_in} rewards {} slack {}", self.rewards, self.slack));
        if out as i128 > allowed {
            return Err(format!(
                "[extraction] actors ended with {out} lamports (in {injected_in} + rewards/donations {} + rounding slack {}): {} lamports too many",
                self.rewards,
                self.slack,
                out as i128 - allowed
            ));
        }
        Ok(())
    }

    fn readd_orphaned_validators(&mut self) -> Res<()> {
        let listed: Vec<Address> = self.m.validator_records(&self.env).iter().map(|x| to_addr(&x.validator_account)).collect();
        let mut need: Vec<Address> = vec![];
        for r in self.recs() {
            if let Some(d) = &r.view.delegation {
                if !listed.contains(&d.voter) && !need.contains(&d.voter) {
                    need.push(d.voter);
                }
            }
        }
        for v in need {
            self.m.add_validator(&mut self.env, &v, 0).map_err(|e| format!("[readd_validator] {e}"))?;
            self.trace.push(format!("end: re-added validator {v} (had tracked stake accounts)"));
        }
        Ok(())
    }

    fn exit_note(&mut self, what: &str) {
        let st = self.st();
        self.trace.push(format!("exit {what}: tvs {} supply {} tickets {} reserve {} legmsol {} legsol {} actors msol {:?}", st.total_virtual_staked_lamports(), st.msol_supply, st.circulating_ticket_balance, self.env.lamports(&self.m.reserve), self.env.token_balance(&self.m.msol_leg), self.env.lamports(&self.m.sol_leg), (0..NA).map(|i| self.msol(i)).collect::<Vec<_>>()));
    }

    fn exit_all(&mut self) -> Res<()> {
        // twice: dust that was below min_withdraw can be worth more after the other actors paid their fees
        for _ in 0..2 {
            self.exit_pass()?;
            self.settle_tickets()?;
        }
        let min_w = self.st().min_withdraw.max(1);
        let st = self.st();
        for a in 0..NA {
            let v = st.msol_to_sol(self.msol(a)).unwrap_or(0);
            if v >= min_w * 2 && !self.chk.opts.allow_loss {
                return Err(format!("[exit] actor {a} still holds {} mSOL (worth {v} lamports) after exiting", self.msol(a)));
            }
        }
        Ok(())
    }

    fn exit_pass(&mut self) -> Res<()> {
        self.exit_note("start");
        // 1. liquid unstake, fall back to a delayed unstake
        for a in 0..NA {
            let bal = self.msol(a);
            if bal == 0 {
                continue;
            }
            let r = self.m.liquid_unstake(&mut self.env, &self.actors[a].user, bal);
            match r {
                Ok(_) => {}
                Err(f) if f.is_marinade(ME::InsufficientLiquidity) || f.is_marinade(ME::WithdrawAmountIsTooLow) => {
                    self.delayed_exit(a)?;
                }
                Err(f) => return Err(format!("[exit] liquid_unstake of all mSOL failed unexpectedly: {f}")),
            }
            self.check("exit: unstake")?;
            self.exit_note(&format!("after unstake of actor {a}"));
        }
        // 2. LP out (returns SOL + mSOL), then delayed unstake of the received mSOL
        for a in 0..NA {
            let lp = self.lp(a);
            if lp == 0 {
                continue;
            }
            match self.m.remove_liquidity(&mut self.env, &self.actors[a].user, lp) {
                Ok(_) => {}
                Err(f) if f.is_marinade(ME::WithdrawAmountIsTooLow) => {}
                Err(f) => return Err(format!("[exit] remove_liquidity of all LP failed unexpectedly: {f}")),
            }
            self.check("exit: remove_liquidity")?;
        }
        for a in 0..NA {
            if self.msol(a) > 0 {
                self.delayed_exit(a)?;
                self.check("exit: delayed unstake")?;
            }
        }
        Ok(())
    }

    fn settle_tickets(&mut self) -> Res<()> {
        // 3. crank + claim until every ticket is paid (bounded)
        let mut limit_stuck = false;
        let mut known_bug_hit = false;
        for round in 0..6 {
            if self.live_tickets().is_empty() {
                break;
            }
            self.env.warp_epochs(1);
            let hits = self.min_deleg_hits + self.price_overflow_hits;
            self.run_crank("exit do_work", |c, e| c.do_work(e))?;
            limit_stuck = self.min_deleg_hits + self.price_overflow_hits > hits;
            self.check(&format!("exit: do_work round {round}"))?;
            let all: Vec<(usize, Address)> = self.actors.iter().enumerate().flat_map(|(i, a)| a.tickets.iter().map(move |t| (i, *t))).collect();
            for (owner, t) in all {
                if self.m.ticket(&self.env, &t).map(|d| d.lamports_amount == 0).unwrap_or(false) {
                    continue; // zero-value ticket: unclaimable by design (known limit)
                }
                let b = self.actors[owner].user.pubkey();
                match self.m.claim(&mut self.env, &t, &b) {
                    Ok(_) => self.actors[owner].tickets.retain(|x| *x != t),
                    Err(f) if f.is_marinade(ME::TicketNotReady) || f.is_marinade(ME::TicketNotDue) => {}
                    Err(f) if f.is_panic() && f.logs.join("\n").contains("state/mod.rs:281") && self.reserve_desynced() => {
                        stat("exit: KNOWN BUG panic (claim: reserve donation makes `available_reserve_balance -= amount` underflow)".into());
                        known_bug_hit = true;
                    }
                    Err(f) => return Err(format!("[exit] claim failed unexpectedly: {f}")),
                }
                self.check("exit: claim")?;
            }
        }
        let stuck: Vec<String> = self.live_tickets().iter().map(|t| format!("{t}")).collect();
        if !stuck.is_empty() && known_bug_hit {
            stat("exit: tickets left unclaimable (KNOWN BUG: claim panics while the reserve holds donated lamports)".into());
        } else if !stuck.is_empty() && limit_stuck {
            // known limit: the remaining unstake is smaller than the stake program's minimum delegation
            stat("exit: tickets left unserved (deactivate split below minimum delegation, known limit)".into());
        } else if !stuck.is_empty() && !self.chk.opts.allow_loss {
            let st = self.st();
            return Err(format!(
                "[exit] tickets could not be claimed after 6 epochs of cranking: {stuck:?}; state: tickets {} reserve {} active {} cooling {}+{}",
                st.circulating_ticket_balance,
                self.env.lamports(&self.m.reserve),
                st.validator_system.total_active_balance,
                st.stake_system.delayed_unstake_cooling_down,
                st.emergency_cooling_down
            ));
        }
        Ok(())
    }

    fn live_tickets(&self) -> Vec<Address> {
        self.actors
            .iter()
            .flat_map(|a| a.tickets.iter().cloned())
            .filter(|t| self.m.ticket(&self.env, t).map(|d| d.lamports_amount > 0).unwrap_or(false))
            .collect()
    }

    fn delayed_exit(&mut self, a: usize) -> Res<()> {
        let bal = self.msol(a);
        match self.order_unstake(a, bal) {
            Ok(t) => {
                self.actors[a].tickets.push(t);
                Ok(())
            }
            Err(f) if f.is_marinade(ME::WithdrawAmountIsTooLow) => Ok(()), // dust
            Err(f) => Err(format!("[exit] order_unstake of all mSOL failed unexpectedly: {f}")),
        }
    }
}

fn out_s(o: &Outcome) -> String {
    match o {
        Outcome::Ok => "ok".into(),
        Outcome::Rejected(n) => format!("rejected ({n})"),
    }
}


// ===================================================================================== genesis

fn new_genesis(spec: &str, warm: bool) -> anyhow::Result<Genesis> {
    let mut env = harness::env::env_from_spec(spec)?;
    let m = Marinade::setup(&mut env, &SetupOpts::default())?;
    let mut votes = vec![];
    for i in 0..NV {
        let v = env.create_vote_account();
        if i < 3 {
            m.add_validator(&mut env, &v, 100).map_err(|e| anyhow::anyhow!("add_validator: {e}"))?;
        }
        votes.push(v);
    }
    let actors: Vec<Actor> = (0..NA).map(|_| Actor { user: m.new_user(&mut env, sol(ACTOR_SOL)), owned: vec![], tickets: vec![] }).collect();
    let min_deleg = probe_min_delegation(&env, &votes[0]);
    let mut g = Genesis { env, m, actors, votes, rewards: 0, min_deleg };
    if warm {
        warm_up(&mut g)?;
    }
    Ok(g)
}

/// Smallest of a few candidate amounts the stake program accepts as a delegation (on a scratch clone).
fn probe_min_delegation(env: &TestEnv, vote: &Address) -> u64 {
    let mut e = env.clone();
    let k = e.new_funded(sol(100));
    for amt in [1u64, 1_000_000, 10_000_000, 100_000_000, 500_000_000, LAMPORTS_PER_SOL] {
        if e.create_delegated_stake_from(&k, vote, amt).is_ok() {
            return amt;
        }
    }
    LAMPORTS_PER_SOL
}

/// Same shape as the transition lifecycle: liquidity + deposits, stake accounts on 3 validators over several
/// epochs (some merged), rewards, a pending delayed unstake whose stake is cooling down.
fn warm_up(g: &mut Genesis) -> anyhow::Result<()> {
    let Genesis { env, m, actors, rewards, .. } = g;
    let mut chk = Checker::new(env, m);
    macro_rules! ok {
        ($what:expr, $r:expr) => {{
            $r.map_err(|e| anyhow::anyhow!("warm-up {}: {e}", $what))?;
            chk.check_after(env, m, $what).map_err(|e| anyhow::anyhow!("{e}"))?;
        }};
    }
    ok!("add_liquidity", m.add_liquidity(env, &actors[1].user, sol(300)));
    ok!("deposit", m.deposit(env, &actors[0].user, sol(600)));
    crank(env, m, &mut chk, |c, e| c.stake_delta(e))?;
    env.warp_epochs(1);
    crank(env, m, &mut chk, |c, e| c.update_all(e))?;
    ok!("deposit", m.deposit(env, &actors[2].user, sol(400)));
    crank(env, m, &mut chk, |c, e| c.stake_delta(e))?;
    env.warp_epochs(1);
    for s in m.indexed_stakes(env) {
        env.reward_stake(&to_addr(&s.record.stake_account), sol(2));
        *rewards += sol(2) as i128;
    }
    crank(env, m, &mut chk, |c, e| c.update_all(e))?;
    crank(env, m, &mut chk, |c, e| c.canonical_and_merge(e))?;
    let msol = env.token_balance(&actors[0].user.msol);
    let t = m.order_unstake(env, &actors[0].user, msol / 5);
    let t = t.map_err(|e| anyhow::anyhow!("warm-up order_unstake: {e}"))?;
    actors[0].tickets.push(t);
    chk.check_after(env, m, "order_unstake").map_err(|e| anyhow::anyhow!("{e}"))?;
    crank(env, m, &mut chk, |c, e| c.stake_delta(e))?;
    // the genesis ledger starts here
    Ok(())
}

fn crank(env: &mut TestEnv, m: &Marinade, chk: &mut Checker, body: impl FnOnce(&mut Crank, &mut TestEnv) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let mut c = Crank::new(m).with_hook(|e, op| chk.check_after(e, m, op));
    body(&mut c, env)
}

// ===================================================================================== runner

struct Shared {
    base: String,
    target: String,
    target_rt: Option<Runtime>,
    cold: Genesis,
    warm: Genesis,
}

fn shared() -> Shared {
    {
        let base = profile::base_spec();
        let target = profile::target_spec();
        let t0 = Instant::now();
        let cold = new_genesis(&base, false).expect("cold genesis");
        let t_cold = t0.elapsed();
        let t1 = Instant::now();
        let warm = new_genesis(&base, true).expect("warm genesis");
        eprintln!("[fuzz] genesis: cold env {:?} (setup + 3 validators + 4 actors), warm env {:?}", t_cold, t1.elapsed());
        let t2 = Instant::now();
        let c = cold.clone();
        eprintln!("[fuzz] cloning a genesis env: {:?} ({} txs in the warm one)", t2.elapsed(), c.env.tx_count);
        let target_rt = if target != base { Some(profile::resolve(&target).expect("resolve HARNESS_TARGET")) } else { None };
        Shared { base, target, target_rt, cold, warm }
    }
}

fn run_one(sh: &Shared, case: &Case) -> Result<(u64, u64), String> {
    let mut sim = Sim::fork(if case.warm { &sh.warm } else { &sh.cold });
    let tx0 = sim.env.tx_count;
    let r = sim.run_case(case, sh.target_rt.as_ref());
    let txs = sim.env.tx_count - tx0;
    match r {
        Ok(()) => Ok((sim.nops, txs)),
        Err(e) => {
            let mut msg = format!("{e}\n\ntrace:\n");
            for l in &sim.trace {
                msg.push_str("  ");
                msg.push_str(l);
                msg.push('\n');
            }
            Err(msg)
        }
    }
}

#[test]
fn stateful_fuzz() {
    let sh = shared();
    let (base, target) = (sh.base.clone(), sh.target.clone());
    let sha = sh.cold.env.marinade_so.sha256.clone();
    let sh = std::panic::AssertUnwindSafe(sh);
    let (rb, rt) = (base.clone(), target.clone());
    harness::report::run_reported("fuzz::stateful", &rb, &rt, &sha, move || {
        let sh: &Shared = &sh;
        let default_cases = 1000;
        let cases: u32 = std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(default_cases);
        let seed: u64 = std::env::var("FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x4d41_5249_4e41_4445);
        let shrink: u32 = std::env::var("FUZZ_SHRINK_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
        let persist = std::env::var("FUZZ_NO_PERSIST").map(|v| v != "1").unwrap_or(true);
        let reg: &'static str = Box::leak(
            std::env::var("FUZZ_REGRESSIONS")
                .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/proptest-regressions/fuzz.txt").to_string())
                .into_boxed_str(),
        );
        let config = Config {
            cases,
            max_shrink_iters: shrink,
            failure_persistence: if persist { Some(Box::new(FileFailurePersistence::Direct(reg))) } else { None },
            rng_algorithm: RngAlgorithm::ChaCha,
            rng_seed: RngSeed::Fixed(seed),
            ..Config::default()
        };
        eprintln!("[fuzz] base={base:?} target={target:?} cases={cases} max_ops={} FUZZ_SEED={seed} (persist: {persist})", max_ops());
        let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &{
            let mut s = [0u8; 32];
            s[..8].copy_from_slice(&seed.to_le_bytes());
            s
        }));
        let t0 = Instant::now();
        let totals = Mutex::new((0u64, 0u64, 0u64));
        let res = runner.run(&case_s(), |case| {
            match run_one(sh, &case) {
                Ok((ops, txs)) => {
                    let mut t = totals.lock().unwrap();
                    t.0 += 1;
                    t.1 += ops;
                    t.2 += txs;
                    Ok(())
                }
                Err(e) => Err(TestCaseError::fail(e)),
            }
        });
        let el = t0.elapsed().as_secs_f64();
        let (nc, nops, ntx) = *totals.lock().unwrap();
        eprintln!(
            "[fuzz] {nc} passing cases in {el:.1}s = {:.2} cases/s ({:.1} ops/case, {:.0} txs/s incl. shrinking)",
            nc as f64 / el,
            nops as f64 / nc.max(1) as f64,
            ntx as f64 / el
        );
        let stats = STATS.lock().unwrap();
        eprintln!("[fuzz] op outcome coverage:");
        for (k, v) in stats.iter() {
            eprintln!("    {v:>8}  {k}");
        }
        match res {
            Ok(()) => Ok(()),
            Err(TestError::Fail(reason, case)) => {
                anyhow::bail!("FUZZ FAILURE (FUZZ_SEED={seed}; minimal case after shrinking):\n{case:?}\n\n{reason}")
            }
            Err(TestError::Abort(r)) => anyhow::bail!("fuzz aborted: {r}"),
        }
    });
}

/// Debug helper: replay a hand-written case with a trace and state dumps
/// (`cargo test --test fuzz replay -- --ignored --nocapture`). Edit the ops below.
#[test]
#[ignore]
fn replay() {
    let sh = shared();
    let case = Case { warm: false, transition_at: 99, ops: vec![Op::Deposit { a: 0, amt: Amt::Sol(1) }] };
    let mut sim = Sim::fork(if case.warm { &sh.warm } else { &sh.cold });
    let r = sim.run_case(&case, sh.target_rt.as_ref());
    for l in &sim.trace {
        eprintln!("   {l}");
    }
    let st = sim.st();
    eprintln!("result {r:?}; tvs {} supply {} tlc {}", st.total_virtual_staked_lamports(), st.msol_supply, st.total_lamports_under_control());
    assert!(r.is_ok());
}
