#!/usr/bin/env bash
# Wrapper around the Trident CLI / the fuzz binaries. See README.md.
#   ./run.sh build
#   ./run.sh ix [ix1,ix2,...]      per-instruction adversarial fuzzing (all instructions if no list)
#   ./run.sh flows                 long mostly-valid sequences
#   ./run.sh all                   ix + flows
#   ./run.sh sbf <target>          same, but executing the compiled SBF binary (MARINADE_SBF)
#   ./run.sh coverage [iters]      line coverage of programs/marinade-finance/src (native executor)
#   ./run.sh replay <target> <seed>   replay one iteration (trident fuzz debug)
# Exit code 1 when a run wrote a report for a signature not in trident-tests/known-findings.txt.
# Knobs: FUZZ_ITERS, FUZZ_FLOW_ITERS (all), FUZZ_CALLS, FUZZ_ONLY, FUZZ_OUT, FUZZ_STRICT=1, MARINADE_SBF.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here/trident-tests"
export FUZZ_OUT="${FUZZ_OUT:-$here/trident-tests/fuzz-out}"
ITERS="${FUZZ_ITERS:-20000}"
cmd="${1:-help}"; shift || true

rc=0
run_target() { # target iters calls
  local t="$1"; export FUZZ_ITERS="$2" FUZZ_CALLS="$3"
  mkdir -p "$FUZZ_OUT"; local mark="$FUZZ_OUT/.start-$t"; touch "$mark"
  # the native program prints its msg! logs to stdout (hundreds of MB): keep only the fuzzer's own lines
  { if command -v trident >/dev/null 2>&1; then trident fuzz run "$t"; else cargo run --release --locked --bin "$t"; fi; } \
    | grep --line-buffered -E 'Assertion failed|MASTER SEED|Fuzzing|FINDING|Overall' || true
  # a report under findings/ is written only for a signature not in known-findings.txt
  local new=""; [ -d "$FUZZ_OUT/findings" ] && new="$(find "$FUZZ_OUT/findings" -name '*.txt' -newer "$mark" | sort)"
  if [ -n "$new" ]; then
    echo "NEW FINDINGS ($t): $(echo "$new" | wc -l) report(s) in $FUZZ_OUT/findings" >&2
    echo "$new" | sed 's/^/  /' >&2
    rc=1
  fi
}

case "$cmd" in
  build) cargo build --release ;;
  ix)    [ $# -gt 0 ] && export FUZZ_ONLY="$1"; run_target fuzz_ix "$ITERS" "${FUZZ_CALLS:-60}" ;;
  flows) run_target fuzz_flows "${FUZZ_ITERS:-5000}" "${FUZZ_CALLS:-150}" ;;
  all)   run_target fuzz_ix "$ITERS" 60; run_target fuzz_flows "${FUZZ_FLOW_ITERS:-5000}" 150 ;;
  sbf)   so="${MARINADE_SBF:-$here/../../target/deploy/marinade_finance.so}"
         [ -f "$so" ] || { echo "build the program first: cargo build-sbf --tools-version v1.52 --manifest-path programs/marinade-finance/Cargo.toml --sbf-out-dir target/deploy" >&2; exit 1; }
         export MARINADE_SBF="$so"; run_target "${1:-fuzz_ix}" "$ITERS" 60 ;;
  replay) # the seed is printed with every finding: "(seed: <hex>)"
         if command -v trident >/dev/null 2>&1; then trident fuzz debug "$1" "$2"; else TRIDENT_FUZZ_DEBUG="$2" TRIDENT_FUZZ_DEBUG_PATH="$FUZZ_OUT/replay.log" cargo run --release --bin "$1"; fi ;;
  coverage)
         iters="${1:-3000}"
         tgt="$here/trident-tests/target-cov"; prof="$tgt/prof"; rm -rf "$prof"; mkdir -p "$prof"
         llvm="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin"
         # build scripts / proc macros are instrumented too: keep their profraw files out of the source tree
         LLVM_PROFILE_FILE="$prof/build-%p-%m.profraw" RUSTFLAGS="-C instrument-coverage" CARGO_TARGET_DIR="$tgt" cargo build --release --bins
         for b in fuzz_ix fuzz_flows; do
           LLVM_PROFILE_FILE="$prof/$b-%p-%m.profraw" FUZZ_ITERS="$iters" FUZZ_CALLS=60 "$tgt/release/$b" >/dev/null 2>&1 || true
         done
         "$llvm/llvm-profdata" merge -sparse "$prof"/*.profraw -o "$prof/all.profdata"
         ign='(\.cargo|\.rustup|trident-tests|/rustc/)'
         objs=(-object "$tgt/release/fuzz_ix" -object "$tgt/release/fuzz_flows")
         "$llvm/llvm-cov" report "${objs[@]}" -instr-profile="$prof/all.profdata" -ignore-filename-regex="$ign" | tee "$here/trident-tests/coverage-summary.txt"
         "$llvm/llvm-cov" show   "${objs[@]}" -instr-profile="$prof/all.profdata" -ignore-filename-regex="$ign" -format=html -output-dir="$here/trident-tests/coverage-html" >/dev/null
         "$llvm/llvm-cov" export "${objs[@]}" -instr-profile="$prof/all.profdata" -ignore-filename-regex="$ign" -format=lcov > "$here/trident-tests/coverage.lcov"
         echo "html: trident-tests/coverage-html/index.html  lcov: trident-tests/coverage.lcov" ;;
  *) sed -n 2,12p "$0" ;;
esac
exit "$rc"
