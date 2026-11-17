# Liquid staking program: cluster-compatibility and fuzz testing

Status: working design shared by everyone building `tests/`. Keep it in sync with the code.

## Why

The program broke on mainnet without a single line of it changing: SIMD-0437 lowers
`lamports_per_byte` in steps (feature gates `set_lamports_per_byte_to_{6333,5080,2575,1322,696}`),
while core-BPF stake program v5 freezes `Meta.rent_exempt_reserve` at the legacy 2_282_880 and
derives the delegated amount from the *live* Rent sysvar. Every instruction that trusted
`meta.rent_exempt_reserve` (merge_stakes, create_canonical_stake, deposit_stake_account,
update_deactivated) started to revert or mis-account (fixed in `d4e2023`).

The runtime the program runs on is a moving target that differs per cluster: feature gates,
sysvar values, core BPF program binaries (stake, ALT, token -> p-token, ...), validator version.
Testnet usually runs ahead of mainnet. We want to:

1. know, every week, what differs between mainnet and testnet/devnet (and what agave has queued);
2. run the program against each of those differences *before* they reach mainnet, including the
   transition (state created before activation, exercised after it);
3. fuzz instruction sequences against accounting invariants;
4. backtest: any program commit against any profile, so a regression like the rent one is
   reproducible (pre-fix commit fails, fix commit passes).

## Layout

`tests/` is its own cargo workspace. It is **not** a member of the root workspace, so the
program's `Cargo.lock` and its verifiable build are untouched by the test dependencies.

```
tests/
  Cargo.toml              # [workspace] members = ["harness"] (cluster-profile is its own workspace)
  rust-toolchain.toml     # stable toolchain for the host-side test crates
  profiles/               # committed cluster snapshots (JSON) + content-addressed program binaries
    mainnet.json testnet.json devnet.json
    programs/<sha256>.so
  cluster-profile/        # bin `cluster-profile`: snapshot | diff | list-upcoming
  harness/                # lib + tests: LiteSVM env, profile application, Marinade fixture,
                          # scenario tests, transition tests, stateful fuzz
  scripts/                # matrix.sh (profiles x tests), backtest.sh (commits x profiles)
```

The program under test is always a compiled `.so` (built by `cargo build-sbf`, by `solana-verify`,
or the binary dumped from mainnet). The harness crate depends on `programs/marinade-finance`
(`features = ["no-entrypoint"]`) only for instruction/account types. The ABI did not change
between `07ffbf7`, `d4e2023` and `HEAD`, so the HEAD harness drives older binaries too.

## Engine

LiteSVM `0.18.2` (agave 4.3.2 runtime; mainnet runs 4.4, testnet 4.5-beta). In-process, fast enough
for fuzzing, full transaction semantics, and it lets us set the feature set, sysvars and program
binaries freely. Limitations we accept and report instead of hiding:

* features that agave added after 4.3.2 are unknown to the engine's `agave-feature-set`; a profile
  that activates one is reported as *unmodeled*, never silently dropped;
* features whose effect happens at activation time inside `Bank` (sysvar rewrites, core BPF program
  upgrades from a buffer) are not executed by LiteSVM. We model them explicitly in
  `harness/src/effects.rs`; the snapshot carries the real post-activation values, so the cluster
  profiles themselves need no model. The model is only needed for *what-if* toggles.

## Profile JSON (schema 1)

Produced by `cluster-profile snapshot`, consumed by the harness. Feature entries also carry
`"module"`: the agave module path (`set_lamports_per_byte_to_2575`, `full_inflation::devnet_and_testnet`),
parsed from agave sources, so spec strings can name features.

```json
{
  "schema": 1,
  "name": "mainnet",
  "cluster": "mainnet-beta",
  "rpc": "https://api.mainnet-beta.solana.com",
  "fetched_at": "2026-11-17T16:00:00Z",
  "slot": 470590808,
  "epoch": 1089,
  "version": { "solana-core": "4.4.0", "feature-set": 2142755730 },
  "features": {
    "<feature id base58>": { "name": "<description from agave-feature-set>", "status": "active", "activated_at": 123 },
    "<feature id base58>": { "name": "...", "status": "pending" },
    "<feature id base58>": { "name": "...", "status": "inactive" }
  },
  "sysvars": {
    "rent": { "lamports_per_byte_year": 5080, "exemption_threshold": 1.0, "burn_percent": 50 },
    "epoch_schedule": { "slots_per_epoch": 432000, "leader_schedule_slot_offset": 432000,
                        "warmup": false, "first_normal_epoch": 0, "first_normal_slot": 0 }
  },
  "programs": {
    "Stake11111111111111111111111111111111111111": {
      "name": "stake", "owner": "BPFLoaderUpgradeab1e11111111111111111111111",
      "sha256": "<hex>", "len": 212056, "file": "programs/<sha256>.so"
    },
    "Vote111111111111111111111111111111111111111": { "name": "vote", "owner": "NativeLoader1111111111111111111111111111111", "builtin": true }
  },
  "buffers": {
    "<buffer address>": { "for_feature": "<feature id>", "target_program": "<program id>",
                          "sha256": "<hex>", "len": 1, "file": "programs/<sha256>.so" }
  }
}
```

* `status`: `active` = feature account exists with `activated_at = Some`; `pending` = account exists,
  `activated_at = None` (activates at the next epoch boundary); `inactive` = no account.
  The feature list is every id in the newest `agave-feature-set` the tool is built with.
* `sha256`/`len`/`file`: over the ELF bytes only (programdata header stripped, cut at the ELF end
  computed from its headers; trailing allocation padding is not part of the program). `file` is
  relative to the profile JSON.
* `programs` always includes: stake, vote, system, address lookup table, config, feature gate,
  bpf loaders, SPL Token, Token-2022, ATA, and Marinade itself
  (`MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD`, only meaningful on mainnet).
* `buffers`: source buffers of pending core-BPF upgrades/migrations named by `effects.rs`, so a
  *what-if* activation can load the exact binary the cluster will switch to.

## Profile spec strings (harness input)

Everything that selects a runtime is a *spec string*, so the matrix and the backtest only pass
environment variables:

```
<profile.json>[,+<feature>][,-<feature>][,rent=<lamports_per_byte>][,program:<id>=<file.so>]
```

* `<feature>` is a base58 id or the agave module name (`set_lamports_per_byte_to_2575`).
* `+F` activates F and applies its modeled effects (rent rewrite, program upgrade from the
  buffer in the profile); `-F` deactivates it and reverts the modeled effect where the model
  can derive the previous value (rent is derived from the whole feature set, so this works).
* `builtin` instead of a path = LiteSVM defaults (`with_mainnet_features`, bundled stake v5),
  for running without any snapshot.

Harness environment variables:

| var | default | meaning |
|---|---|---|
| `MARINADE_SO` | `../target/deploy/marinade_finance.so` (repo build) | program under test; `profile` = the Marinade binary inside the base profile |
| `HARNESS_BASE` | `profiles/mainnet.json` if present, else `builtin` | runtime the tests start on |
| `HARNESS_TARGET` | = `HARNESS_BASE` | runtime the cluster "upgrades" to mid-test in transition tests; pure tests run on it too |
| `HARNESS_REPORT` | unset | if set, append one JSON line per test outcome (for matrix.sh) |

## Test kinds

1. **Scenario tests** (`harness/tests/scenarios.rs`): one instruction family each, run on `TARGET`.
2. **Transition tests** (`harness/tests/transition.rs`): build state on `BASE` over several epochs
   (deposits, stake_reserve, activation, rewards, pending unstakes), switch the same SVM to `TARGET`
   (feature set, sysvars, program binaries), then run the full crank cycle and user flows.
   The rent regression is the canonical case: `BASE = mainnet,-set_lamports_per_byte_to_6333,-set_lamports_per_byte_to_5080`,
   `TARGET = mainnet`; `07ffbf7` must fail, `d4e2023`/HEAD must pass.
3. **Stateful fuzz** (`harness/tests/fuzz.rs`): proptest-generated op sequences (user ops, crank ops,
   epoch advances with rewards/MEV/slashing, optional mid-sequence transition), invariants checked
   after every op. Deterministic seeds, shrinking, `PROPTEST_CASES` scales CI vs weekly.
4. **Pure property tests** on `calc.rs` / fees (host-side, no SVM).

## Invariants (checked after every op)

* `state.msol_supply == msol_mint.supply`
* reserve PDA lamports == `state.available_reserve_balance` + reserve rent-exempt amount
* `validator_system.total_active_balance == sum(validator.active_balance)`
* `stake_system` bookkeeping matches the stake list (count, delegated sums of tracked accounts)
* every lamport accounted for: sum(stake accounts) + reserve + sol leg + ticket balances vs
  `total_lamports_under_control` and friends (exact relations documented in `invariants.rs`)
* mSOL price never decreases without a slashing op
* no op fails with a panic / arithmetic overflow / non-Marinade error; expected Marinade errors are
  allow-listed per op
* every program-owned and Marinade-controlled stake account stays rent-exempt under the live Rent

## Matrix and weekly job

`scripts/matrix.sh`: snapshot (or reuse committed) profiles, compute *upcoming* = active or pending
on testnet/devnet but not on mainnet, plus anything listed with `--try`, then run all tests for:
`mainnet`, `testnet`, `devnet`, `mainnet,+F` for each upcoming F, and `mainnet -> mainnet,+F`
transitions. A target that fails where `mainnet` passes is a finding and names its cause (feature,
program hash, sysvar). Weekly GitHub workflow runs it for HEAD and for the binary live on mainnet.

`scripts/backtest.sh <commit>...`: build each commit (toolchain per commit), run the matrix with it.
