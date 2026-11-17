# Feature activation effects (model behind `harness/src/effects.rs`)

Verified 2026-11-17 against agave `v4.4.1` (mainnet runs 4.4.0), `v4.5.0-beta.0` (testnet and
devnet), `master` (5.0.0-alpha.0, commit 0c0870c), LiteSVM 0.18.2 (agave 4.3.2 runtime) and the
core BPF stake program `solana-program/stake` tags `program@v5.0.0` / `program@v5.1.0`.
Line numbers are for `v4.4.1` unless said otherwise. Sources were only read, never built.

## 1. Rent sysvar

Where: `Bank::compute_and_apply_new_feature_activations` (`runtime/src/bank.rs:6404`), called from
`process_new_epoch` (`bank.rs:1928`) on the first slot of the epoch in which the feature
account (pending) turns active, before rewards are computed. `activated_at` of the feature
account is that slot, so all features activating at one boundary share one slot. Order inside the
function, which fixes the arithmetic:

1. `deprecate_rent_exemption_threshold` (`rent6iV...`, SIMD-0194), `bank.rs:6431` ->
   `RentCollector::deprecate_rent_exemption_threshold` (`runtime/src/rent_collector.rs:49`):
   ```
   lamports_per_byte  = (lamports_per_byte as f64 * f64::from_le_bytes(exemption_threshold)) as u64
   exemption_threshold = 1.0
   burn_percent        = 50
   ```
   Genesis 3480 * 2.0 = 6960 exactly, so the stake minimum stays 2_282_880. Then `update_rent()`
   (`bank.rs:2882`) rewrites the sysvar account. The same function also runs at genesis
   (`compute_and_apply_genesis_features`, `bank.rs:6346`) when the feature is active there.
2. SIMD-0437 steps (`bank.rs:6437-6467`): an array `[6333, 5080, 2575, 1322, 696]`; for each id
   in array order that is in `new_feature_activations`, `rent.lamports_per_byte = constant` and
   `update_rent()`. Comment in source: "if multiple activate in one epoch, the lowest activated value
   will be used. If features are activated out of order, the most recently activated value will be used."
   Plain assignment, no rounding. **Nothing requires the threshold deprecation**: the comment says "all
   assume" it. Without it the threshold stays 2.0 and the minimum doubles (`Rent::minimum_balance`
   multiplies by 2).
3. SIMD-0438 `set_lamports_per_byte_to_6960` (`rnt8ZQp...`, `bank.rs:6475`): assigns 6960 after the
   steps of the same boundary ("the safeguard must override it"). A step activating in a later
   epoch overrides it again.

Semantics of the stored number: since `solana-rent 4.1` the field is `lamports_per_byte`
(`exemption_threshold` and `burn_percent` are `#[deprecated]`, `solana-rent-4.4.0/src/lib.rs`).
The wire layout is unchanged (u64, 8 bytes of f64, u8; 17 bytes). With threshold 1.0 (always
the case after SIMD-0194) it is the full exemption price per byte and "per year" no longer exists
(rent collection is gone). `Rent::minimum_balance` = `(128 + len) * lpb` for threshold 1.0,
`2 * (128 + len) * lpb` for 2.0, f64 path otherwise. `Rent::default()` in 4.4 is already 6960 /
1.0 / 50. Existing accounts are never touched: lowering the price leaves stake accounts
with `Meta.rent_exempt_reserve = 2_282_880` (the stake program writes that constant, see 4).

`rent_for` therefore returns, for any active set (canonical order: deprecation, steps by array
order, then 6960): start at 3480 / 2.0 / 50; deprecation if active (fold); the active step with
the highest array index (= lowest price); 6960 if active (wins). `rent_for_activations` replays
real activation slots (handles out-of-order steps and a step older than the deprecation, which
gets multiplied by 2.0: 5080 -> 10160).

| active | lamports_per_byte | threshold | stake (200 B) minimum |
|---|---|---|---|
| none (genesis) | 3480 | 2.0 | 2_282_880 |
| deprecation | 6960 | 1.0 | 2_282_880 |
| + 6333 | 6333 | 1.0 | 2_077_224 |
| + 5080 (mainnet/testnet/devnet today) | 5080 | 1.0 | 1_666_240 |
| + 2575 | 2575 | 1.0 | 844_600 |
| + 1322 | 1322 | 1.0 | 433_616 |
| + 696 | 696 | 1.0 | 228_288 |
| + 6960 safeguard | 6960 | 1.0 | 2_282_880 |

Live check (getAccountInfo SysvarRent): all three clusters `5080 / 1.0 / 50`,
`getMinimumBalanceForRentExemption(200) = 1_666_240`.

Master / 4.5 differences: `bank.rs` on `v4.5.0-beta.0` and master no longer contains the
`deprecate_rent_exemption_threshold` handler (feature cleanup; genesis/default Rent already is
6960/1.0). The SIMD-0437/0438 block is unchanged (4.5.0-beta.0 `bank.rs:6361-6403`, master
`6318-6360`). So the threshold-deprecation toggle is only meaningful for pre-4.5 what-ifs. master adds
`remove_runtime_float_ops` (inflation only).

LiteSVM 0.18.2 (`src/lib.rs:589-649`, `set_sysvars`): derives Rent from the feature set **once, when
`with_sysvars()` runs** (legacy 3480/2.0 if the deprecation is inactive, then each active step in
array order, 6960 last). It is correct for any set known at construction, but a later
`set_feature_set` does not rewrite Rent: use `effects::rent_for` and `set_sysvar` for transitions.

## 2. Program swaps (core BPF upgrades, p-token, p-ATA)

Mechanism (`runtime/src/bank/builtins/core_bpf_migration/mod.rs`):

* `upgrade_core_bpf_program` (`:331`): target must be an executable loader-v3 Program with a
  ProgramData; source must be a loader-v3 `Buffer` owned by the upgradeable loader. New
  ProgramData: `slot = bank.slot`, authority = the target's current authority (if it is `Some`
  the buffer authority must equal it, else `UpgradeAuthorityMismatch`), lamports =
  live-Rent minimum for the new size, ELF = buffer bytes after the 37-byte metadata (including
  trailing allocation padding). `directly_invoke_loader_v3_deploy` runs the loader's ELF
  verification against the current runtime environment and updates the program cache. The old
  ProgramData and the buffer lamports are burned, the buffer is zeroed.
* `upgrade_loader_v2_program_with_loader_v3_program` (`:409`): target must be an executable
  loader-v2 program; creates a fresh Program + ProgramData pair (authority `None`), ProgramData
  may be pre-funded (system-owned) only with `allow_prefunded` (passed from
  `relax_programdata_account_check_migration` in 4.4, constant `true` in 4.5).
* agave >= 4.5 only: with `core_bpf_hash_validation` active (it is not anywhere) the SHA-256 of
  the buffer ELF with trailing zeros trimmed must equal a constant (`SourceBuffer::
  new_checked_with_verified_build_hash`). Note this hash trims zeros, unlike `len` in the profile.
* Builtin -> core BPF migrations (`apply_new_builtin_program_feature_transitions`, `bank.rs:6529`):
  in `v4.4.1`, 4.5.0-beta.0 and master every `BUILTINS[..].core_bpf_migration_config` is `None`
  outside `dev-context-only-utils` (`builtins/src/lib.rs`), `STATELESS_BUILTINS` is empty. There is no
  production migration pending. Stake, ALT, config and feature-gate were migrated earlier and
  cleaned up.

Handlers present per agave version (fetched from `raw.githubusercontent.com/anza-xyz/agave/<tag>/runtime/src/bank.rs`):

| feature | id | target <- source buffer | handler in agave |
|---|---|---|---|
| `vote_state_v4` | `Gx4XFcr...` | Stake <- `BM11F4hq...` | 3.1.x, 4.0.0 only; none in 4.2.0+ (dead constant) |
| `replace_spl_token_with_p_token` (SIMD-0266) | `ptokFjw...` | Tokenkeg (loader v2) <- `ptok6rng...` | 3.1 - 4.4.x; removed in 4.5 |
| `upgrade_bpf_stake_program_to_v5` (SIMD-0490) | `STk5Xj8...` | Stake <- `4EBQBjw1...` | 4.0 - 4.4.x; removed in 4.5 |
| `upgrade_bpf_stake_program_to_v5_1` (SIMD-0391) | `s51VGwC...` | Stake <- `p51x11QC...` | 4.2.0 - 4.3.x; removed in 4.4.0 |
| `replace_ata_with_p_ata` (SIMD-0567) | `pata6uF...` | ATokenGPv (loader v2) <- `pataAAkF...` | 4.5.0-beta.0 and master |

Handlers are deleted once a feature is active everywhere, so a *what-if* for an old feature
cannot rely on agave source alone; and the source buffers are consumed at activation.

Does LiteSVM 0.18.2 reproduce them? Not as activation effects. `programs/mod.rs` only picks the
binary at construction (`with_default_programs`): p-token `pinocchio_token_program.so` (loader v3)
if `replace_spl_token_with_p_token` is active in the feature set at that moment, else
`spl_token-3.5.0.so` (loader v2); `spl_associated_token_account-1.1.1.so` (loader v2, 105_032
bytes, same size as the clusters' ATA) always; stake is **always `core_bpf_stake-5.0.0.so`
(202_280 bytes) regardless of any feature**. Clusters run v5.1.x (section 3), so a profile
must load the stake binary from the snapshot instead of using LiteSVM's. ALT is loaded as core BPF
always. `p-ATA` is not modeled by LiteSVM (needs the buffer binary from the profile).

## 3. Live status (RPC, 2026-11-17)

mainnet-beta v4.4.0 epoch 1089 (slot 470596160); testnet v4.5.0-beta.0 epoch 1090; devnet v4.5.0-beta.0 epoch 1211.
`A@slot` = active since that slot (first slot of an epoch), `-` = feature account missing (inactive),
no feature account is `pending` on any cluster.

| feature | mainnet | testnet | devnet |
|---|---|---|---|
| deprecate_rent_exemption_threshold | 407376000 | 386300256 | 441936000 |
| set_lamports_per_byte_to_6333 | 444096000 | 434684256 | 491184000 |
| set_lamports_per_byte_to_5080 | 446256000 | 437708256 | 492912000 |
| set_lamports_per_byte_to_2575 | - | - | - |
| set_lamports_per_byte_to_1322 | - | - | - |
| set_lamports_per_byte_to_696 | - | - | - |
| set_lamports_per_byte_to_6960 (SIMD-0438) | - | - | - |
| vote_state_v4 | 409968000 | 387596256 | 446256000 |
| replace_spl_token_with_p_token | 419472000 | 396236256 | 451008000 |
| upgrade_bpf_stake_program_to_v5 | 427248000 | 407036256 | 460080000 |
| upgrade_bpf_stake_program_to_v5_1 | 443232000 | 431228256 | 488592000 |
| replace_ata_with_p_ata | - | - | - |
| core_bpf_hash_validation | - | - | - |
| remove_inactive_stakes | - | 461468256 | - |
| alpenglow | 459648000 | 444620256 | 504144000 |
| double_disinflation_rate | - | - | - |
| remove_runtime_float_ops | - | - | - |
| block_revenue_sharing | - | - | - |
| loaderv3_programdata_size_floor | 466128000 | 463196260 | 519264000 |
| relax_programdata_account_check_migration | 412992000 | 395372256 | 450576000 |
| reduce_slot_time_to_350 / 300 / 250 / 200 ms | 440208000 / 441936000 / 447552000 / 454464000 | 426476256 / 427340256 / 428204256 / 429068256 | 481680000 / 482976000 / 485136000 / 487296000 |
| zk_elgamal_proof_program_enabled | 315792000 | 302924256 | 346032000 |

(5Ach9H8V..., 63Zvc4ix... loader-v3 features: inactive everywhere; `6TkHkRmP...` raise_cpi_nesting_limit_to_8: inactive everywhere.)

Source buffers (getMultipleAccounts):

| buffer | mainnet | testnet | devnet |
|---|---|---|---|
| `pataAAkF439TTjwRqmxRkTTEfZemRdoriXusez4PQ6S` (p-ATA) | exists, 35_869 B, 182_905_400 lamports, loader v3 Buffer, authority `8pEc9Hb...`, ELF 35_832 B (35_817 trimmed), trimmed SHA-256 `4e008052...7d9e` = `BUILD_HASH` | same | same |
| `ptok6rng...` (p-token) | missing (consumed) | missing | missing |
| `4EBQBjw1...` (stake v5) | missing | missing | missing |
| `p51x11QC...` (stake v5.1) | missing | missing | missing |
| `BM11F4hq...` (vote_state_v4 stake) | missing | missing | missing |

Post-activation binaries on all clusters: Stake ProgramData `6WU8Nxarf9fudRK5atWwjLY4vFaw5UrrWhL88qz7iCMJ`, 212_056 B
(ELF 212_041 trimmed), deployment slot = the v5.1 activation slot of each cluster (443232000 /
431228256 / 488592000), authority None, identical bytes on all three (trimmed SHA-256
`be260eeb07e6a7e8...`). Tokenkeg ProgramData `3gvYRKWy...`, 108_600 B, slot = p-token activation slot,
authority None, identical on all three. ATA is still loader v2 (105_032 B) everywhere.
So today `mainnet -> mainnet,+replace_ata_with_p_ata` is the only pending program swap that can be tested
with exact bytes; the next Rent steps (2575, 1322, 696) and SIMD-0438 have no on-chain account yet.

## 4. The stake program cannot see the feature set

Confirmed from `program@v5.1.0` (`program/src/processor.rs`, `lib.rs`, `helpers/`): the program is a plain
`solana-program` entrypoint; it has no access to `FeatureSet`, and it contains no feature checks
(`grep feature` finds nothing). Behavior changes only through (a) program upgrades (new ELF; v5.0 -> v5.1
changed the warmup/cooldown arithmetic from f64 to integer basis points, `stake_v2` /
`warmup_cooldown_rate_bps`) and (b) sysvar values. Sysvars it reads, all through syscalls:

* `Rent::get()` (Initialize, DelegateStake, Split, Merge, Withdraw, MoveStake/MoveLamports): every
  rent-exempt reserve is `rent.minimum_balance(data_len)` live. `Meta.rent_exempt_reserve` is
  *written* as `PSEUDO_RENT_EXEMPT_RESERVE = 2_282_880` (Initialize, Split destination) and never
  read ("ignore it in the stake program", `lib.rs:25`). Delegated amount =
  `lamports - rent.minimum_balance(len)` (`helpers/delegate.rs:37`). This is the Marinade incident.
* `Clock::get()` (epoch for activation state, lockups, deactivation).
* `StakeHistorySysvar(clock.epoch)` (reads the stake history account through `sol_get_sysvar`).
* `EpochRewards::get()` (`processor.rs:1426`): while `active` every instruction except
  `GetMinimumDelegation` fails with `EpochRewardsActive`. This matters for harness epoch
  advances: the sysvar must be inactive again (LiteSVM default is inactive).
* No `EpochSchedule`, no epoch stake syscall, no feature accounts. The minimum delegation is the constant 1 SOL
  (`get_minimum_delegation`). Vote state is read from the passed vote account's data (vote program v4 layout).
  The deprecated stake Config account is accepted but unused (LiteSVM still creates it).

Conclusion for the test design: a rent step is fully emulated by writing the Rent sysvar; a stake
program change by loading another ELF at `Stake111...`; nothing else needs the feature set. The
feature set matters to the *runtime* (loaders, syscalls, cost, account rules, vote program), which LiteSVM
consults at execution time.

## 5. Unmodeled activation-time effects (also `effects::UNMODELED`)

None of these touches stake accounts, the stake history, or programs Marinade calls:

* `pico_inflation`, `full_inflation::*`, `double_disinflation_rate`, `remove_runtime_float_ops`: Bank rewrites
  `Inflation` (+ `burn_percent` for pico/full). Rewards are injected by the harness by hand.
* `reduce_slot_time_to_*`: `apply_slot_time_persistent_changes` updates ns_per_slot, slots_per_year, hashes_per_tick; no sysvar.
* `alpenglow`: creates epoch inflation/vote reward account state at the epoch boundary
  (`bank.rs:1979`), stops hashes_per_tick refresh. Active everywhere.
* `remove_inactive_stakes` (testnet only), `block_revenue_sharing`: epoch-boundary rewards / stakes cache.
* `zk_token_sdk_enabled`, `zk_elgamal_proof_program_enabled`: add a builtin at activation; LiteSVM adds builtins at construction.
* `core_bpf_hash_validation`: validation, not an effect; we do not verify buffer hashes.
* Not a feature: LiteSVM's bundled stake v5.0.0 (see section 2); load the profile's stake ELF.
* Generic per-epoch work that is not feature gated and is not modeled by LiteSVM: stake history append, epoch rewards
  partitioned distribution (`EpochRewards.active` window), vote/stake rewards crediting. The harness does these by hand.
* No sysvar is created at feature activation in 4.4.1/4.5/master (`SlotHashes`, `LastRestartSlot`, `EpochRewards`,
  `StakeHistory` exist from genesis; LiteSVM seeds them in `set_sysvars`).
* Capitalization effects (lamports burned/funded for new ProgramData, `update_captalization`) are not tracked.
