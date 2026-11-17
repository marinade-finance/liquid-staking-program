//! The simulated chain around the program: two Marinade instances (A under test, B as the
//! "someone else's instance" source of look-alike accounts), users, an attacker, validators,
//! decoy accounts, and readers that deserialize program state natively.

use crate::ixs::{self, Ix};
use crate::util::*;
use anchor_lang::{AccountDeserialize, AnchorDeserialize};
use marinade_finance::instructions::admin::{ConfigMarinadeParams, InitializeData, LiqPoolInitializeData};
use marinade_finance::state::delayed_unstake_ticket::TicketAccountData;
use marinade_finance::state::fee::{Fee, FeeCents};
use marinade_finance::state::liq_pool::LiqPool;
use marinade_finance::state::stake_system::{StakeRecord, StakeSystem};
use marinade_finance::state::validator_system::{ValidatorRecord, ValidatorSystem};
use marinade_finance::State;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const TICKET_SIZE: usize = 8 + 32 + 32 + 8 + 8;

#[derive(Clone, Debug)]
pub struct Inst {
    pub label: &'static str,
    pub state: Pubkey,
    pub admin: Pubkey,
    pub vmgr: Pubkey,
    pub pauser: Pubkey,
    pub operational: Pubkey,
    pub treasury_owner: Pubkey,
    pub msol_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub msol_leg: Pubkey,
    pub treasury_msol: Pubkey,
    pub reserve: Pubkey,
    pub sol_leg: Pubkey,
    pub stake_list: Pubkey,
    pub validator_list: Pubkey,
    pub deposit_auth: Pubkey,
    pub withdraw_auth: Pubkey,
    pub msol_mint_auth: Pubkey,
    pub lp_mint_auth: Pubkey,
    pub msol_leg_auth: Pubkey,
}

impl Inst {
    pub fn dup_flag(&self, vote: &Pubkey) -> Pubkey {
        ValidatorRecord::find_duplication_flag(&self.state, vote).0
    }
    pub fn canonical_stake(&self, vote: &Pubkey) -> Pubkey {
        State::find_canonical_stake_address(&self.state, vote).0
    }
}

#[derive(Clone, Debug)]
pub struct User {
    pub key: Pubkey,
    pub msol: Pubkey,
    pub lp: Pubkey,
}

#[derive(Clone, Debug)]
pub struct Cfg {
    pub min_stake: u64,
    pub reward_fee_bp: u32,
    pub stake_capacity: u32,
    pub validator_capacity: u32,
    pub slots_for_stake_delta: u64,
    pub lp_target: u64,
}

impl Cfg {
    pub fn random(t: &mut Trident) -> Cfg {
        let pick = |t: &mut Trident, v: &[u64]| v[t.random_from_range(0..v.len())];
        Cfg {
            min_stake: pick(t, &[SOL / 100, SOL / 2, SOL, 3 * SOL]),
            reward_fee_bp: pick(t, &[0, 300, 1000]) as u32,
            stake_capacity: pick(t, &[2, 4, 8, 32]) as u32,
            validator_capacity: pick(t, &[2, 4, 8]) as u32,
            slots_for_stake_delta: pick(t, &[3_000, 10_000, 100_000]),
            lp_target: pick(t, &[50 * SOL, 100 * SOL, 1000 * SOL]),
        }
    }
}

pub struct World {
    pub cfg: Cfg,
    pub a: Inst,
    pub b: Inst,
    pub c: Inst,
    pub users: Vec<User>,
    pub attacker: User,
    pub votes: Vec<Pubkey>,
    pub spare_votes: Vec<Pubkey>,
    pub fake_vote: Pubkey,
    pub tags: HashMap<Pubkey, Vec<&'static str>>,
    pub pool: Vec<Pubkey>,
    pub tracked: BTreeSet<Pubkey>,
    pub tickets: Vec<Pubkey>,
    pub spare_tickets: Vec<Pubkey>,
    pub user_stakes: Vec<(Pubkey, Pubkey)>,
    pub donated: bool,
    pub slashed: bool,
    pub paused_ok: bool,
    pub prev: Option<crate::invariants::Snap>,
    pub last_lamports: BTreeMap<Pubkey, u64>,
}

pub fn new_inst(t: &mut Trident, label: &'static str, cfg: &Cfg) -> Inst {
    let state = t.random_pubkey();
    let reserve = State::find_reserve_address(&state).0;
    let msol_mint_auth = State::find_msol_mint_authority(&state).0;
    let lp_mint_auth = LiqPool::find_lp_mint_authority(&state).0;
    let sol_leg = LiqPool::find_sol_leg_address(&state).0;
    let msol_leg_auth = LiqPool::find_msol_leg_authority(&state).0;
    let deposit_auth = StakeSystem::find_stake_deposit_authority(&state).0;
    let withdraw_auth = StakeSystem::find_stake_withdraw_authority(&state).0;
    let msol_mint = t.random_pubkey();
    let lp_mint = t.random_pubkey();
    let treasury_owner = t.random_pubkey();
    let treasury_msol = t.random_pubkey();
    let msol_leg = t.random_pubkey();
    let stake_list = t.random_pubkey();
    let validator_list = t.random_pubkey();
    let operational = t.random_pubkey();
    let inst = Inst {
        label,
        state,
        admin: t.random_pubkey(),
        vmgr: t.random_pubkey(),
        pauser: t.random_pubkey(),
        operational,
        treasury_owner,
        msol_mint,
        lp_mint,
        msol_leg,
        treasury_msol,
        reserve,
        sol_leg,
        stake_list,
        validator_list,
        deposit_auth,
        withdraw_auth,
        msol_mint_auth,
        lp_mint_auth,
        msol_leg_auth,
    };
    put_mint(t, &msol_mint, &msol_mint_auth, 0);
    put_mint(t, &lp_mint, &lp_mint_auth, 0);
    put_token_account(t, &treasury_msol, &msol_mint, &treasury_owner, 0);
    put_token_account(t, &msol_leg, &msol_mint, &msol_leg_auth, 0);
    let token_rent = rent_min(t, 165);
    for pda in [reserve, sol_leg] {
        put(t, &pda, token_rent, vec![], &system_id());
    }
    let pid = program_id();
    let l = rent_min(t, State::serialized_len());
    put(t, &state, l, vec![0u8; State::serialized_len()], &pid);
    let n = StakeSystem::bytes_for_list(cfg.stake_capacity, 0) as usize;
    let l = rent_min(t, n);
    put(t, &stake_list, l, vec![0u8; n], &pid);
    let n = ValidatorSystem::bytes_for_list(cfg.validator_capacity, 0) as usize;
    let l = rent_min(t, n);
    put(t, &validator_list, l, vec![0u8; n], &pid);
    put(t, &operational, 10 * SOL, vec![], &system_id());
    for k in [inst.admin, inst.vmgr, inst.pauser] {
        put(t, &k, SOL, vec![], &system_id());
    }
    inst
}

pub fn init_data(a: &Inst, cfg: &Cfg) -> InitializeData {
    InitializeData {
        admin_authority: a.admin,
        validator_manager_authority: a.vmgr,
        min_stake: cfg.min_stake,
        rewards_fee: Fee::from_basis_points(cfg.reward_fee_bp),
        liq_pool: LiqPoolInitializeData {
            lp_liquidity_target: cfg.lp_target,
            lp_max_fee: Fee::from_basis_points(300),
            lp_min_fee: Fee::from_basis_points(30),
            lp_treasury_cut: Fee::from_basis_points(2500),
        },
        additional_stake_record_space: 0,
        additional_validator_record_space: 0,
        slots_for_stake_delta: cfg.slots_for_stake_delta,
        pause_authority: a.pauser,
    }
}

pub struct Outcome {
    pub ok: bool,
    pub err: String,
    pub panicked: bool,
    pub code: Option<u32>,
    pub logs: String,
}

impl World {
    /// Build both instances (initialized + configured), users, validators and decoys.
    pub fn build(t: &mut Trident) -> World {
        let cfg = Cfg::random(t);
        // clock: epoch far enough that activation/deactivation history is irrelevant
        let epoch0 = 100 + t.random_from_range(0u64..50);
        let sched = t.get_sysvar::<solana_sdk::epoch_schedule::EpochSchedule>();
        t.warp_to_epoch(epoch0);
        t.warp_to_slot(sched.get_first_slot_in_epoch(epoch0));

        let a = new_inst(t, "A", &cfg);
        let b = new_inst(t, "B", &cfg);
        let c = new_inst(t, "C", &cfg); // never initialized: target of the `initialize` fuzz step
        let mut w = World {
            cfg: cfg.clone(),
            a: a.clone(),
            b: b.clone(),
            c,
            users: vec![],
            attacker: User { key: Pubkey::default(), msol: Pubkey::default(), lp: Pubkey::default() },
            votes: vec![],
            spare_votes: vec![],
            fake_vote: Pubkey::default(),
            tags: HashMap::new(),
            pool: vec![],
            tracked: BTreeSet::new(),
            tickets: vec![],
            spare_tickets: vec![],
            user_stakes: vec![],
            donated: false,
            slashed: false,
            paused_ok: false,
            prev: None,
            last_lamports: BTreeMap::new(),
        };
        for inst in [&a, &b] {
            let ix = ixs::initialize(inst, init_data(inst, &cfg));
            let r = w.send_raw(t, &ix.to_instruction());
            assert!(r.ok, "setup: initialize {} failed: {} {}", inst.label, r.err, r.logs);
        }
        // users
        for _ in 0..3 {
            let u = w.new_user(t, &a);
            w.users.push(u);
        }
        let att = w.new_user(t, &a);
        w.attacker = att;
        // validators: 3 registered in A and in B, 2 spare valid vote accounts, one non-vote account
        for _ in 0..3 {
            let v = t.random_pubkey();
            put_vote(t, &v);
            w.votes.push(v);
        }
        for _ in 0..2 {
            let v = t.random_pubkey();
            put_vote(t, &v);
            w.spare_votes.push(v);
        }
        w.fake_vote = t.random_pubkey();
        put(t, &w.fake_vote, 10 * SOL, vec![7u8; 3762], &system_id());

        // config (valid, all paths reachable)
        let params = ConfigMarinadeParams {
            withdraw_stake_account_enabled: Some(true),
            delayed_unstake_fee: Some(FeeCents::from_bp_cents(1_000)),
            withdraw_stake_account_fee: Some(FeeCents::from_bp_cents(1_000)),
            deposit_sol_fee: Some(FeeCents::from_bp_cents(500)),
            deposit_stake_account_fee: Some(FeeCents::from_bp_cents(500)),
            min_deposit: Some(1_000_000),
            min_withdraw: Some(1_000_000),
            ..Default::default()
        };
        for inst in [&a, &b] {
            let r = w.send_raw(t, &ixs::config_marinade(inst, params).to_instruction());
            assert!(r.ok, "setup: config_marinade {}: {} {}", inst.label, r.err, r.logs);
            for v in w.votes.clone() {
                let flag = inst.dup_flag(&v);
                let payer = w.users[0].key;
                let r = w.send_raw(t, &ixs::add_validator(inst, &v, &flag, &payer, 100).to_instruction());
                assert!(r.ok, "setup: add_validator {}: {} {}", inst.label, r.err, r.logs);
            }
        }
        w.register_all(t);
        w.snapshot_lamports(t);
        w
    }

    fn new_user(&mut self, t: &mut Trident, a: &Inst) -> User {
        let key = t.random_pubkey();
        put(t, &key, 100_000 * SOL, vec![], &system_id());
        let msol = t.random_pubkey();
        put_token_account(t, &msol, &a.msol_mint, &key, 0);
        let lp = t.random_pubkey();
        put_token_account(t, &lp, &a.lp_mint, &key, 0);
        User { key, msol, lp }
    }

    /// tag registry: what kind of account each known key is
    fn register_all(&mut self, t: &mut Trident) {
        let mut tag = |w: &mut World, k: Pubkey, tags: &[&'static str]| {
            w.tags.entry(k).or_default().extend_from_slice(tags);
            w.pool.push(k);
            w.tracked.insert(k);
        };
        let users = self.users.clone();
        let att = self.attacker.clone();
        for u in users.iter().chain(std::iter::once(&att)) {
            tag(self, u.key, &["wallet"]);
            tag(self, u.msol, &["msol_ta"]);
            tag(self, u.lp, &["lp_ta"]);
        }
        for inst in [self.a.clone(), self.b.clone()] {
            let a_side = inst.label == "A";
            for k in [inst.admin, inst.vmgr, inst.pauser, inst.operational, inst.treasury_owner] {
                tag(self, k, &["wallet"]);
            }
            // PDAs that are plain system accounts accept lamports like wallets (a "donation")
            tag(self, inst.reserve, &["wallet", "reserve"]);
            tag(self, inst.sol_leg, &["wallet", "sol_leg"]);
            for k in [
                inst.state,
                inst.msol_mint,
                inst.lp_mint,
                inst.stake_list,
                inst.validator_list,
                inst.deposit_auth,
                inst.withdraw_auth,
                inst.msol_mint_auth,
                inst.lp_mint_auth,
                inst.msol_leg_auth,
            ] {
                tag(self, k, &[]);
            }
            if a_side {
                tag(self, inst.msol_leg, &["msol_ta"]);
                tag(self, inst.treasury_msol, &["msol_ta"]);
            } else {
                tag(self, inst.msol_leg, &["msol_ta_B"]);
                tag(self, inst.treasury_msol, &["msol_ta_B"]);
            }
        }
        {
            let c = self.c.clone();
            for k in [c.admin, c.vmgr, c.pauser, c.operational, c.treasury_owner] {
                tag(self, k, &["wallet"]);
            }
            tag(self, c.reserve, &["wallet", "reserve"]);
            tag(self, c.sol_leg, &["wallet", "sol_leg"]);
            tag(self, c.msol_leg, &["msol_ta_C"]);
            tag(self, c.treasury_msol, &["msol_ta_C"]);
            tag(self, c.stake_list, &["zero_list"]);
            tag(self, c.validator_list, &["zero_list"]);
            for k in [c.state, c.msol_mint, c.lp_mint, c.deposit_auth, c.withdraw_auth, c.msol_mint_auth, c.lp_mint_auth, c.msol_leg_auth] {
                tag(self, k, &[]);
            }
        }
        for v in self.votes.clone() {
            tag(self, v, &["vote", "vote_reg"]);
        }
        for v in self.spare_votes.clone() {
            tag(self, v, &["vote"]);
        }
        let fv = self.fake_vote;
        tag(self, fv, &["wallet"]);
        // pre-made decoys: stake accounts of different kinds, B tickets, zeroed tickets
        let v0 = self.votes[0];
        let e = t.get_sysvar::<solana_sdk::clock::Clock>().epoch;
        let (a, b) = (self.a.clone(), self.b.clone());
        let u0 = self.users[0].key;
        // user-owned delegated stake (legit input of deposit_stake_account for user 0)
        let k = t.random_pubkey();
        put_stake(t, &k, &u0, &u0, &v0, 5 * SOL, e - 5, u64::MAX);
        tag(self, k, &["stake_user"]);
        self.user_stakes.push((k, u0));
        // stake authorised to A's PDAs but unknown to A's stake list
        let k = t.random_pubkey();
        put_stake(t, &k, &a.deposit_auth, &a.withdraw_auth, &v0, 5 * SOL, e - 5, u64::MAX);
        tag(self, k, &["rogue_stake"]);
        // stake authorised to B's PDAs
        let k = t.random_pubkey();
        put_stake(t, &k, &b.deposit_auth, &b.withdraw_auth, &v0, 5 * SOL, e - 5, u64::MAX);
        tag(self, k, &["stake_B"]);
        // deactivated PDA stake, initialized-only PDA stake, delegated to an unregistered vote
        let k = t.random_pubkey();
        put_stake(t, &k, &a.deposit_auth, &a.withdraw_auth, &v0, 5 * SOL, e - 8, e - 3);
        tag(self, k, &["rogue_stake"]);
        let k = t.random_pubkey();
        put_stake_initialized(t, &k, &a.deposit_auth, &a.withdraw_auth, 2 * SOL);
        tag(self, k, &["rogue_stake"]);
        let sv = self.spare_votes[0];
        let k = t.random_pubkey();
        put_stake(t, &k, &a.deposit_auth, &a.withdraw_auth, &sv, 5 * SOL, e - 5, u64::MAX);
        tag(self, k, &["rogue_stake"]);
        // user stake delegated to an unregistered validator
        let k = t.random_pubkey();
        put_stake(t, &k, &u0, &u0, &sv, 5 * SOL, e - 5, u64::MAX);
        tag(self, k, &["stake_user_unreg"]);
        // zeroed program-owned ticket-sized accounts (valid `new_ticket_account` inputs)
        for _ in 0..2 {
            let k = t.random_pubkey();
            let l = rent_min(t, TICKET_SIZE);
            put(t, &k, l, vec![0u8; TICKET_SIZE], &program_id());
            tag(self, k, &["ticket_new"]);
            self.spare_tickets.push(k);
        }
        // a ticket that belongs to instance B (valid in B, must be rejected by A)
        let k = t.random_pubkey();
        let mut d = vec![0u8; TICKET_SIZE];
        let td = TicketAccountData { state_address: b.state, beneficiary: u0, lamports_amount: SOL, created_epoch: e - 5 };
        use anchor_lang::AccountSerialize;
        let mut buf = vec![];
        td.try_serialize(&mut buf).unwrap();
        d[..buf.len()].copy_from_slice(&buf);
        let l = rent_min(t, TICKET_SIZE);
        put(t, &k, l, d, &program_id());
        tag(self, k, &["ticket_B"]);
        // the program itself and other programs as account arguments
        for k in [program_id(), system_id(), token_id(), stake_id(), vote_id(), clock_id(), rent_id(), epoch_schedule_id(), stake_history_id(), stake_config_id()] {
            tag(self, k, &["program_or_sysvar"]);
        }
    }

    pub fn tags_of(&self, t: &mut Trident, k: &Pubkey) -> Vec<&'static str> {
        if let Some(v) = self.tags.get(k) {
            return v.clone();
        }
        let acc = t.get_account(k);
        if *acc.owner() == system_id() {
            vec!["wallet", "fresh"]
        } else if *acc.owner() == program_id() {
            if acc.data().len() == TICKET_SIZE {
                // a ticket created during the run
                if TicketAccountData::try_deserialize(&mut acc.data()).map(|d| d.state_address == self.a.state).unwrap_or(false) {
                    vec!["ticket"]
                } else if acc.data().iter().all(|b| *b == 0) {
                    vec!["ticket_new"]
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        } else if *acc.owner() == token_id() {
            match read_token(t, k) {
                Some(ta) if ta.mint == self.a.msol_mint => vec!["msol_ta"],
                Some(ta) if ta.mint == self.a.lp_mint => vec!["lp_ta"],
                _ => vec![],
            }
        } else {
            vec![]
        }
    }

    pub fn snapshot_lamports(&mut self, t: &mut Trident) {
        let keys: Vec<Pubkey> = self.tracked.iter().copied().collect();
        for k in keys {
            let l = lam(t, &k);
            self.last_lamports.insert(k, l);
        }
    }

    pub fn track(&mut self, t: &mut Trident, k: Pubkey) {
        if self.tracked.insert(k) {
            // new account: baseline is its current balance minus nothing (it may be fresh), see invariants
            let l = lam(t, &k);
            self.last_lamports.insert(k, l);
        }
    }

    // ------------------------------------------------------------------ execution

    pub fn send_raw(&mut self, t: &mut Trident, ix: &Instruction) -> Outcome {
        let res = t.process_transaction(std::slice::from_ref(ix), None);
        let ok = res.is_success();
        let (err, panicked, code) = match res.get_result() {
            Ok(()) => (String::new(), false, None),
            Err(e) => {
                let s = format!("{e:?}");
                (s.clone(), s.contains("ProgramFailedToComplete"), res.get_custom_error_code())
            }
        };
        Outcome { ok, err, panicked, code, logs: if ok { String::new() } else { res.logs() } }
    }

    pub fn send(&mut self, t: &mut Trident, ix: &Ix) -> Outcome {
        self.send_raw(t, &ix.to_instruction())
    }

    // ------------------------------------------------------------------ readers

    pub fn state(&self, t: &mut Trident, i: &Inst) -> Option<State> {
        let a = t.get_account(&i.state);
        State::try_deserialize(&mut a.data()).ok()
    }

    fn list<T: AnchorDeserialize>(&self, t: &mut Trident, acct: &Pubkey, count: u32, item_size: u32) -> Vec<T> {
        let a = t.get_account(acct);
        let d = a.data();
        (0..count)
            .filter_map(|i| {
                let s = 8 + (i * item_size) as usize;
                T::deserialize(&mut d.get(s..s + item_size as usize)?).ok()
            })
            .collect()
    }

    pub fn stakes(&self, t: &mut Trident, i: &Inst) -> Vec<StakeRecord> {
        match self.state(t, i) {
            Some(st) => self.list(t, &i.stake_list, st.stake_system.stake_count(), st.stake_system.stake_record_size()),
            None => vec![],
        }
    }

    pub fn validators(&self, t: &mut Trident, i: &Inst) -> Vec<ValidatorRecord> {
        match self.state(t, i) {
            Some(st) => self.list(t, &i.validator_list, st.validator_system.validator_count(), st.validator_system.validator_record_size()),
            None => vec![],
        }
    }

    pub fn ticket(&self, t: &mut Trident, k: &Pubkey) -> Option<TicketAccountData> {
        let a = t.get_account(k);
        TicketAccountData::try_deserialize(&mut a.data()).ok()
    }

    pub fn epoch(&self, t: &mut Trident) -> u64 {
        t.get_sysvar::<solana_sdk::clock::Clock>().epoch
    }

    /// move the clock to `epoch`, `slots_before_end` slots before the last slot of that epoch
    pub fn set_clock(&self, t: &mut Trident, epoch: u64, slots_before_end: u64) {
        let sched = t.get_sysvar::<solana_sdk::epoch_schedule::EpochSchedule>();
        let last = sched.get_last_slot_in_epoch(epoch);
        let first = sched.get_first_slot_in_epoch(epoch);
        t.warp_to_epoch(epoch);
        t.warp_to_slot(last.saturating_sub(slots_before_end).max(first));
    }

    pub fn user_of(&self, k: &Pubkey) -> Option<&User> {
        self.users.iter().find(|u| u.key == *k)
    }
}
