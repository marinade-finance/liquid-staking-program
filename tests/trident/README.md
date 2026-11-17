# Trident instruction-level fuzzing

Per-instruction adversarial fuzzing of the Marinade program with [Trident](https://ackee.xyz/trident/docs/latest/)
(Ackee Blockchain). Complements the LiteSVM harness in `tests/harness` (lifecycle / transition / cluster-compat /
proptest sequences): here Trident generates the instruction **arguments and the account sets**, executes the program
(natively by default, or the compiled SBF binary) and judges every outcome with oracles and accounting invariants.

* Trident `0.12.0` (`trident-fuzz`, `trident-cli`, `trident-svm 0.2.0` = agave `solana-svm 2.3.13`, same as the Solana 2.3.13 CLI used for the program).
  `0.13.0-rc.4` exists (trident-svm 0.3.0-rc.4, also solana 2.3) but only adds features this setup does not need; stable was preferred.
* Own cargo workspace and `Cargo.lock` under `tests/trident/trident-tests/`; the program's root `Cargo.toml`/`Cargo.lock` and the
  verifiable build are untouched (the crate depends on `programs/marinade-finance` by path with `no-entrypoint`, like the harness).
  `tests/trident/Anchor.toml` is a stub so the Trident CLI (which wants `Anchor.toml` + `trident-tests/Trident.toml`) works from this subtree.
* The crate's `[profile.release]` sets `overflow-checks = true` like the program's root profile; keep it, otherwise arithmetic overflows would silently wrap.

## Layout

```
tests/trident/
  Anchor.toml  rust-toolchain.toml  run.sh  README.md
  trident-tests/
    Trident.toml  Cargo.toml  known-findings.txt
    src/world.rs       two Marinade instances (A under test, B = "someone else's"), unused C for `initialize`, users, attacker, validators, decoys
    src/ixs.rs         builders for all 29 instructions with a per-slot role (Fixed / Auth / New / Free + accepted kinds)
    src/mutate.rs      account-set mutation + the "this substitution must not succeed" oracle
    src/invariants.rs  accounting invariants on natively deserialized state
    src/engine.rs      step engine: args with edge values, mutation, execution, oracles, crank rounds, world actions, stats
    src/native.rs      executor selection (native entrypoint | MARINADE_SBF=<.so>)
    src/bin/fuzz_ix.rs      per-instruction adversarial target
    src/bin/fuzz_flows.rs   long mostly-valid sequences with epochs, rewards, slashing, crank rounds
```

## Prerequisites

```bash
cargo install --locked trident-cli --version 0.12.0       # optional: ./run.sh falls back to `cargo run`
rustup toolchain install 1.97.1 --component llvm-tools    # tests/trident/rust-toolchain.toml pins it
```

## Run

```bash
cd tests/trident
./run.sh build
./run.sh ix                         # all instructions (FUZZ_ITERS=20000 iterations x FUZZ_CALLS=60 steps)
./run.sh ix deposit,claim           # focus: 70% of steps hit these instructions, the rest are valid "driver" steps that move the state along
./run.sh flows                      # long valid-ish sequences
./run.sh all
./run.sh sbf fuzz_ix                # same fuzz code against target/deploy/marinade_finance.so (real SBF binary, real CU metering)
./run.sh coverage 3000              # llvm line coverage of programs/marinade-finance/src
./run.sh replay fuzz_ix <seed>      # replay one failing iteration
# equivalent CLI form:  cd trident-tests && FUZZ_ITERS=5000 trident fuzz run fuzz_ix
```

Environment knobs: `FUZZ_ITERS`, `FUZZ_FLOW_ITERS` (`all` only, default 5000), `FUZZ_CALLS` (steps per iteration), `FUZZ_ONLY=ix1,ix2`, `FUZZ_MUTATE_PCT` (default 65 for `fuzz_ix`, 3 for `fuzz_flows`),
`FUZZ_DONATE=0` (no direct lamport donations to the reserve PDA), `FUZZ_SLASH=0`, `FUZZ_OUT` (default `trident-tests/fuzz-out`),
`FUZZ_STRICT=1` (ignore `known-findings.txt`), `FUZZ_ALLOW=substr,...`, `MARINADE_SBF=<.so>`. Fewer threads: `taskset -c 0-1 ...` (Trident uses all visible CPUs).

### Reading the output

* A **finding** is an `assert`-style panic with signature `category:instruction:message`; Trident prints `Assertion failed ... (seed: <hex>)`.
  The first occurrence of each signature per process also writes `fuzz-out/findings/<signature>.txt` with the full instruction (every account slot, role, flags, kind tags), the
  applied mutations, program panic location and logs. Repeats are only counted. Replay: `./run.sh replay fuzz_ix <seed>` (single iteration, single thread).
* `fuzz-out/<target>-stats.txt`: `count<TAB>instruction<TAB>valid|mut|garbage<TAB>OK|ERR custom(code)|PANIC` plus `FINDING ...` counters and world actions. Use it to see which
  instructions / error codes are reached (`OK_noeffect` = mutated tx succeeded through an early-return no-op path and is not judged).
* `known-findings.txt` lists signatures already triaged so a run keeps going after them (see "Findings" below). An entry is one or more
  substrings joined by ` && ` that must all occur in the signature; keep it narrow (instruction, panic message, source file under `src/`) so that a
  *different* panic in the same instruction still surfaces. Program paths in signatures are trimmed to `src/...`, so entries do not depend on the checkout path.
* `run.sh` exits **1** when a run wrote a report under `findings/` (= a signature not in `known-findings.txt`), else 0. CI: `tests.yml` runs a
  smoke (`FUZZ_ITERS=3000`, `FUZZ_FLOW_ITERS=500`, ~1 min after the build) on push/PR; the weekly `cluster-compat.yml` runs `./run.sh all` with
  `FUZZ_ITERS=100000` (dispatch input `trident_iters`) and reports new findings in the `cluster-compat` issue.

### Coverage

`./run.sh coverage N` builds with `-C instrument-coverage` into `trident-tests/target-cov`, runs both targets for N iterations and writes
`coverage-summary.txt` (llvm-cov report), `coverage-html/` and `coverage.lcov`. This works because the program runs **natively** inside Trident's SVM.
Trident's own `[fuzz.coverage]` (CLI) needs `cargo-llvm-cov`; the script uses the llvm-tools from rustup directly.

## What is fuzzed

All 29 instructions in `lib.rs` (`initialize` against the never-initialized instance C and against re-initialization).

**Arguments**: 0, 1, 2, balance, balance+-1, u64::MAX(-1), random, log-uniform, around `min_deposit`/`min_stake`, indices out of range (`len`, `len+1`, `u32::MAX`), edge fees (0, 100%, 100%+1, `u32::MAX`),
`realloc` capacities (0, `u32::MAX`, below the used count), config values (all `ConfigMarinadeParams`/`ConfigLpParams` fields, optional), random `change_authority` targets, instruction-data garbage (truncated / extended / random discriminator, dropped / extra accounts).

**Account mutation** (1 to 3 per instruction, judged on the final account list): fresh random key, random pool account (users, authorities, PDAs, mints, token accounts, instance-B accounts, programs, sysvars, stake decoys),
duplicate of another slot, attacker key, signer flag dropped, writable flag dropped, swap of two slots, clone with a foreign owner, look-alike clone (same data and owner, other address),
decoys: user-owned delegated stake, stake authorised to A's PDAs but not in the list, stake authorised to B's PDAs, deactivated / initialised-only PDA stake, stake delegated to an unregistered vote, valid-but-unregistered vote account, a non-vote account of vote size, a ticket of instance B, zeroed tickets.

**Oracles** (after a successful transaction):

* *substitution must not succeed*: a `Fixed` slot (PDA, stored address, program, sysvar, tracked stake) replaced by anything else, an authority slot replaced / un-signed, a signer slot un-signed, a writable slot made read-only, a `Free` slot replaced by an account of the wrong kind. Only judged when the transaction changed instance A (no-op early returns prove nothing).
* *authority gate*: an authority-only instruction (`config_*`, `change_authority`, `add/remove_validator`, `set_validator_score`, `pause/resume`, `realloc_*`, `emergency_unstake`, `partial_unstake`) must have been signed by the pre-state authority.
* *no panic*: `ProgramFailedToComplete` is reported with the panic message and location (native: panic hook; SBF: log line). Native-executor artifacts (privilege-escalation / executable-balance panics from the syscall stub, which are plain `InstructionError`s on chain) are filtered.
* *lamport conservation* over every account that took part (fee is 0), *instance B untouched* by a transaction addressed to A.
* *invariants* (`src/invariants.rs`, same relations as `tests/harness/src/invariants.rs`): mSOL and LP supply equal the state fields; `reserve == available_reserve_balance + rent` (`>=` after a donation); sol leg >= rent;
  `total_active_balance`, scores, per-validator active balance, cooling-down sums equal the stake/validator lists; every listed stake account is stake-owned, delegated, authorised to the PDAs and rent exempt; ticket count/balance equal the ticket accounts;
  solvency (`reserve - rent + stake surplus >= total_lamports_under_control`); mSOL price never decreases (unless a slash was injected); identity / authority / `paused` fields only change in their instructions.

**World actions** (state evolution): epoch advance with slot inside / outside the stake-delta window, staking rewards, unrecorded lamports on a stake account, direct donation to the reserve PDA, slashing (`fuzz_flows`),
and scripted *crank rounds* (order unstake, update_active / update_deactivated, stake_reserve, deactivate / emergency / partial unstake, canonical stake + merge, claim) with valid instructions.

## Limitations

* Native execution runs the program source as a host binary inside Trident's SVM (agave 2.3.13 runtime, `SVMFeatureSet::default()` = **all runtime features off**, `Rent::default()`, built-in stake program, stake history sysvar is empty so (de)activation is instantaneous after one epoch).
  It does not model compute-unit consumption of the program body, stack/heap limits, or the SBF toolchain; `./run.sh sbf` runs the real binary (CU metering, same invariants) at roughly half the speed.
  Not covered: SIMD-0437 rent steps, core-BPF stake v5, p-token (see the LiteSVM harness for those).
* Signatures are not verified by Trident's SVM; "signer" is the `is_signer` flag. The mutator only grants it to keys that can sign (wallets, authorities), never to PDAs/mints.
* The legacy `finalize_delinquent_upgrade` path (state `IteratingValidators`) is unreachable from a fresh `initialize` (state is `Done`) and is only exercised for its error path.
* Account mutation is applied to one instruction per transaction; CPI-composed multi-instruction transactions are not generated.

## Results of the first campaign (HEAD, native executor unless noted)

~230k iterations of `fuzz_ix` (60 steps each, 65% mutation) + 32k iterations of `fuzz_flows` (150 steps) + 12k iterations of `fuzz_ix` against the SBF binary, ~45 minutes wall on 3 threads
(native: ~17k steps/s; SBF about half). Line coverage of `programs/marinade-finance/src` (6000 iterations of each target, `./run.sh coverage 6000`): **87.1% lines, 85.8% regions**
(`coverage-summary.txt`). Not reached: the legacy delinquent-upgrader path (`finalize_delinquent_upgrade` 11%, the `IteratingStakes/IteratingValidators` branches in `update.rs` / `deposit_stake_account.rs`, state is always `Done`),
`events/*` (derive-only code), `fee.rs` `TryFrom<f64>`/`FromStr` helpers (CLI-only), `add_with_balance` (auto-add on deposit is disabled), `WrongValidatorDuplicationFlag` in `remove_validator` (the Anchor seeds constraint rejects first).

### Findings (all triaged by reading the code path; none is a loss-of-funds issue)

| # | where | what | class |
|---|---|---|---|
| F1 | `state/mod.rs:281` `on_transfer_from_reserve`, hit by `stake_reserve` and `claim` | lamports sent directly to the reserve PDA make `available_reserve_balance` smaller than the real balance; `stake_reserve` (stake_delta uses the real balance) and `claim` (checks the real balance) then subtract more than the virtual balance: `attempt to subtract with overflow`. Reproduced natively and on the SBF binary. Anyone can trigger it with a plain transfer; cleared by `update_active/update_deactivated` which re-sync the field. | program bug (known), permissionless temporary DoS of `stake_reserve`/`claim` |
| F2 | `deactivate_stake.rs:182` (`2 * min_stake`), `partial_unstake.rs:144` (`target + min_stake`) | `config_marinade` accepts any `min_stake` (only a lower bound), a huge value makes these overflow-panic. | admin-only misconfiguration, add an upper bound / checked math |
| F3 | `validator_system.rs:190` (`add_validator`), `set_validator_score.rs:42` | `total_validator_score += score` overflows for scores near `u32::MAX`. | manager-only, reverts |
| F4 | `realloc_stake_list` / `realloc_validator_list`; `partial_unstake.rs:95`, `:137` | `record_size * capacity` (u32) overflows for huge `capacity`; `partial_unstake` panics instead of returning an error code: `assert!(desired >= min_stake)`, and, after a slash leaves ticket debt above active + reserve + cooling-down stake, `u64::try_from(total_active_balance + stake_delta).expect(..)` (`deactivate_stake` saturates the same quantity). | admin / manager only, reverts |
| F5 | Anchor-generated code of `initialize` (`state`) and `order_unstake` (`new_ticket_account`) | the `zero` constraint slices the first 8 bytes without a length check: an account with < 8 bytes of data gives `range end index 8 out of range` instead of an Anchor error. | framework behaviour, tx fails either way |
| F6 | `partial_unstake.stake_history` | no `address = stake_history::ID` (the other crank instructions have it); the account is only forwarded to the rent-return `withdraw` CPI, so any account is accepted. | info, harmless |

Not found in this campaign: any authority-gated instruction succeeding without the pre-state authority signing, any `Fixed` slot (PDA, stored address, tracked stake, program, sysvar) substitution that changed state and succeeded
(including instance-B accounts, look-alike clones, rogue PDA-authorised stakes), any violated accounting invariant, any cross-instance write, any lamport leak. `known-findings.txt` holds the signatures above so a run continues after them.

Harness problems found and fixed while triaging (kept here because they explain oracle design): substituted slots must be judged with the account kinds computed *before* the transaction; a no-op early-`return Ok(())` success proves nothing; look-alike clones of program-owned accounts are not attacker-constructible;
PDAs must not be granted a signer flag; the native syscall stub panics on CPI privilege escalation / executable-balance changes where the chain returns an `InstructionError`; Trident's SVM has no stake-config account.
