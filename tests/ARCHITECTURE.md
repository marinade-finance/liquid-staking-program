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
| `MARINADE_SO` | `../target/deploy/marinade_finance.so` (repo build) | program under test; `profile` = the Marinade binary inside the base profile (`HARNESS_BASE`, whatever the target is; committed mainnet profile if the base has none) |
| `HARNESS_BASE` | `profiles/mainnet.json` if present, else `builtin` | runtime the tests start on |
| `HARNESS_TARGET` | = `HARNESS_BASE` | runtime the cluster "upgrades" to mid-test in transition tests; pure tests run on it too |
| `HARNESS_REPORT` | unset | if set, append one JSON line per test outcome (for matrix.sh) |
| `HARNESS_RENT_TOPUP` | unset (off) | operational mode, see below. `1` = run marcrank's stake rent top-up next to the binary live on mainnet (the Marinade binary of the base profile; any other binary is left alone); `force` = next to any binary; `0` = off |

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

## Operational mode (`HARNESS_RENT_TOPUP`)

The Marinade binary live on mainnet predates the rent fix `d4e2023`. Under today's Rent (5080/byte, stake rent 1_666_240)
`stake_reserve` creates stake accounts holding `delegation + 1_666_240` while stake v5 writes the frozen
`meta.rent_exempt_reserve = 2_282_880`; the old program requires `lamports == delegation + meta.rent_exempt_reserve` in
merge_stakes / create_canonical_stake (`SourceStakeMustBeUpdated`) and mis-accounts update_deactivated. On mainnet the
crank bot hides this: marcrank's `top-up-stake-rent` runs first in `do-work`. The bare live binary therefore fails most
tests ("MAINNET BROKEN"), which says nothing about upcoming features. With `HARNESS_RENT_TOPUP=1` the harness behaves like
the bot (`harness/src/topup.rs`):

* rule (`topup::top_up`, a verbatim port of marcrank, same unit tests): for every Marinade stake account that is delegated,
  `needed = delegation.stake + meta.rent_exempt_reserve - lamports` (saturating); `0` = left alone (MEV surplus included);
  `needed > gap` with `gap = min(meta.rent_exempt_reserve, 2_282_880) - live stake rent` = "needs triage", skipped, not
  funded; otherwise a system transfer of `needed`. At most 512 accounts per run. `Marinade::top_up_stake_rent(env, payer)`
  returns the number of accounts topped up.
* funding: a dedicated crank payer (`Marinade::rent_payer`), funded with `env.fund`, so the lamports are booked as
  injected and the conservation invariant stays exact. `invariants::check_fully_updated` accepts
  `lamports == delegation + meta.rent_exempt_reserve` in this mode.
* where: at the start of `Crank::start_of_epoch` / `do_work` (like marcrank), and immediately before every direct crank op
  wrapper on `Marinade` that reads stake balances (update_active, update_deactivated, merge_stakes,
  create_canonical_stake, deactivate_stake, partial_unstake, emergency_unstake), so transition.rs, scenarios.rs and the
  fuzzer get the same behaviour without knowing about it.
* which binary: `1` only affects the binary whose sha256 equals the Marinade binary of the base profile. A fixed binary
  (HEAD) is neither helped nor safe: the bot's transfer after `update_active` makes HEAD see lamports above
  `delegation + live rent` (`SourceStakeMustBeUpdated` in merge_stakes, found with `HARNESS_RENT_TOPUP=force`). HEAD
  therefore behaves identically with and without the flag.
* `topup::known_live_limitations()`: failures the bot cannot prevent. They are consulted only when the flag is set AND the
  binary is the live one; a matching failure prints a `KNOWN-LIVE-LIMITATION` line and counts as a pass (the failing step
  is skipped in `transition.rs`, ends the test in `scenarios.rs`, is judged as an expected rejection in the fuzzer).
  Anything else still fails. Today's list: `deposit_stake_account` of a user stake created under the reduced Rent
  (`WrongStakeBalance`: lamports are `delegation + live rent`, the program wants `+ 2_282_880`; the bot only funds
  Marinade's own accounts; `scenarios::deposit_stake_account_live_needs_user_topup` shows it works once the user sends the
  gap lamports themselves). This is the real user-facing state of mainnet today.

`matrix.sh --so profile` runs with `HARNESS_RENT_TOPUP=1` (label `live+topup`); `--no-topup` runs the bare binary (label `live`).

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
program hash, sysvar). Weekly GitHub workflow runs it for HEAD and for the binary live on mainnet (`live+topup`: with the crank bot's stake rent
top-up, see "Operational mode").

`scripts/backtest.sh <commit>...`: build each commit (toolchain per commit), run the matrix with it.
