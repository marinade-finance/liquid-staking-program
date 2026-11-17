# Cluster-compatibility tests

The program broke on mainnet without a line of it changing (SIMD-0437 rent steps plus core-BPF stake v5, fixed in `d4e2023`).
This directory tests the program against the runtime it will actually run on: snapshots of mainnet, testnet and devnet, and what-if feature activations.
Design and contracts: [ARCHITECTURE.md](ARCHITECTURE.md). Why features behave as they do: [docs/feature-effects.md](docs/feature-effects.md).

| piece | what it does |
|---|---|
| `cluster-profile/` | `snapshot` a cluster into JSON (features, sysvars, core programs), `diff` clusters, `list-upcoming` features |
| `harness/` | LiteSVM test harness: scenarios, mainnet-to-target transition tests, stateful fuzz |
| `trident/` | Trident instruction-level fuzzing: every instruction with fuzzed arguments and mutated account sets, judged by oracles + invariants ([trident/README.md](trident/README.md)) |
| `scripts/matrix.sh` | program binaries x runtime targets, classifies findings |
| `scripts/backtest.sh` | build any commit (right toolchain), run it against runtime profiles |
| `profiles/` | committed snapshots (`mainnet.json`, `testnet.json`, `devnet.json`, `programs/<sha256>.so`) |

CI: `.github/workflows/tests.yml` (push/PR, light, includes a Trident smoke) and `.github/workflows/cluster-compat.yml` (weekly + manual, full matrix and a long Trident campaign, opens or updates one issue labelled `cluster-compat`).

## Prerequisites

* Rust: `tests/rust-toolchain.toml` pins the host toolchain (rustup installs it on first use: `cd tests && rustup toolchain install`).
* `jq`, `curl`, `git`; shellcheck is optional.
* Solana 2.3.13 (`cargo-build-sbf`) to build the program; platform tools `v1.52` are used (`cargo-build-sbf --tools-version v1.52`). Anchor 0.27 commits need Solana 1.18.26 (backtest.sh downloads it).
* Network access only for `snapshot` and for downloading toolchains.

## Commands

```bash
# 1. build the program under test (HEAD)
cargo-build-sbf --tools-version v1.52 --manifest-path programs/marinade-finance/Cargo.toml --sbf-out-dir target/deploy

# 2. harness tests against the committed mainnet profile (env vars are the whole interface)
cd tests && MARINADE_SO=../target/deploy/marinade_finance.so PROPTEST_CASES=64 cargo test

# 3. full matrix for HEAD and for the binary live on mainnet (uses the committed profiles)
tests/scripts/matrix.sh --so head=target/deploy/marinade_finance.so --so profile --fuzz-cases 200
#    `--so profile` runs with HARNESS_RENT_TOPUP=1 (label live+topup); add --no-topup for the bare live binary (label live)

# 4. same with fresh cluster snapshots, plus extra what-if features
tests/scripts/matrix.sh --snapshot --so head=target/deploy/marinade_finance.so --try-master --try set_lamports_per_byte_to_2575

# 5. backtest commits (default runtime = the rent regression)
tests/scripts/backtest.sh 07ffbf7 d4e2023 HEAD
```

Harness environment: `MARINADE_SO` (path, or `profile` = the Marinade binary inside the base profile), `HARNESS_BASE`, `HARNESS_TARGET` (spec strings, below), `HARNESS_REPORT` (JSON lines, one per test), `HARNESS_RENT_TOPUP` (operational mode, below), `PROPTEST_CASES`.
A spec string is `<profile.json|builtin>[,+F][,-F][,rent=N][,program:<id>=<file.so>]`, where `F` is a feature id or agave module name.
`matrix.sh --dry-run` prints the targets and exits without running anything.

## Operational mode: the live binary with the crank bot

The binary live on mainnet predates the rent fix, so on its own it fails under today's Rent; on mainnet marcrank's `top-up-stake-rent` hides that. `HARNESS_RENT_TOPUP=1` makes the harness do what the bot does (same rule as marcrank, run at the start of every crank and before every direct crank op), so the live binary can be tested the way it actually runs:

```bash
cd tests && MARINADE_SO=profile HARNESS_RENT_TOPUP=1 cargo test --no-fail-fast   # live binary + bot
cd tests && MARINADE_SO=profile cargo test --no-fail-fast                         # bare live binary: 7 failures, all the rent gap
```

The flag only affects the binary of the base profile (HEAD behaves the same with and without it; `HARNESS_RENT_TOPUP=force` applies it to any binary). What the bot cannot fix is listed in `harness/src/topup.rs` (`known_live_limitations`: user stake accounts created under the reduced Rent cannot be deposited, `WrongStakeBalance`). Those failures print `KNOWN-LIVE-LIMITATION` lines (run with `-- --nocapture`) and count as passes, only with the flag and only on the live binary. Details: ARCHITECTURE.md, "Operational mode".

## Stateful fuzz and property tests

```bash
cd tests
# mainnet runtime, 1000 cases (default; ~30 s). BASE == TARGET: no transition
HARNESS_BASE=profiles/mainnet.json PROPTEST_CASES=1000 cargo test --test fuzz -- --nocapture
# "feature activates mid-life": one runtime switch at a random op index
HARNESS_BASE=profiles/mainnet.json HARNESS_TARGET=profiles/mainnet.json,+set_lamports_per_byte_to_2575 cargo test --test fuzz -- --nocapture
# pure arithmetic properties (calc.rs, fees, price helpers), no SVM
cargo test --test calc_props
# known program findings (each asserts the CORRECT behaviour, so they fail today and are #[ignore]d)
cargo test --test findings -- --ignored
```

`fuzz.rs` knobs: `PROPTEST_CASES`, `FUZZ_MAX_OPS` (default 40), `FUZZ_SEED` (u64; runs are deterministic, the seed is printed),
`FUZZ_SHRINK_ITERS`, `FUZZ_NO_PERSIST=1`, `FUZZ_REGRESSIONS=<file>` (default `harness/proptest-regressions/fuzz.txt`: failing
seeds are saved there and replayed first; commit it). `cargo test --test fuzz replay -- --ignored --nocapture` replays a
hand-written case with a trace. The test prints per-op outcome coverage and cases/s at the end. Failures print the shrunk
op list (with the transition marker) and a per-op trace.

## Instruction-level fuzzing (Trident)

The proptest fuzzer above drives *valid* user and crank sequences across runtimes. `trident/` attacks single
instructions instead: all 29 with edge-value arguments, account substitution (foreign instance, look-alike
clones, wrong mints, rogue stakes, dropped signer/writable flags), and garbage data, on agave 2.3 with the
program run natively (fast, line coverage) or as the SBF binary.

```bash
cd tests/trident
./run.sh all                                   # fuzz_ix + fuzz_flows; exit 1 on a signature not in known-findings.txt
./run.sh ix claim,stake_reserve                # focus on some instructions
./run.sh sbf fuzz_ix                           # real binary (target/deploy/marinade_finance.so)
./run.sh coverage 6000                         # 87% line coverage of programs/marinade-finance/src
```

Findings and their triage: [trident/README.md](trident/README.md#findings-all-triaged-by-reading-the-code-path-none-is-a-loss-of-funds-issue).

## Reading the matrix

`matrix.sh` runs every (binary, target) with `HARNESS_BASE` = mainnet profile and `HARNESS_TARGET` = the target.
Transition tests therefore model "mainnet upgrades to the target", and pure tests run on the target.
Targets: `mainnet` (baseline), `testnet`, `devnet`, and `mainnet,+F` for every feature that `cluster-profile list-upcoming` reports (active or pending elsewhere, not on mainnet), every `--try` feature and, with `--try-master`, every feature known only on agave master.

Binaries: `head` is your build; `live+topup` is the binary live on mainnet with the crank bot's stake rent top-up (operational mode, `--no-topup` gives the bare `live` binary, which fails on today's Rent by itself).

Result per cell:

* `pass (n/m)`: all tests passed.
* `FINDING`: a test fails on the target that passes on the mainnet baseline. The findings section names the cause: the feature id/module for `mainnet,+F`, and for whole profiles the feature, program and sysvar differences from `cluster-profile diff`.
* `BROKEN`: the baseline itself fails, i.e. the binary already fails on today's mainnet runtime.
* `unmodeled`: passed, but the harness printed `UNMODELED` for something in the target (a feature the engine does not know, or whose activation effect is not modeled). Treat as "not testable in-process", not as a pass.

Exit codes: 0 clean, 1 findings, 2 mainnet broken, 3 usage or environment error.
Outputs in `--out` (default `tests/matrix-out`): `summary.md`, `summary.json`, `targets.tsv`, `diff/`, and `runs/<binary>/<target>/{test.log,report.jsonl}` for each run.
In CI `summary.md` is also appended to the step summary.
The harness is built once (`cargo test --no-run`); every run only changes environment variables.

## Trying a feature gate by hand

```bash
tests/scripts/matrix.sh --so head=target/deploy/marinade_finance.so --try set_lamports_per_byte_to_2575,set_lamports_per_byte_to_6960 --tests transition
```

or directly:

```bash
cd tests
HARNESS_BASE=profiles/mainnet.json HARNESS_TARGET=profiles/mainnet.json,+set_lamports_per_byte_to_1322 \
  MARINADE_SO=../target/deploy/marinade_finance.so cargo test transition -- --nocapture
```

`+F` activates F with its modeled effects (rent rewrite, core-BPF swap from the buffer stored in the profile); `-F` removes it again. Use the Actions "cluster-compat" workflow (`try_features`) to do the same in CI.

## Backtesting

```bash
tests/scripts/backtest.sh [--base SPEC] [--target SPEC]... [--revert <commit>] [--verifiable] <commit|path.so>...
```

Each commit is built in a cached git worktree (`~/.cache/marinade-backtest`, `BACKTEST_CACHE`). The toolchain comes from the commit's `Cargo.lock`: anchor-lang 0.27 uses Solana 1.18.26, 0.31 uses Solana 2.3.13 with `--tools-version v1.52`. Override with `SOLANA_LEGACY_BIN` / `SOLANA_BIN`; missing toolchains are downloaded from release.anza.xyz. Binaries are cached by commit sha.
`--revert c` tests HEAD with commit `c` reverted. `--verifiable` builds with `solana-verify` in docker as in [Docs/DEPLOYMENT.md](../Docs/DEPLOYMENT.md) (CI or a machine with docker only).
The default runtime is the rent regression: base = mainnet without `set_lamports_per_byte_to_6333` and `_5080`, target = mainnet. `07ffbf7` must fail, `d4e2023` and HEAD must pass. The output is a program x target table.

## Refreshing the committed profiles

```bash
cd tests/cluster-profile && CARGO_TARGET_DIR=../target-profile cargo build --release && cd ..
for c in mainnet testnet devnet; do target-profile/release/cluster-profile snapshot --cluster $c --out profiles; done
target-profile/release/cluster-profile diff profiles/mainnet.json profiles/testnet.json profiles/devnet.json
```

Use a reliable RPC (`--rpc`) if the public endpoint rate-limits. Commit the JSON files together with the new `profiles/programs/*.so`. The weekly workflow reports drift between the committed and fresh profiles; it never commits them itself.

## Known limits

* The engine is LiteSVM 0.18.2, i.e. the agave 4.3.2 runtime. Mainnet runs 4.4, testnet 4.5 beta. Features newer than the engine are `unmodeled`; behaviour of the runtime code itself (as opposed to feature flags, sysvars and program binaries) is that of 4.3.2.
* Activation effects that happen inside `Bank` (sysvar rewrites, core-BPF upgrades) are modeled by hand in `harness/src/effects.rs` for what-if toggles; cluster snapshots carry the real post-activation values and need no model. Unmodeled effects are reported, never dropped.
* Validator client differences (Firedancer vs Agave), consensus, scheduling, fees/priority, and RPC behaviour are not covered.
* Snapshots are point in time. The mainnet profile is what the weekly job last saw, not what will be live after the next epoch boundary (pending features are listed as such).
* The program under test builds with anchor 0.31 sources only in CI; older commits go through `backtest.sh`.
