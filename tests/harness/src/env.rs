//! [`TestEnv`]: a LiteSVM configured from a [`Runtime`], with epoch control and helpers.
//!
//! ## Epoch / stake-history model
//! * `Clock` (slot, epoch, epoch_start_timestamp, unix_timestamp, leader_schedule_epoch) is derived from the
//!   slot through the runtime's `EpochSchedule`; 400 ms per slot from a fixed genesis time. Use
//!   [`TestEnv::warp_epochs`] (lands 5000 slots = 2000 s into the epoch so `claim` is allowed),
//!   [`TestEnv::warp_to_stake_window`] (inside Marinade's stake-delta window), [`TestEnv::warp_to_slot`].
//! * The `StakeHistory` sysvar stays EMPTY. The stake program treats a missing history entry for the
//!   activation epoch as "fully effective one epoch after activation" and a missing entry for the
//!   deactivation epoch as "fully deactivated one epoch after deactivation". That is exactly what
//!   mainnet does at Marinade's stake size (a few thousand SOL against ~400M SOL effective, far below the
//!   warmup/cooldown rate limit). Verified against the loaded stake program in `tests/env.rs`.
//! * `EpochRewards` is inactive by default; [`TestEnv::set_epoch_rewards_active`] toggles it (while active
//!   every stake instruction except GetMinimumDelegation fails with EpochRewardsActive).
//! * Rewards: [`TestEnv::reward_stake`] = inflation reward credited like partitioned rewards (account
//!   lamports and `delegation.stake` both grow); [`TestEnv::add_mev`] = lamports only.
//! * Vote accounts are written directly (VoteStateV3, owner Vote111...), no vote program execution.

use crate::convert_ix;
use crate::profile::{addr, sha256_hex, ProgramBin, Runtime, BPF_LOADER_UPGRADEABLE};
use anchor_lang::solana_program::program_pack::Pack;
use anyhow::{anyhow, Context, Result};
use litesvm::LiteSVM;
use solana_account::{Account, ReadableAccount};
use solana_address::Address;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use std::collections::BTreeMap;

pub const SLOT_MS: u64 = 400;
pub const GENESIS_UNIX: i64 = 1_700_000_000;
pub const START_EPOCH: u64 = 600;
/// slots into an epoch that `warp_epochs` lands on (2000 s, > Marinade's 30 min claim delay)
pub const EPOCH_START_OFFSET: u64 = 5_000;
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

pub fn sol(n: u64) -> u64 {
    n * LAMPORTS_PER_SOL
}

// ------------------------------------------------------------------ tx results

#[derive(Clone, Debug)]
pub struct TxMeta {
    pub logs: Vec<String>,
    pub cu: u64,
    pub fee: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ErrClass {
    /// Marinade custom error (6000+)
    Marinade { code: u32, name: Option<String> },
    /// other anchor error (constraint violations etc.)
    Anchor { code: u32, name: Option<String> },
    /// custom error from another program (stake, token, system)
    Custom { code: u32 },
    /// program panic (including arithmetic overflow)
    Panic,
    Other(String),
}

#[derive(Clone, Debug)]
pub struct TxFailure {
    pub err: TransactionError,
    pub ix_index: Option<u8>,
    pub class: ErrClass,
    pub logs: Vec<String>,
    pub cu: u64,
}

impl TxFailure {
    pub fn marinade_code(&self) -> Option<u32> {
        match &self.class {
            ErrClass::Marinade { code, .. } => Some(*code),
            _ => None,
        }
    }
    pub fn marinade_name(&self) -> Option<&str> {
        match &self.class {
            ErrClass::Marinade { name, .. } => name.as_deref(),
            _ => None,
        }
    }
    pub fn is_marinade(&self, e: marinade_finance::error::MarinadeError) -> bool {
        self.marinade_code() == Some(u32::from(e))
    }
    pub fn is_panic(&self) -> bool {
        self.class == ErrClass::Panic
    }
    /// panics / non-Marinade, non-anchor errors that no op should ever produce unprompted
    pub fn is_unexpected(&self) -> bool {
        !matches!(self.class, ErrClass::Marinade { .. } | ErrClass::Anchor { .. })
    }
}

impl std::fmt::Display for TxFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} [{:?}]", self.err, self.class)?;
        let tail: Vec<&String> = self.logs.iter().rev().take(8).collect();
        for l in tail.into_iter().rev() {
            write!(f, "\n    {l}")?;
        }
        Ok(())
    }
}
impl std::error::Error for TxFailure {}

pub type TxResult = Result<TxMeta, TxFailure>;

fn classify(err: &TransactionError, logs: &[String]) -> (Option<u8>, ErrClass) {
    use solana_instruction::error::InstructionError;
    let ix_index = match err {
        TransactionError::InstructionError(i, _) => Some(*i),
        _ => None,
    };
    if logs.iter().any(|l| l.contains("panicked at")) {
        return (ix_index, ErrClass::Panic);
    }
    // anchor: "AnchorError ... Error Code: Name. Error Number: 6061. Error Message: ..."
    for l in logs.iter().rev() {
        if let (Some(a), Some(b)) = (l.find("Error Code: "), l.find("Error Number: ")) {
            let name = l[a + 12..].split('.').next().map(str::to_string);
            let code: Option<u32> = l[b + 14..].split('.').next().and_then(|s| s.trim().parse().ok());
            if let Some(code) = code {
                return if (6000..7000).contains(&code) {
                    (ix_index, ErrClass::Marinade { code, name })
                } else {
                    (ix_index, ErrClass::Anchor { code, name })
                };
            }
        }
    }
    if let TransactionError::InstructionError(_, InstructionError::Custom(c)) = err {
        let c = *c;
        return if (6000..7000).contains(&c) {
            (ix_index, ErrClass::Marinade { code: c, name: None })
        } else {
            (ix_index, ErrClass::Custom { code: c })
        };
    }
    (ix_index, ErrClass::Other(format!("{err:?}")))
}

// ------------------------------------------------------------------ views

#[derive(Clone, Debug, PartialEq)]
pub struct StakeView {
    pub address: Address,
    pub lamports: u64,
    pub data_len: usize,
    pub owner: Address,
    /// "uninitialized" | "initialized" | "stake" | "rewards_pool"
    pub kind: &'static str,
    pub rent_exempt_reserve: u64,
    pub staker: Address,
    pub withdrawer: Address,
    pub lockup_epoch: u64,
    pub lockup_unix: i64,
    pub delegation: Option<DelegationView>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DelegationView {
    pub voter: Address,
    pub stake: u64,
    pub activation_epoch: u64,
    pub deactivation_epoch: u64,
    pub credits_observed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// activating this epoch (effective == 0)
    Activating,
    FullyActive,
    /// deactivation_epoch == current epoch
    Deactivating,
    /// effective == 0 after deactivation
    Deactivated,
    Inactive,
}

// ------------------------------------------------------------------ env

#[derive(Default, Clone, Copy, Debug)]
pub struct Ledger {
    /// net lamports created by harness-side mutations (airdrops, rewards, program deployments, ...)
    pub injected: i128,
    /// transaction fees removed from circulation
    pub burned: u128,
}

pub struct TestEnv {
    svm: LiteSVM,
    pub runtime: Runtime,
    pub payer: Keypair,
    pub marinade_so: ProgramBin,
    pub ledger: Ledger,
    /// number of transactions executed
    pub tx_count: u64,
}

fn build_svm(rt: &Runtime) -> Result<LiteSVM> {
    let svm = LiteSVM::default()
        .with_feature_set(rt.feature_set.clone())
        .with_builtins()
        .with_lamports(1_000_000u64 * LAMPORTS_PER_SOL)
        .with_sysvars()
        .with_feature_accounts()
        .with_default_programs()
        .with_sigverify(true)
        .with_blockhash_check(false)
        .with_transaction_history(0);
    Ok(svm)
}

impl TestEnv {
    pub fn new(rt: &Runtime, marinade_so: ProgramBin) -> Result<Self> {
        let mut svm = build_svm(rt)?;
        svm.set_sysvar(&rt.rent_sysvar());
        svm.set_sysvar(&rt.epoch_schedule);
        for (id, bin) in &rt.programs {
            svm.add_program_with_loader(*id, &bin.bytes, bin.loader)
                .map_err(|e| anyhow!("loading {id} from {}: {e}", bin.source))?;
        }
        svm.add_program_with_loader(addr(crate::marinade::PROGRAM_ID_STR), &marinade_so.bytes, addr(BPF_LOADER_UPGRADEABLE))
            .map_err(|e| anyhow!("loading marinade from {}: {e}", marinade_so.source))?;
        let mut env = TestEnv {
            svm,
            runtime: rt.clone(),
            payer: Keypair::new(),
            marinade_so,
            ledger: Ledger::default(),
            tx_count: 0,
        };
        let first = env.runtime.epoch_schedule.get_first_slot_in_epoch(START_EPOCH);
        env.set_slot(first + 10);
        let payer = env.payer.pubkey();
        env.fund(&payer, sol(1_000_000));
        // everything above is "genesis"; start the ledger here
        env.ledger = Ledger::default();
        Ok(env)
    }

    /// Switch the SAME svm (accounts, clock, ...) to another runtime: feature set, builtins, program
    /// environments (all programs recompiled), Rent, EpochSchedule and program binaries. Call at an
    /// epoch boundary (it does not warp).
    pub fn transition(&mut self, target: &Runtime) -> Result<()> {
        let before = self.total_lamports();
        let old = std::mem::take(&mut self.svm);
        let mut svm = old.with_feature_set(target.feature_set.clone()).with_feature_accounts();
        svm.rebuild_caches().map_err(|e| anyhow!("rebuild_caches: {e}"))?;
        if target.bundled {
            svm = svm.with_default_programs();
        }
        for (id, bin) in &target.programs {
            svm.add_program_with_loader(*id, &bin.bytes, bin.loader)
                .map_err(|e| anyhow!("loading {id} from {}: {e}", bin.source))?;
        }
        svm.set_sysvar(&target.rent_sysvar());
        let schedule_changed = {
            let cur: solana_epoch_schedule::EpochSchedule = svm.get_sysvar();
            cur != target.epoch_schedule
        };
        svm.set_sysvar(&target.epoch_schedule);
        self.svm = svm;
        self.runtime = target.clone();
        if schedule_changed {
            let s = self.slot();
            self.set_slot(s);
        }
        let after = self.total_lamports();
        self.ledger.injected += after as i128 - before as i128;
        Ok(())
    }

    // ---- raw access
    pub fn svm(&self) -> &LiteSVM {
        &self.svm
    }
    /// Direct mutable access. Lamports created/destroyed through it are NOT tracked; prefer [`Self::mutate`].
    pub fn svm_mut(&mut self) -> &mut LiteSVM {
        &mut self.svm
    }
    /// Run a harness-side mutation and book the net lamports change as external injection.
    pub fn mutate<R>(&mut self, f: impl FnOnce(&mut LiteSVM) -> R) -> R {
        let before = self.total_lamports();
        let r = f(&mut self.svm);
        let after = self.total_lamports();
        self.ledger.injected += after as i128 - before as i128;
        r
    }

    pub fn total_lamports(&self) -> u128 {
        self.svm.accounts_db().inner.values().map(|a| a.lamports() as u128).sum()
    }

    pub fn get_account(&self, a: &Address) -> Option<Account> {
        self.svm.get_account(a)
    }
    pub fn data(&self, a: &Address) -> Option<Vec<u8>> {
        self.svm.get_account(a).map(|a| a.data)
    }
    pub fn lamports(&self, a: &Address) -> u64 {
        self.svm.get_balance(a).unwrap_or(0)
    }
    pub fn set_account(&mut self, a: Address, acc: Account) {
        self.mutate(|s| s.set_account(a, acc).expect("set_account"));
    }
    /// All accounts owned by `owner`.
    pub fn accounts_owned_by(&self, owner: &Address) -> Vec<(Address, Account)> {
        self.svm.get_program_accounts(owner)
    }
    pub fn fund(&mut self, a: &Address, lamports: u64) {
        let mut acc = self.get_account(a).unwrap_or_else(|| Account {
            lamports: 0,
            data: vec![],
            owner: solana_sdk_ids::system_program::ID,
            executable: false,
            rent_epoch: u64::MAX,
        });
        acc.lamports += lamports;
        self.set_account(*a, acc);
    }
    pub fn new_funded(&mut self, lamports: u64) -> Keypair {
        let k = Keypair::new();
        self.fund(&k.pubkey(), lamports);
        k
    }

    // ---- rent
    pub fn rent(&self) -> solana_rent::Rent {
        self.svm.get_sysvar()
    }
    pub fn rent_exempt(&self, data_len: usize) -> u64 {
        self.rent().minimum_balance(data_len)
    }
    /// live rent of a stake account (what v5 delegates against)
    pub fn stake_rent(&self) -> u64 {
        self.rent_exempt(200)
    }

    // ---- clock
    pub fn clock(&self) -> solana_clock::Clock {
        self.svm.get_sysvar()
    }
    pub fn slot(&self) -> u64 {
        self.clock().slot
    }
    pub fn epoch(&self) -> u64 {
        self.clock().epoch
    }
    pub fn epoch_schedule(&self) -> solana_epoch_schedule::EpochSchedule {
        self.runtime.epoch_schedule.clone()
    }
    pub fn epoch_first_slot(&self, epoch: u64) -> u64 {
        self.runtime.epoch_schedule.get_first_slot_in_epoch(epoch)
    }
    pub fn epoch_last_slot(&self, epoch: u64) -> u64 {
        self.runtime.epoch_schedule.get_last_slot_in_epoch(epoch)
    }
    fn timestamp_of(slot: u64) -> i64 {
        GENESIS_UNIX + (slot * SLOT_MS / 1000) as i64
    }
    /// Set the clock consistently from a slot (epoch, timestamps, leader schedule epoch).
    pub fn set_slot(&mut self, slot: u64) {
        let es = &self.runtime.epoch_schedule;
        let epoch = es.get_epoch(slot);
        let c = solana_clock::Clock {
            slot,
            epoch,
            leader_schedule_epoch: es.get_leader_schedule_epoch(slot),
            epoch_start_timestamp: Self::timestamp_of(es.get_first_slot_in_epoch(epoch)),
            unix_timestamp: Self::timestamp_of(slot),
        };
        self.svm.set_sysvar(&c);
    }
    pub fn warp_to_slot(&mut self, slot: u64) {
        assert!(slot >= self.slot(), "cannot warp backwards ({} < {})", slot, self.slot());
        self.set_slot(slot);
    }
    pub fn warp_to_epoch_start(&mut self, epoch: u64, offset_slots: u64) {
        let s = self.epoch_first_slot(epoch) + offset_slots;
        self.warp_to_slot(s);
    }
    /// Advance `n` epochs: to slot EPOCH_START_OFFSET of epoch+n.
    pub fn warp_epochs(&mut self, n: u64) {
        let e = self.epoch() + n;
        self.warp_to_epoch_start(e, EPOCH_START_OFFSET);
    }
    /// Move inside the stake-delta window of the current epoch (100 slots after it opens, if not already past).
    pub fn warp_to_stake_window(&mut self, slots_for_stake_delta: u64) {
        let e = self.epoch();
        let target = self.epoch_last_slot(e).saturating_sub(slots_for_stake_delta) + 100;
        if self.slot() < target {
            self.warp_to_slot(target);
        }
    }
    pub fn in_stake_window(&self, slots_for_stake_delta: u64) -> bool {
        let c = self.clock();
        c.slot >= self.epoch_last_slot(c.epoch).saturating_sub(slots_for_stake_delta)
    }
    /// Model the partitioned-rewards window: while active, stake instructions fail with EpochRewardsActive.
    pub fn set_epoch_rewards_active(&mut self, active: bool) {
        let mut er: solana_sysvar::epoch_rewards::EpochRewards = self.svm.get_sysvar();
        er.active = active;
        self.svm.set_sysvar(&er);
    }

    // ---- transactions
    /// Send a transaction paid by `self.payer`. `signers` are the additional signers.
    pub fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> TxResult {
        let payer_pk = self.payer.pubkey();
        let mut all: Vec<&Keypair> = vec![&self.payer];
        for s in signers {
            if s.pubkey() != payer_pk && !all.iter().any(|k| k.pubkey() == s.pubkey()) {
                all.push(s);
            }
        }
        self.svm.expire_blockhash();
        let bh = self.svm.latest_blockhash();
        let tx = Transaction::new_signed_with_payer(ixs, Some(&payer_pk), &all[..], bh);
        self.tx_count += 1;
        match self.svm.send_transaction(tx) {
            Ok(m) => {
                self.ledger.burned += m.fee as u128;
                Ok(TxMeta { logs: m.logs, cu: m.compute_units_consumed, fee: m.fee })
            }
            Err(f) => {
                let (ix_index, class) = classify(&f.err, &f.meta.logs);
                Err(TxFailure { err: f.err, ix_index, class, logs: f.meta.logs, cu: f.meta.compute_units_consumed })
            }
        }
    }

    // ---- programs
    /// sha256 of the ELF currently deployed for `program` (as loaded in the svm).
    pub fn program_sha256(&self, program: &Address) -> Option<String> {
        let acc = self.get_account(program)?;
        if acc.owner == addr(BPF_LOADER_UPGRADEABLE) {
            // Program { programdata_address } = 4 byte tag + 32
            let pd = Address::new_from_array(acc.data.get(4..36)?.try_into().ok()?);
            let pda = self.get_account(&pd)?;
            Some(sha256_hex(pda.data.get(45..)?))
        } else {
            Some(sha256_hex(&acc.data))
        }
    }

    // ---- tokens
    pub fn create_mint(&mut self, authority: &Address, decimals: u8) -> Address {
        use anchor_spl::token::spl_token::state::Mint;
        let k = Address::new_unique();
        let m = Mint {
            mint_authority: Some(crate::convert::to_anchor(authority)).into(),
            supply: 0,
            decimals,
            is_initialized: true,
            freeze_authority: None.into(),
        };
        let mut data = vec![0u8; Mint::LEN];
        Mint::pack(m, &mut data).unwrap();
        let lamports = self.rent_exempt(Mint::LEN);
        self.set_account(k, Account { lamports, data, owner: addr(TOKEN_PROGRAM), executable: false, rent_epoch: u64::MAX });
        k
    }
    pub fn create_token_account(&mut self, mint: &Address, owner: &Address) -> Address {
        let k = Address::new_unique();
        self.create_token_account_at(k, mint, owner);
        k
    }
    pub fn create_token_account_at(&mut self, k: Address, mint: &Address, owner: &Address) {
        use anchor_spl::token::spl_token::state::{Account as TA, AccountState};
        let t = TA {
            mint: crate::convert::to_anchor(mint),
            owner: crate::convert::to_anchor(owner),
            amount: 0,
            delegate: None.into(),
            state: AccountState::Initialized,
            is_native: None.into(),
            delegated_amount: 0,
            close_authority: None.into(),
        };
        let mut data = vec![0u8; TA::LEN];
        TA::pack(t, &mut data).unwrap();
        let lamports = self.rent_exempt(TA::LEN);
        self.set_account(k, Account { lamports, data, owner: addr(TOKEN_PROGRAM), executable: false, rent_epoch: u64::MAX });
    }
    pub fn token_balance(&self, a: &Address) -> u64 {
        use anchor_spl::token::spl_token::state::Account as TA;
        self.data(a).and_then(|d| TA::unpack(&d).ok()).map(|t| t.amount).unwrap_or(0)
    }
    pub fn mint_supply(&self, m: &Address) -> u64 {
        use anchor_spl::token::spl_token::state::Mint;
        self.data(m).and_then(|d| Mint::unpack(&d).ok()).map(|t| t.supply).unwrap_or(0)
    }

    // ---- vote accounts
    /// Create a vote account directly (VoteStateV3, owned by the vote program).
    pub fn create_vote_account(&mut self) -> Address {
        use solana_vote_interface::state::{VoteStateV3, VoteStateVersions};
        let k = Address::new_unique();
        let st = VoteStateV3 {
            node_pubkey: Address::new_unique(),
            authorized_withdrawer: Address::new_unique(),
            commission: 5,
            ..Default::default()
        };
        let mut data = vec![0u8; VoteStateV3::size_of()];
        bincode::serialize_into(&mut data[..], &VoteStateVersions::new_v3(st)).expect("vote state");
        let lamports = self.rent_exempt(data.len());
        self.set_account(
            k,
            Account { lamports, data, owner: solana_sdk_ids::vote::ID, executable: false, rent_epoch: u64::MAX },
        );
        k
    }

    // ---- stake accounts
    /// Create + initialize + delegate a stake account through the real stake program.
    /// Total lamports = live stake rent + `delegated`; staker = withdrawer = `authority`.
    pub fn create_delegated_stake(&mut self, authority: &Keypair, vote: &Address, delegated: u64) -> Result<Address> {
        let stake = Keypair::new();
        let lamports = self.stake_rent() + delegated;
        let auth = solana_stake_interface::state::Authorized {
            staker: p(&authority.pubkey()),
            withdrawer: p(&authority.pubkey()),
        };
        let ixs = solana_stake_interface::instruction::create_account_and_delegate_stake(
            &p(&self.payer.pubkey()),
            &p(&stake.pubkey()),
            &p(vote),
            &auth,
            &solana_stake_interface::state::Lockup::default(),
            lamports,
        );
        let ixs: Vec<Instruction> = ixs.iter().map(|i| convert_ix!(i)).collect();
        self.send(&ixs, &[&stake, authority]).map_err(|e| anyhow!("create_delegated_stake: {e}"))?;
        Ok(stake.pubkey())
    }

    pub fn stake_view(&self, a: &Address) -> Option<StakeView> {
        use solana_stake_interface::state::StakeStateV2;
        let acc = self.get_account(a)?;
        let st: StakeStateV2 = bincode::deserialize(&acc.data).ok()?;
        let (kind, meta, deleg) = match &st {
            StakeStateV2::Uninitialized => ("uninitialized", None, None),
            StakeStateV2::Initialized(m) => ("initialized", Some(*m), None),
            StakeStateV2::Stake(m, s, _) => ("stake", Some(*m), Some(*s)),
            StakeStateV2::RewardsPool => ("rewards_pool", None, None),
        };
        #[allow(deprecated)]
        Some(StakeView {
            address: *a,
            lamports: acc.lamports,
            data_len: acc.data.len(),
            owner: acc.owner,
            kind,
            rent_exempt_reserve: meta.map(|m| m.rent_exempt_reserve).unwrap_or(0),
            staker: meta.map(|m| q(&m.authorized.staker)).unwrap_or_default(),
            withdrawer: meta.map(|m| q(&m.authorized.withdrawer)).unwrap_or_default(),
            lockup_epoch: meta.map(|m| m.lockup.epoch).unwrap_or(0),
            lockup_unix: meta.map(|m| m.lockup.unix_timestamp).unwrap_or(0),
            delegation: deleg.map(|s| DelegationView {
                voter: q(&s.delegation.voter_pubkey),
                stake: s.delegation.stake,
                activation_epoch: s.delegation.activation_epoch,
                deactivation_epoch: s.delegation.deactivation_epoch,
                credits_observed: s.credits_observed,
            }),
        })
    }

    /// Activation status at the current epoch under the (empty) StakeHistory model.
    pub fn activation(&self, d: &DelegationView) -> Activation {
        activation_at(d, self.epoch())
    }

    /// Inflation-style reward: `delegation.stake` and lamports both grow (partitioned rewards).
    pub fn reward_stake(&mut self, a: &Address, lamports: u64) {
        use solana_stake_interface::state::StakeStateV2;
        let mut acc = self.get_account(a).expect("stake account");
        let mut st: StakeStateV2 = bincode::deserialize(&acc.data).expect("stake state");
        match &mut st {
            StakeStateV2::Stake(_, s, _) => s.delegation.stake += lamports,
            _ => panic!("reward_stake: {a} is not delegated"),
        }
        let mut data = vec![0u8; acc.data.len()];
        bincode::serialize_into(&mut data[..], &st).unwrap();
        acc.data = data;
        acc.lamports += lamports;
        self.set_account(*a, acc);
    }

    /// MEV-style income: lamports only (not delegated).
    pub fn add_mev(&mut self, a: &Address, lamports: u64) {
        self.fund(a, lamports);
    }

    /// Simulate slashing: shrink `delegation.stake` and lamports.
    pub fn slash_stake(&mut self, a: &Address, lamports: u64) {
        use solana_stake_interface::state::StakeStateV2;
        let mut acc = self.get_account(a).expect("stake account");
        let mut st: StakeStateV2 = bincode::deserialize(&acc.data).expect("stake state");
        if let StakeStateV2::Stake(_, s, _) = &mut st {
            s.delegation.stake = s.delegation.stake.saturating_sub(lamports);
        }
        let mut data = vec![0u8; acc.data.len()];
        bincode::serialize_into(&mut data[..], &st).unwrap();
        acc.data = data;
        acc.lamports -= lamports.min(acc.lamports);
        self.set_account(*a, acc);
    }
}

pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Activation status with an empty StakeHistory (see module docs).
pub fn activation_at(d: &DelegationView, epoch: u64) -> Activation {
    if d.deactivation_epoch != u64::MAX {
        if epoch == d.deactivation_epoch {
            Activation::Deactivating
        } else if epoch > d.deactivation_epoch {
            Activation::Deactivated
        } else {
            Activation::FullyActive
        }
    } else if epoch <= d.activation_epoch {
        Activation::Activating
    } else {
        Activation::FullyActive
    }
}

// pubkey conversions for the stake interface crate
pub fn p(a: &Address) -> solana_pubkey::Pubkey {
    solana_pubkey::Pubkey::new_from_array(a.to_bytes())
}
pub fn q(k: &solana_pubkey::Pubkey) -> Address {
    Address::new_from_array(k.to_bytes())
}

impl Default for TestEnvOptions {
    fn default() -> Self {
        Self {}
    }
}
/// reserved for future options
pub struct TestEnvOptions {}

/// Build an env from `HARNESS_BASE` (or an explicit spec) and the Marinade binary under test.
pub fn env_from_spec(spec: &str) -> Result<TestEnv> {
    let rt = crate::profile::resolve(spec).with_context(|| format!("resolving {spec:?}"))?;
    let so = crate::profile::marinade_so(&rt)?;
    TestEnv::new(&rt, so)
}

pub fn programs_hashes(env: &TestEnv) -> BTreeMap<String, Option<String>> {
    let mut m = BTreeMap::new();
    for (n, id) in [("stake", crate::profile::STAKE_ID), ("token", TOKEN_PROGRAM), ("marinade", crate::marinade::PROGRAM_ID_STR)] {
        m.insert(n.to_string(), env.program_sha256(&addr(id)));
    }
    m
}
