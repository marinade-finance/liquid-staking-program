#!/usr/bin/env bash
# Backtest program builds against runtime profiles.
#
#   backtest.sh [--base SPEC] [--target SPEC]... [--revert <commit>] [--verifiable]
#               [--tests FILTER] [--fuzz-cases N] [--out DIR] [--build-only] <commit|path.so>...
#
# Each positional argument is a git commit (built in a cached git worktree with the toolchain its
# Cargo.lock needs) or a prebuilt .so. --revert <c> adds "HEAD with commit c reverted".
# Default runtime = the SIMD-0437 rent regression: base = mainnet without the two rent steps,
# target = mainnet. 07ffbf7 must fail, d4e2023 and HEAD must pass.
#
# Anchor 0.27 commits (the deployed releases) build in docker with the deployment image of
# Docs/DEPLOYMENT.md; anchor 0.31 commits with Solana 2.3 + platform-tools v1.52.
# Environment: SOLANA_LEGACY_BIN (Solana 1.18.x, only with BACKTEST_LEGACY_TOOLCHAIN=1),
#   SOLANA_BIN (Solana 2.3.x bin dir, for anchor 0.31 commits), BACKTEST_CACHE (default
#   ~/.cache/marinade-backtest), SOLANA_VERIFY (solana-verify binary for --verifiable),
#   BACKTEST_KEEP_WORKTREES=1 (keep worktrees for incremental rebuilds).
# Missing toolchains are downloaded from release.anza.xyz into $BACKTEST_CACHE/solana.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TESTS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$TESTS_DIR/.." && pwd)"

LEGACY_VERSION="1.18.26"
MODERN_VERSION="2.3.13"
MODERN_TOOLS="v1.52"
VERIFY_IMAGE="projectserum/build@sha256:0e11aced57c448c7da9bf0a563a146398ac8b88f357b8fc2e1be314b42320686"

CACHE="${BACKTEST_CACHE:-$HOME/.cache/marinade-backtest}"
BASE_SPEC=""
TARGETS=()
REVERT=""
VERIFIABLE=0
TESTS_FILTER=""
FUZZ_CASES=64
OUT=""
BUILD_ONLY=0
ITEMS=()

usage() { sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
die() { echo "backtest.sh: $*" >&2; exit 3; }

while [ $# -gt 0 ]; do
  case "$1" in
    --base) [ $# -ge 2 ] || die "--base needs a value"; BASE_SPEC="$2"; shift 2 ;;
    --target) [ $# -ge 2 ] || die "--target needs a value"; TARGETS+=("$2"); shift 2 ;;
    --revert) [ $# -ge 2 ] || die "--revert needs a value"; REVERT="$2"; shift 2 ;;
    --verifiable) VERIFIABLE=1; shift ;;
    --tests) [ $# -ge 2 ] || die "--tests needs a value"; TESTS_FILTER="$2"; shift 2 ;;
    --fuzz-cases) [ $# -ge 2 ] || die "--fuzz-cases needs a value"; FUZZ_CASES="$2"; shift 2 ;;
    --out) [ $# -ge 2 ] || die "--out needs a value"; OUT="$2"; shift 2 ;;
    --build-only) BUILD_ONLY=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) usage >&2; die "unknown option $1" ;;
    *) ITEMS+=("$1"); shift ;;
  esac
done
[ ${#ITEMS[@]} -gt 0 ] || [ -n "$REVERT" ] || { usage >&2; die "nothing to test"; }

MAINNET="$TESTS_DIR/profiles/mainnet.json"
if [ -z "$BASE_SPEC" ] && [ ${#TARGETS[@]} -eq 0 ]; then
  BASE_SPEC="$MAINNET,-set_lamports_per_byte_to_6333,-set_lamports_per_byte_to_5080"
  TARGETS=("$MAINNET")
fi
[ -n "$BASE_SPEC" ] || BASE_SPEC="$MAINNET"
[ ${#TARGETS[@]} -gt 0 ] || TARGETS=("$BASE_SPEC")
[ -n "$OUT" ] || OUT="$TESTS_DIR/backtest-out"
mkdir -p "$OUT" "$CACHE/bins" "$CACHE/worktrees"
OUT="$(cd "$OUT" && pwd)"

# ---- toolchains -------------------------------------------------------------
ensure_toolchain() { # version -> prints bin dir
  local ver="$1" dir="$CACHE/solana/$1/solana-release/bin"
  if [ ! -x "$dir/cargo-build-sbf" ]; then
    echo "[backtest] downloading Solana $ver" >&2
    mkdir -p "$CACHE/solana/$ver"
    curl -fsSL "https://release.anza.xyz/v$ver/solana-release-x86_64-unknown-linux-gnu.tar.bz2" \
      | tar -xj -C "$CACHE/solana/$ver" >&2 || die "download of Solana $ver failed"
  fi
  echo "$dir"
}
legacy_bin() {
  if [ -n "${SOLANA_LEGACY_BIN:-}" ]; then echo "$SOLANA_LEGACY_BIN"; else ensure_toolchain "$LEGACY_VERSION"; fi
}
modern_bin() {
  if [ -n "${SOLANA_BIN:-}" ]; then echo "$SOLANA_BIN"; else ensure_toolchain "$MODERN_VERSION"; fi
}

anchor_version() { # worktree dir
  awk '/^name = "anchor-lang"$/ { getline; gsub(/[^0-9.]/, "", $0); print; exit }' "$1/Cargo.lock"
}

# build_commit <worktree> <out .so>
build_worktree() {
  local wt="$1" out="$2" av bin
  av="$(anchor_version "$wt")"
  [ -n "$av" ] || die "no anchor-lang in $wt/Cargo.lock"
  if [ "$VERIFIABLE" = 1 ]; then
    local sv="${SOLANA_VERIFY:-solana-verify}"
    command -v "$sv" >/dev/null || die "--verifiable needs solana-verify (see Docs/DEPLOYMENT.md)"
    echo "[backtest] solana-verify build (docker, anchor $av)" >&2
    (cd "$wt" && "$sv" build --bpf --base-image "$VERIFY_IMAGE" --library-name marinade_finance "$wt") >&2
  else
    case "$av" in
      0.27.*)
        if [ "${BACKTEST_LEGACY_TOOLCHAIN:-0}" = 1 ]; then
          # Solana 1.18 / platform-tools v1.41 builds of these commits crash in add_validator
          # (access violation at 0xffff...); use only to compare toolchains.
          bin="$(legacy_bin)"; echo "[backtest] anchor $av -> Solana legacy ($bin) [BACKTEST_LEGACY_TOOLCHAIN]" >&2
          (cd "$wt" && PATH="$bin:$PATH" nice -n 19 cargo-build-sbf --manifest-path programs/marinade-finance/Cargo.toml \
            --sbf-out-dir target/deploy) >&2
        else
          # the image these commits were deployed with (Docs/DEPLOYMENT.md): Solana 1.14.16, sbf-tools v1.29
          docker info >/dev/null 2>&1 || die "anchor $av commits build in docker ($VERIFY_IMAGE); start a docker daemon"
          echo "[backtest] anchor $av -> docker $VERIFY_IMAGE" >&2
          local ca=() ; [ -f "${SSL_CERT_FILE:-/nonexistent}" ] && ca=(-v "$SSL_CERT_FILE:/ca.crt:ro" -e CARGO_HTTP_CAINFO=/ca.crt -e SSL_CERT_FILE=/ca.crt)
          docker run --rm --network host -e HTTPS_PROXY -e HTTP_PROXY -e https_proxy -e http_proxy -e NO_PROXY "${ca[@]}" \
            -v "$wt:/build" -w /build "$VERIFY_IMAGE" cargo build-sbf -- --locked --frozen >&2
        fi ;;
      0.31.*)
        bin="$(modern_bin)"; echo "[backtest] anchor $av -> Solana modern ($bin)" >&2
        (cd "$wt" && PATH="$bin:$PATH" nice -n 19 cargo-build-sbf --tools-version "$MODERN_TOOLS" \
          --manifest-path programs/marinade-finance/Cargo.toml --sbf-out-dir target/deploy) >&2 ;;
      *) die "unsupported anchor-lang $av in $wt/Cargo.lock" ;;
    esac
  fi
  [ -f "$wt/target/deploy/marinade_finance.so" ] || die "build produced no marinade_finance.so"
  cp "$wt/target/deploy/marinade_finance.so" "$out"
}

# resolve <item> -> prints "<label>\t<so path>" (builds if needed)
resolve() {
  local item="$1" sha label so wt mode
  if [ -f "$item" ]; then
    label="$(basename "$item" .so)"
    printf '%s\t%s\n' "$label" "$(cd "$(dirname "$item")" && pwd)/$(basename "$item")"; return
  fi
  sha="$(git -C "$REPO_ROOT" rev-parse --verify --quiet "$item^{commit}")" || die "not a commit or file: $item"
  label="$(git -C "$REPO_ROOT" rev-parse --short=7 "$sha")"
  mode=""; [ "$VERIFIABLE" = 1 ] && mode="-verifiable"
  so="$CACHE/bins/$sha$mode.so"
  if [ ! -f "$so" ]; then
    wt="$CACHE/worktrees/$sha$mode"
    git -C "$REPO_ROOT" worktree prune
    [ -d "$wt" ] || git -C "$REPO_ROOT" worktree add --detach "$wt" "$sha" >&2
    build_worktree "$wt" "$so"
    [ "${BACKTEST_KEEP_WORKTREES:-0}" = 1 ] || git -C "$REPO_ROOT" worktree remove --force "$wt" >&2
  else
    echo "[backtest] cached build for $label" >&2
  fi
  printf '%s\t%s\n' "$label" "$so"
}

resolve_revert() { # commit to revert on top of HEAD
  local c="$1" csha head so wt mode
  csha="$(git -C "$REPO_ROOT" rev-parse --verify --quiet "$c^{commit}")" || die "not a commit: $c"
  head="$(git -C "$REPO_ROOT" rev-parse HEAD)"
  mode=""; [ "$VERIFIABLE" = 1 ] && mode="-verifiable"
  so="$CACHE/bins/revert-$csha-on-$head$mode.so"
  if [ ! -f "$so" ]; then
    wt="$CACHE/worktrees/revert-$csha-on-$head$mode"
    git -C "$REPO_ROOT" worktree prune
    [ -d "$wt" ] || git -C "$REPO_ROOT" worktree add --detach "$wt" "$head" >&2
    git -C "$wt" revert --no-commit "$csha" >&2 || die "revert of $c does not apply cleanly on HEAD"
    build_worktree "$wt" "$so"
    [ "${BACKTEST_KEEP_WORKTREES:-0}" = 1 ] || git -C "$REPO_ROOT" worktree remove --force "$wt" >&2
  fi
  printf 'HEAD-revert-%s\t%s\n' "$(git -C "$REPO_ROOT" rev-parse --short=7 "$csha")" "$so"
}

LABELS=(); SOFILES=()
for it in "${ITEMS[@]}"; do
  IFS=$'\t' read -r l f < <(resolve "$it"); LABELS+=("$l"); SOFILES+=("$f")
done
if [ -n "$REVERT" ]; then
  IFS=$'\t' read -r l f < <(resolve_revert "$REVERT"); LABELS+=("$l"); SOFILES+=("$f")
fi
for i in "${!LABELS[@]}"; do
  echo "[backtest] ${LABELS[$i]}  sha256=$(sha256sum "${SOFILES[$i]}" | cut -d' ' -f1)  ${SOFILES[$i]}" >&2
done
[ "$BUILD_ONLY" = 0 ] || exit 0

# ---- run harness -------------------------------------------------------------
command -v jq >/dev/null || die "jq is required"
echo "[backtest] cargo test --no-run (once)" >&2
TEST_ARGS=()
if [ -n "$TESTS_FILTER" ]; then
  # a file under harness/tests/ selects that test target, anything else is a test-name filter
  if [ -f "$TESTS_DIR/harness/tests/$TESTS_FILTER.rs" ]; then TEST_ARGS=(--test "$TESTS_FILTER"); else TEST_ARGS=("$TESTS_FILTER"); fi
fi
(cd "$TESTS_DIR" && cargo test --no-run "${TEST_ARGS[@]}") > "$OUT/build.log" 2>&1 || { tail -n 40 "$OUT/build.log" >&2; die "harness build failed"; }

CELLS="$OUT/cells"; rm -rf "$CELLS"; mkdir -p "$CELLS"
safe() { printf '%s' "$1" | tr -c 'A-Za-z0-9._+-' '_'; }
for i in "${!LABELS[@]}"; do
  for ti in "${!TARGETS[@]}"; do
    dir="$OUT/runs/$(safe "${LABELS[$i]}")/target$ti"; mkdir -p "$dir"; rm -f "$dir/report.jsonl"
    echo "[backtest] ${LABELS[$i]} x ${TARGETS[$ti]}" >&2
    rc=0
    # captured output: libtest's own "test X ... ok|FAILED" lines are the source of truth
    (cd "$TESTS_DIR" && MARINADE_SO="${SOFILES[$i]}" HARNESS_BASE="$BASE_SPEC" HARNESS_TARGET="${TARGETS[$ti]}" \
      HARNESS_REPORT="$dir/report.jsonl" PROPTEST_CASES="$FUZZ_CASES" \
      cargo test "${TEST_ARGS[@]}") > "$dir/test.log" 2>&1 || rc=$?
    python3 -I "$SCRIPT_DIR/parse_libtest.py" "$dir/test.log" > "$dir/results.jsonl"
    logerr="$(grep -m1 -E '^error(\[|:)|could not compile' "$dir/test.log" || true)"
    jq -s --arg so "${LABELS[$i]}" --argjson ti "$ti" --argjson rc "$rc" --arg logerr "$logerr" '
      ([ .[] | select(.status == "FAILED") ]) as $f
      | ([ .[] | select(.status != "ignored") ] | length) as $ran
      | {so: $so, ti: $ti, total: $ran, passed: ([ .[] | select(.status == "ok") ] | length),
         failed: (if $ran == 0 then ["<no test ran>"]
                  elif ($f | length) == 0 and $rc != 0 then ["<cargo test failed>"]
                  else [ $f[] | "\(.binary | sub("^tests/"; "") | sub("\\.rs$"; ""))::\(.test)" ] end),
         first_error: (if ($f | length) > 0 then ($f[0].error // "(no message)")
                       elif $ran == 0 then "no test matched or the harness did not run"
                       elif $rc != 0 then (if $logerr == "" then "cargo test failed" else $logerr end) else "" end)}' \
      "$dir/results.jsonl" > "$CELLS/$(safe "${LABELS[$i]}")__$ti.json"
  done
done

{
  echo "# Backtest"
  echo
  echo "base: \`$BASE_SPEC\`"
  echo
  for ti in "${!TARGETS[@]}"; do echo "* target $ti: \`${TARGETS[$ti]}\`"; done
  echo
  printf '| program |'; for ti in "${!TARGETS[@]}"; do printf ' target %s |' "$ti"; done; echo
  printf '|---|'; for _ in "${TARGETS[@]}"; do printf -- '---|'; done; echo
  for i in "${!LABELS[@]}"; do
    printf '| %s |' "${LABELS[$i]}"
    for ti in "${!TARGETS[@]}"; do
      jq -r '(if (.failed | length) == 0 then "pass (\(.passed)/\(.total))"
              else "**FAIL** (\(.failed | length)): \(.failed[0:4] | join(", ")): `\(.first_error | gsub("\\|"; "/") | .[0:160])`" end) + " |"' \
        "$CELLS/$(safe "${LABELS[$i]}")__$ti.json" | tr '\n' ' ' | sed 's/^/ /'
    done
    echo
  done
} > "$OUT/summary.md"
cat "$OUT/summary.md"
[ -z "${GITHUB_STEP_SUMMARY:-}" ] || { cat "$OUT/summary.md"; echo; } >> "$GITHUB_STEP_SUMMARY"
