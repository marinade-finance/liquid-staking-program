//! Activation-time effects of feature gates.
//!
//! Agave applies some features inside `Bank::compute_and_apply_new_feature_activations`
//! at the epoch boundary where they activate (sysvar rewrites, builtin -> core BPF
//! migrations, core BPF upgrades from a buffer). LiteSVM only consults the feature set at
//! execution time, so a *what-if* toggle of such a feature needs this model. Cluster
//! snapshots already carry the real post-activation values.
//!
//! Every statement here is verified against agave `v4.4.1` (what mainnet runs), `v4.5.0-beta.0`
//! (testnet/devnet) and `master` (5.0.0-alpha.0); citations are in `tests/docs/feature-effects.md`.
//! Ids are base58 strings so this module needs no solana crates.
//!
//! Key facts (see the doc for the evidence):
//!
//! * The Rent sysvar is rewritten at the epoch boundary, in a fixed order per boundary:
//!   (1) `deprecate_rent_exemption_threshold`: `lpb = (lpb as f64 * threshold) as u64`,
//!   `threshold = 1.0`, `burn_percent = 50`; (2) each SIMD-0437 step that activates at this
//!   boundary overwrites `lpb` with its constant, in array order (so the lowest one wins);
//!   (3) SIMD-0438 (`set_lamports_per_byte_to_6960`) overwrites `lpb` with 6960 last.
//!   A step does NOT require the threshold deprecation (nothing enforces it); without it the
//!   threshold stays 2.0 and the exemption minimum is doubled.
//! * The u64 in the sysvar is `lamports_per_byte` since solana-rent 4.1 (formerly
//!   `lamports_per_byte_year`). The wire layout is unchanged. With threshold 1.0 it is the
//!   full exemption price per byte; there is no "per year" any more.
//! * LiteSVM 0.18.2 bundles the v5.0.0 stake binary (not a feature; always loaded); the clusters run
//!   v5.1.x, so the stake binary must come from the profile (see docs).
//! * `Meta.rent_exempt_reserve` written by the core BPF stake program (v5.x) is the constant
//!   2_282_880 and is ignored by the program, which uses the live Rent.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RentParams {
    /// Pre SIMD-0194 this is `lamports_per_byte_year` (genesis 3480); afterwards it is the
    /// lamports per byte of exemption (genesis-equivalent 6960). Same u64 slot in the sysvar.
    pub lamports_per_byte_year: u64,
    pub exemption_threshold: f64,
    pub burn_percent: u8,
}

/// Rent sysvar of mainnet/testnet/devnet genesis (Rent::default() before SIMD-0194).
pub const GENESIS_RENT: RentParams = RentParams {
    lamports_per_byte_year: 3480,
    exemption_threshold: 2.0,
    burn_percent: 50,
};

/// `solana_rent::ACCOUNT_STORAGE_OVERHEAD`.
pub const ACCOUNT_STORAGE_OVERHEAD: u64 = 128;
/// `StakeStateV2::size_of()`.
pub const STAKE_STATE_V2_SIZE: usize = 200;
/// `Meta.rent_exempt_reserve` as written by core BPF stake v5.x (`PSEUDO_RENT_EXEMPT_RESERVE`)
/// and by every pre-SIMD-0194 stake account (200 bytes at genesis Rent).
pub const LEGACY_STAKE_RENT_EXEMPT_RESERVE: u64 = 2_282_880;

impl RentParams {
    /// Mirror of `solana_rent::Rent::minimum_balance_unchecked` (4.4):
    /// threshold 1.0 -> `(128 + len) * lpb`; threshold 2.0 -> `2 * (128 + len) * lpb`;
    /// anything else -> f64 path. (`max(1)` of `Bank::get_minimum_balance_for_rent_exemption`
    /// is not applied here.)
    pub fn minimum_balance(&self, data_len: usize) -> u64 {
        let base = (ACCOUNT_STORAGE_OVERHEAD + data_len as u64) * self.lamports_per_byte_year;
        if self.exemption_threshold == 1.0 {
            base
        } else if self.exemption_threshold == 2.0 {
            2 * base
        } else {
            (base as f64 * self.exemption_threshold) as u64
        }
    }

    /// Exemption minimum of a stake account (what the stake program demands).
    pub fn stake_account_minimum(&self) -> u64 {
        self.minimum_balance(STAKE_STATE_V2_SIZE)
    }
}

/// SIMD-0194. Bank: `RentCollector::deprecate_rent_exemption_threshold` (runtime/src/rent_collector.rs).
pub const DEPRECATE_RENT_EXEMPTION_THRESHOLD: &str = "rent6iVy6PDoViPBeJ6k5EJQrkj62h7DPyLbWGHwjrC";

/// SIMD-0437 steps: (feature id, module name, lamports per byte).
/// Array order is the order in which agave applies them within one epoch boundary.
pub const SIMD_0437_STEPS: &[(&str, &str, u64)] = &[
    ("4a6f7o7iTcA8hRDCrPLkSatnt5Ykxiu36wo5p1Tt12wC", "set_lamports_per_byte_to_6333", 6333),
    ("61BtM7BkDEE8Yq5fskEVAQT9mYA8qCejJWoLe5apqg81", "set_lamports_per_byte_to_5080", 5080),
    ("rntCigrTppP5JdZz7K8TyN9sMzLdAcXp8SejYpVpX6D", "set_lamports_per_byte_to_2575", 2575),
    ("rntD7invRBswCAdKtRsh1G4psKjrPdS3BKqtnA78C7N", "set_lamports_per_byte_to_1322", 1322),
    ("rntTjNZ9boq8owDxjGVFHPfWNQPDaKiM5JcjxmDGg47", "set_lamports_per_byte_to_696", 696),
];

/// SIMD-0438 safeguard: (feature id, module name, lamports per byte). Applied after the
/// SIMD-0437 steps of the same boundary, so it overrides them; later steps override it.
pub const SIMD_0438_RESET: (&str, &str, u64) = (
    "rnt8ZQpz2HYhX3DkYBDGjJS1a36mYq69oXka7JrhEdi",
    "set_lamports_per_byte_to_6960",
    6960,
);

/// Rent sysvar a cluster ends up with, given which features are active (base58 ids).
///
/// Activation order is unknown here, so the canonical order is assumed (every active
/// feature activated at the same boundary: threshold deprecation, then the steps in array
/// order (lowest wins), then the 6960 reset). Use [`rent_for_activations`] when the
/// activation slots are known (e.g. from a cluster profile), which also handles features
/// activated out of order across epochs.
pub fn rent_for(is_active: &dyn Fn(&str) -> bool) -> RentParams {
    rent_for_activations(&|id| if is_active(id) { Some(0) } else { None })
}

/// Like [`rent_for`] but replays agave's per-epoch-boundary application using the
/// activation slot of each feature (`None` = inactive). Features with equal slot activated
/// at the same boundary and are applied deprecation -> steps (array order) -> 6960 reset;
/// boundaries are replayed in increasing slot order, so e.g. 5080 activating one epoch
/// after 2575 leaves 5080 ("most recently activated value wins"), and a step activating
/// *before* the threshold deprecation is multiplied by the old threshold (2.0).
pub fn rent_for_activations(activated_at: &dyn Fn(&str) -> Option<u64>) -> RentParams {
    // (slot, order within a boundary, new lamports_per_byte or 0 for the deprecation)
    let mut events: Vec<(u64, usize, u64)> = Vec::new();
    if let Some(slot) = activated_at(DEPRECATE_RENT_EXEMPTION_THRESHOLD) {
        events.push((slot, 0, 0));
    }
    for (i, (id, _, price)) in SIMD_0437_STEPS.iter().enumerate() {
        if let Some(slot) = activated_at(id) {
            events.push((slot, 1 + i, *price));
        }
    }
    if let Some(slot) = activated_at(SIMD_0438_RESET.0) {
        events.push((slot, 1 + SIMD_0437_STEPS.len(), SIMD_0438_RESET.2));
    }
    events.sort_by_key(|(slot, order, _)| (*slot, *order));

    let mut rent = GENESIS_RENT;
    for (_, order, price) in events {
        if order == 0 {
            // RentCollector::deprecate_rent_exemption_threshold
            rent = RentParams {
                lamports_per_byte_year: (rent.lamports_per_byte_year as f64
                    * rent.exemption_threshold) as u64,
                exemption_threshold: 1.0,
                burn_percent: 50,
            };
        } else {
            rent.lamports_per_byte_year = price;
        }
    }
    rent
}

/// True if toggling this feature can change [`rent_for`].
pub fn affects_rent(feature_id: &str) -> bool {
    feature_id == DEPRECATE_RENT_EXEMPTION_THRESHOLD
        || feature_id == SIMD_0438_RESET.0
        || SIMD_0437_STEPS.iter().any(|(id, _, _)| *id == feature_id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapKind {
    /// a native builtin becomes a core BPF program loaded from `source_buffer`
    MigrateBuiltinToCoreBpf,
    /// an existing core BPF program is upgraded in place from `source_buffer`
    UpgradeCoreBpf,
    /// a non-core program (e.g. SPL Token) is replaced by the buffer's program
    ReplaceProgram,
}

#[derive(Clone, Copy, Debug)]
pub struct ProgramSwap {
    pub feature_id: &'static str,
    pub feature_module: &'static str,
    pub program_id: &'static str,
    pub source_buffer: &'static str,
    pub kind: SwapKind,
}

/// Program binaries a feature swaps in at activation, newest first.
///
/// There is currently NO production builtin -> core BPF migration in agave (v4.4.1, v4.5.0-beta.0,
/// master): every `BUILTINS[..].core_bpf_migration_config` is `None` outside test-only configs.
/// Source buffers are consumed (zeroed) by the activation, so for features already active on a
/// cluster the buffer no longer exists there; the post-activation binary comes from the
/// cluster profile's `programs` instead. Only `replace_ata_with_p_ata` still has a live buffer.
pub const PROGRAM_SWAPS: &[ProgramSwap] = &[
    // SIMD-0567, agave >= 4.5.0-beta.0. Buffer live on mainnet/testnet/devnet, feature inactive everywhere.
    ProgramSwap {
        feature_id: "pata6uFabFDejC1U7rpmFj9s1FhmiPyWk6D51U1ksx3",
        feature_module: "replace_ata_with_p_ata",
        program_id: "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
        source_buffer: "pataAAkF439TTjwRqmxRkTTEfZemRdoriXusez4PQ6S",
        kind: SwapKind::ReplaceProgram,
    },
    // SIMD-0391, handler in agave 4.2.x-4.3.x (removed in 4.4.0). Active everywhere; buffer gone.
    ProgramSwap {
        feature_id: "s51VGwCAgebo2745DSUris72RavoLkXGUmVJosESCXr",
        feature_module: "upgrade_bpf_stake_program_to_v5_1",
        program_id: "Stake11111111111111111111111111111111111111",
        source_buffer: "p51x11QCYMHwuVS1MBcLHKb3MezWyqGS5BEB41CA1dk",
        kind: SwapKind::UpgradeCoreBpf,
    },
    // SIMD-0490, handler in agave 4.0 - 4.4.x (removed in 4.5). Active everywhere; buffer gone.
    ProgramSwap {
        feature_id: "STk5Xj8hdAx3sTzmtJ3QysKkq6X2A3yj73JtxttiRyk",
        feature_module: "upgrade_bpf_stake_program_to_v5",
        program_id: "Stake11111111111111111111111111111111111111",
        source_buffer: "4EBQBjw1kqF1dqUBb6fc5Ji4tCEQgNf9ESGGX3smwXwh",
        kind: SwapKind::UpgradeCoreBpf,
    },
    // SIMD-0266, handler in agave 3.1 - 4.4.x (removed in 4.5). Active everywhere; buffer gone.
    ProgramSwap {
        feature_id: "ptokFjwyJtrwCa9Kgo9xoDS59V4QccBGEaRFnRPnSdP",
        feature_module: "replace_spl_token_with_p_token",
        program_id: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        source_buffer: "ptok6rngomXrDbWf5v5Mkmu5CEbB51hzSCPDoj9DrvF",
        kind: SwapKind::ReplaceProgram,
    },
    // SIMD-0185. Handler existed only in agave 3.1.x - 4.0.x (upgrade of the stake program for
    // vote state v4); gone since 4.2. `vote_state_v4::stake_program_buffer` is a dead constant in
    // 4.4.1+. Active everywhere; buffer gone.
    ProgramSwap {
        feature_id: "Gx4XFcrVMt4HUvPzTpTSVkdDVgcDSjKhDN1RqRS6KDuZ",
        feature_module: "vote_state_v4",
        program_id: "Stake11111111111111111111111111111111111111",
        source_buffer: "BM11F4hqrpinQs28sEZfzQ2fYddivYs4NEAHF6QMjkJF",
        kind: SwapKind::UpgradeCoreBpf,
    },
];

/// How agave performs a swap (from `runtime/src/bank/builtins/core_bpf_migration/mod.rs`).
#[derive(Clone, Copy, Debug)]
pub struct SwapDetails {
    pub feature_id: &'static str,
    /// Upgrade authority of the resulting ProgramData account.
    pub upgrade_authority: &'static str,
    /// The buffer ELF is run through the loader's deploy verification (`directly_invoke_loader_v3_deploy`).
    pub elf_verified: bool,
    /// The SHA-256 of the buffer ELF (trailing zeros trimmed) is checked against a constant
    /// (only agave >= 4.5 and only once `core_bpf_hash_validation` is active, which it is not anywhere).
    pub build_hash: Option<&'static str>,
    pub note: &'static str,
}

pub const SWAP_DETAILS: &[SwapDetails] = &[
    SwapDetails {
        feature_id: "pata6uFabFDejC1U7rpmFj9s1FhmiPyWk6D51U1ksx3",
        upgrade_authority: "none (loader v2 target; new Program + ProgramData accounts, ProgramData slot = activation slot)",
        elf_verified: true,
        build_hash: Some("4e008052b0a7ac695482474946700abe70e9cfd03d178aa8887b02674e7f7d9e"),
        note: "allow_prefunded=true; buffer authority 8pEc9HbSo4bVbVcTZsxAirzYRVLBJ4Jtr18PzEy7pkzf is ignored",
    },
    SwapDetails {
        feature_id: "s51VGwCAgebo2745DSUris72RavoLkXGUmVJosESCXr",
        upgrade_authority: "kept from the existing ProgramData (None on all clusters)",
        elf_verified: true,
        build_hash: None,
        note: "program account untouched; ProgramData replaced (slot = activation slot); buffer authority must equal target authority if the target has one",
    },
    SwapDetails {
        feature_id: "STk5Xj8hdAx3sTzmtJ3QysKkq6X2A3yj73JtxttiRyk",
        upgrade_authority: "kept from the existing ProgramData (None on all clusters)",
        elf_verified: true,
        build_hash: None,
        note: "same mechanism as v5.1",
    },
    SwapDetails {
        feature_id: "ptokFjwyJtrwCa9Kgo9xoDS59V4QccBGEaRFnRPnSdP",
        upgrade_authority: "none (loader v2 target)",
        elf_verified: true,
        build_hash: None,
        note: "relax_programdata_account_check_migration => allow_prefunded; ProgramData ELF is the buffer ELF incl. trailing allocation padding",
    },
    SwapDetails {
        feature_id: "Gx4XFcrVMt4HUvPzTpTSVkdDVgcDSjKhDN1RqRS6KDuZ",
        upgrade_authority: "kept from the existing ProgramData",
        elf_verified: true,
        build_hash: None,
        note: "historical only",
    },
];

/// Features with an activation-time effect this model does not reproduce.
#[derive(Clone, Copy, Debug)]
pub struct Unmodeled {
    pub feature_id: &'static str,
    pub feature_module: &'static str,
    pub note: &'static str,
}

pub const UNMODELED: &[Unmodeled] = &[
    Unmodeled {
        feature_id: "4RWNif6C2WCNiKVW7otP4G7dkmkHGyKQWRpuZ1pxKU5m",
        feature_module: "pico_inflation",
        note: "Bank sets Inflation::pico() and burn_percent; only the reward amounts the harness injects by hand depend on it",
    },
    Unmodeled {
        feature_id: "DT4n6ABDqs6w4bnfwrXT9rsprcPf6cdDga1egctaPkLC",
        feature_module: "full_inflation::devnet_and_testnet",
        note: "Bank sets Inflation::full() and burn_percent (also the certusone vote/enable pair BzBBveUDymEYoYzcMWNQCx3cd4jQs7puaVFHLtsbB6fm + 7XRJcS5Ud5vxGB54JbK9N2vBZVwnwdBNeJW1ibRgD9gx); rewards only",
    },
    Unmodeled {
        feature_id: "ddibiks7RHZR69QXYMCmk4EZNt6WckspTqEEWv6cS38",
        feature_module: "double_disinflation_rate",
        note: "re-anchors Inflation.initial and sets taper 0.30 at activation (apply_double_disinflation_rate); inactive on all clusters; rewards only",
    },
    Unmodeled {
        feature_id: "7AzqUCKQjoCKhWDDHgEYkTY8iRwWtuuxHogcmx1UJzR8",
        feature_module: "remove_runtime_float_ops",
        note: "agave >= 4.5: apply_integer_inflation_initial_rate at activation; inactive on all clusters; rewards only",
    },
    Unmodeled {
        feature_id: "iBRL5RuWhw4yqaAZu96RUULHckHTZAoe2b77qaV38JZ",
        feature_module: "reduce_slot_time_to_350ms",
        note: "ns_per_slot/slots_per_year/hashes_per_tick refresh (apply_slot_time_persistent_changes); no sysvar change; also 300ms iBRLL3k18HST852F1Mf3Lv83waTNQmmqvKDxvYGwQFL, 250ms iBRLMc81UjRa8fn8A6eE8bJTnRbgQoPTynM51akENCV, 200ms iBRLjhJnkmDZgNoZRDMW11d8ZV7HvsL3vAyRjZB5npW",
    },
    Unmodeled {
        feature_id: "A1pengvuM6JEcyNuTnMqepBKhwHE3N6PmUrdATGawhJS",
        feature_module: "alpenglow",
        note: "creates EpochInflationAccountState / vote reward accounts at the epoch boundary, stops hashes_per_tick refresh, changes vote/rewards flow; active on all clusters; LiteSVM has no consensus",
    },
    Unmodeled {
        feature_id: "RMsTKfD6hZnBhhNvgGBeKNrqCNkeoP3DYYxNtcuWtRg",
        feature_module: "remove_inactive_stakes",
        note: "stakes cache drops inert delegations at the boundary (rewards/epoch-stake calc only); active on testnet only",
    },
    Unmodeled {
        feature_id: "Crbnc267wkJvFhYakqwWWX57sx5bkrftJLekzy6yWKLT",
        feature_module: "block_revenue_sharing",
        note: "changes how block revenue is distributed at the epoch boundary (reward partitioning); inactive on all clusters",
    },
    Unmodeled {
        feature_id: "zk1snxsc6Fh3wsGNbbHAJNHiJoYgF29mMnTSusGx5EJ",
        feature_module: "zk_token_sdk_enabled",
        note: "adds the ZK token proof builtin at activation; LiteSVM adds builtins only when built (with_builtins) so a later toggle needs a rebuild; irrelevant to Marinade (also zk_elgamal_proof_program_enabled zkhiy5oLowR7HY4zogXjCjeMXyruLqBwSWH21qcFtnv)",
    },
    Unmodeled {
        feature_id: "FCsQnd4V52NESBhVURWYSWBbmLFzgxEZVC2WngzHzp1u",
        feature_module: "core_bpf_hash_validation",
        note: "agave >= 4.5 checks SHA-256 of the migration buffer ELF (trailing zeros trimmed) against a build hash; we only swap binaries, we do not validate; inactive on all clusters",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    const DEP: &str = DEPRECATE_RENT_EXEMPTION_THRESHOLD;

    fn id(step: usize) -> &'static str {
        SIMD_0437_STEPS[step].0
    }

    fn rent_with(active: &[&str]) -> RentParams {
        rent_for(&|id| active.contains(&id))
    }

    fn stake_rent(r: RentParams) -> u64 {
        r.stake_account_minimum()
    }

    #[test]
    fn genesis_stake_rent_is_the_legacy_reserve() {
        assert_eq!(stake_rent(rent_for(&|_| false)), 2_282_880);
        assert_eq!(stake_rent(rent_for(&|_| false)), LEGACY_STAKE_RENT_EXEMPT_RESERVE);
    }

    #[test]
    fn deprecation_alone_keeps_the_minimum_but_folds_the_threshold() {
        let rent = rent_with(&[DEP]);
        assert_eq!(rent.lamports_per_byte_year, 6960);
        assert_eq!(rent.exemption_threshold, 1.0);
        assert_eq!(rent.burn_percent, 50);
        assert_eq!(stake_rent(rent), 2_282_880);
    }

    #[test]
    fn mainnet_2026_11_stake_rent() {
        let rent = rent_with(&[DEP, id(0), id(1)]);
        assert_eq!(rent.lamports_per_byte_year, 5080);
        assert_eq!(rent.exemption_threshold, 1.0);
        assert_eq!(stake_rent(rent), 1_666_240);
        // matches getMinimumBalanceForRentExemption(200) on all three clusters
    }

    #[test]
    fn each_future_step() {
        let cases = [
            (0, 6333u64, 2_077_224u64),
            (1, 5080, 1_666_240),
            (2, 2575, 844_600),
            (3, 1322, 433_616),
            (4, 696, 228_288),
        ];
        for (n, lpb, stake) in cases {
            // steps activate in order, so the first n+1 are active
            let mut active = vec![DEP];
            active.extend((0..=n).map(id));
            let rent = rent_with(&active);
            assert_eq!(rent.lamports_per_byte_year, lpb, "step {n}");
            assert_eq!(stake_rent(rent), stake, "step {n}");
        }
    }

    #[test]
    fn deactivating_a_step_reverts() {
        let all = [DEP, id(0), id(1), id(2), id(3), id(4)];
        assert_eq!(rent_with(&all).lamports_per_byte_year, 696);
        assert_eq!(rent_with(&all[..5]).lamports_per_byte_year, 1322);
        assert_eq!(rent_with(&all[..4]).lamports_per_byte_year, 2575);
        assert_eq!(rent_with(&all[..3]).lamports_per_byte_year, 5080);
        assert_eq!(rent_with(&all[..2]).lamports_per_byte_year, 6333);
        assert_eq!(rent_with(&all[..1]).lamports_per_byte_year, 6960);
        assert_eq!(rent_with(&[]), GENESIS_RENT);
        // pending tail removed: -2575 from mainnet-plus-everything is back at 5080
        let without_2575: Vec<&str> = all.iter().copied().filter(|f| *f != id(2)).collect();
        // 1322 and 696 still active => lowest wins (set-only view)
        assert_eq!(rent_with(&without_2575).lamports_per_byte_year, 696);
    }

    #[test]
    fn steps_do_not_require_the_deprecation() {
        // agave does not check; the threshold stays 2.0 and the minimum doubles
        let rent = rent_with(&[id(1)]);
        assert_eq!(rent.lamports_per_byte_year, 5080);
        assert_eq!(rent.exemption_threshold, 2.0);
        assert_eq!(stake_rent(rent), 2 * 328 * 5080);
    }

    #[test]
    fn simd_0438_resets_to_6960() {
        let rent = rent_with(&[DEP, id(0), id(1), SIMD_0438_RESET.0]);
        assert_eq!(rent.lamports_per_byte_year, 6960);
        assert_eq!(stake_rent(rent), 2_282_880);
        assert!(affects_rent(SIMD_0438_RESET.0));
    }

    #[test]
    fn activation_order_is_replayed() {
        let at = |m: &'static [(&'static str, u64)]| {
            move |f: &str| m.iter().find(|(i, _)| *i == f).map(|(_, s)| *s)
        };
        // out of order across epochs: the most recently activated value wins
        let rent = rent_for_activations(&at(&[(DEP, 1), ("rntCigrTppP5JdZz7K8TyN9sMzLdAcXp8SejYpVpX6D", 2), ("61BtM7BkDEE8Yq5fskEVAQT9mYA8qCejJWoLe5apqg81", 3)]));
        assert_eq!(rent.lamports_per_byte_year, 5080);
        // same boundary: array order, lowest wins
        let rent = rent_for_activations(&at(&[(DEP, 1), ("rntCigrTppP5JdZz7K8TyN9sMzLdAcXp8SejYpVpX6D", 2), ("61BtM7BkDEE8Yq5fskEVAQT9mYA8qCejJWoLe5apqg81", 2)]));
        assert_eq!(rent.lamports_per_byte_year, 2575);
        // 6960 in the same boundary as a step overrides it; a later step overrides 6960
        let rent = rent_for_activations(&at(&[(DEP, 1), ("rntTjNZ9boq8owDxjGVFHPfWNQPDaKiM5JcjxmDGg47", 2), (SIMD_0438_RESET.0, 2)]));
        assert_eq!(rent.lamports_per_byte_year, 6960);
        let rent = rent_for_activations(&at(&[(DEP, 1), (SIMD_0438_RESET.0, 2), ("rntTjNZ9boq8owDxjGVFHPfWNQPDaKiM5JcjxmDGg47", 3)]));
        assert_eq!(rent.lamports_per_byte_year, 696);
        // a step before the deprecation is multiplied by the old threshold
        let rent = rent_for_activations(&at(&[("61BtM7BkDEE8Yq5fskEVAQT9mYA8qCejJWoLe5apqg81", 1), (DEP, 2)]));
        assert_eq!(rent.lamports_per_byte_year, 10160);
        assert_eq!(rent.exemption_threshold, 1.0);
        // the real mainnet history
        let rent = rent_for_activations(&at(&[
            (DEP, 407_376_000),
            ("4a6f7o7iTcA8hRDCrPLkSatnt5Ykxiu36wo5p1Tt12wC", 444_096_000),
            ("61BtM7BkDEE8Yq5fskEVAQT9mYA8qCejJWoLe5apqg81", 446_256_000),
        ]));
        assert_eq!(stake_rent(rent), 1_666_240);
    }

    #[test]
    fn affects_rent_covers_every_rent_feature() {
        assert!(affects_rent(DEP));
        for (id, _, _) in SIMD_0437_STEPS {
            assert!(affects_rent(id));
        }
        assert!(!affects_rent("ptokFjwyJtrwCa9Kgo9xoDS59V4QccBGEaRFnRPnSdP"));
    }

    #[test]
    fn swap_tables_are_consistent() {
        for s in PROGRAM_SWAPS {
            assert!(SWAP_DETAILS.iter().any(|d| d.feature_id == s.feature_id), "{}", s.feature_module);
        }
        assert_eq!(PROGRAM_SWAPS.len(), SWAP_DETAILS.len());
    }
}
