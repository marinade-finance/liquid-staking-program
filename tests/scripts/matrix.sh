#!/usr/bin/env bash
# Cluster-compatibility matrix: program binaries x runtime targets.
#
#   matrix.sh [--so [NAME=]<path|profile>]... [--profiles DIR] [--snapshot] [--try F[,F...]]
#             [--try-master] [--tests FILTER] [--fuzz-cases N] [--out DIR] [--dry-run]
#
# Exit codes: 0 all pass, 1 findings (a target fails where the baseline passes),
#             2 baseline (mainnet) broken, 3 usage / environment error.
# See tests/README.md and tests/ARCHITECTURE.md ("Profile spec strings", "Matrix and weekly job").
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TESTS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$TESTS_DIR/.." && pwd)"

SOS=()
PROFILES="$TESTS_DIR/profiles"
SNAPSHOT=0
TRY=""
TRY_MASTER=0
TESTS_FILTER=""
FUZZ_CASES=64
OUT=""
DRY_RUN=0

usage() { sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
die() { echo "matrix.sh: $*" >&2; exit 3; }

while [ $# -gt 0 ]; do
  case "$1" in
    --so) [ $# -ge 2 ] || die "--so needs a value"; SOS+=("$2"); shift 2 ;;
    --profiles) [ $# -ge 2 ] || die "--profiles needs a value"; PROFILES="$2"; shift 2 ;;
    --snapshot) SNAPSHOT=1; shift ;;
    --try) [ $# -ge 2 ] || die "--try needs a value"; TRY="${TRY:+$TRY,}$2"; shift 2 ;;
    --try-master) TRY_MASTER=1; shift ;;
    --tests) [ $# -ge 2 ] || die "--tests needs a value"; TESTS_FILTER="$2"; shift 2 ;;
    --fuzz-cases) [ $# -ge 2 ] || die "--fuzz-cases needs a value"; FUZZ_CASES="$2"; shift 2 ;;
    --out) [ $# -ge 2 ] || die "--out needs a value"; OUT="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option $1" ;;
  esac
done

command -v jq >/dev/null || die "jq is required"
[ -n "$OUT" ] || OUT="$TESTS_DIR/matrix-out"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
PROFILES="$(cd "$PROFILES" && pwd)"

# ---- program binaries -------------------------------------------------------
if [ ${#SOS[@]} -eq 0 ]; then
  SOS=("head=$REPO_ROOT/target/deploy/marinade_finance.so")
fi
SO_LABELS=()
SO_VALUES=()
for s in "${SOS[@]}"; do
  if [[ "$s" == *=* ]]; then
    label="${s%%=*}"; val="${s#*=}"
  elif [ "$s" = "profile" ]; then
    label="profile"; val="profile"
  else
    label="$(basename "$s" .so)"; val="$s"
  fi
  if [ "$val" != "profile" ]; then
    [ "$DRY_RUN" = 1 ] || [ -f "$val" ] || die "program binary not found: $val"
    [ -f "$val" ] && val="$(cd "$(dirname "$val")" && pwd)/$(basename "$val")"
  fi
  SO_LABELS+=("$label")
  SO_VALUES+=("$val")
done

# ---- cluster-profile tool ---------------------------------------------------
CP="${CLUSTER_PROFILE:-$TESTS_DIR/target-profile/release/cluster-profile}"
if [ ! -x "$CP" ]; then
  echo "[matrix] building cluster-profile" >&2
  (cd "$TESTS_DIR/cluster-profile" && CARGO_TARGET_DIR="$TESTS_DIR/target-profile" cargo build --release --locked >&2) \
    || die "cannot build cluster-profile"
  CP="$TESTS_DIR/target-profile/release/cluster-profile"
fi

# ---- profiles ---------------------------------------------------------------
if [ "$SNAPSHOT" = 1 ]; then
  PROFILES="$OUT/profiles"
  mkdir -p "$PROFILES"
  for c in mainnet testnet devnet; do
    echo "[matrix] snapshot $c" >&2
    "$CP" snapshot --cluster "$c" --out "$PROFILES" >&2 || die "snapshot of $c failed"
  done
fi
for c in mainnet testnet devnet; do
  [ -f "$PROFILES/$c.json" ] || die "missing profile $PROFILES/$c.json"
done
MAINNET="$PROFILES/mainnet.json"
OTHERS=("$PROFILES/testnet.json" "$PROFILES/devnet.json")

# ---- targets ----------------------------------------------------------------
# targets.tsv: label <TAB> spec <TAB> kind <TAB> delta
TARGETS="$OUT/targets.tsv"
: > "$TARGETS"
add_target() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >> "$TARGETS"; }

add_target "mainnet" "$MAINNET" baseline "none (baseline)"

mkdir -p "$OUT/diff"
for p in testnet devnet; do
  "$CP" diff "$MAINNET" "$PROFILES/$p.json" --json "$OUT/diff/$p.json" > "$OUT/diff/$p.md"
  delta="$(jq -r --arg p "$p" '
    [ (.upcoming_for_base[]? | select((.status[$p] // "") | test("^(active|pending)")) | "+\(.module) (\(.id))"),
      (.active_on_base_not_other[]? | "-\(.module) (\(.id))"),
      (.program_diffs[]? | select(.state[$p] != .state.mainnet) | "program \(.name) \(.id): \(.state.mainnet // "?") -> \(.state[$p] // "?")"),
      (.sysvar_diffs[]? | select(contains($p + " =")) | .[0:160])
    ] | join("; ")' "$OUT/diff/$p.json")"
  add_target "$p" "$PROFILES/$p.json" profile "${delta:-no differences}"
done

"$CP" diff "$MAINNET" "${OTHERS[@]}" --json "$OUT/diff/all.json" > "$OUT/diff/all.md"
"$CP" list-upcoming "$MAINNET" "${OTHERS[@]}" > "$OUT/upcoming.txt"
SEEN=" "
while read -r id module; do
  [ -n "$id" ] || continue
  SEEN+="$id "
  add_target "mainnet,+${module:-$id}" "$MAINNET,+$id" feature "activate ${module:-$id} ($id)"
done < "$OUT/upcoming.txt"

if [ -n "$TRY" ]; then
  IFS=',' read -r -a try_list <<< "$TRY"
  for f in "${try_list[@]}"; do
    [ -n "$f" ] || continue
    [[ "$SEEN" == *" $f "* ]] && continue
    SEEN+="$f "
    add_target "mainnet,+$f" "$MAINNET,+$f" feature "activate $f (manual --try)"
  done
fi
if [ "$TRY_MASTER" = 1 ]; then
  while IFS=' ' read -r id module; do
    [ -n "$id" ] || continue
    [[ "$SEEN" == *" $id "* ]] && continue
    SEEN+="$id "
    add_target "mainnet,+${module:-$id}" "$MAINNET,+$id" feature "activate ${module:-$id} ($id) (only on agave master, inactive everywhere)"
  done < <(jq -r '.master_only_inactive[]? | "\(.id) \(.module)"' "$OUT/diff/all.json")
fi

N_TARGETS="$(wc -l < "$TARGETS")"
echo "[matrix] ${#SO_LABELS[@]} binaries x $N_TARGETS targets" >&2
if [ "$DRY_RUN" = 1 ]; then
  echo "binaries:"; for i in "${!SO_LABELS[@]}"; do echo "  ${SO_LABELS[$i]} = ${SO_VALUES[$i]}"; done
  echo "targets (HARNESS_BASE=$MAINNET):"
  while IFS=$'\t' read -r label spec kind delta; do
    echo "  [$kind] $label  HARNESS_TARGET=$spec"; echo "      delta: $delta"
  done < "$TARGETS"
  exit 0
fi

# ---- build once ---------------------------------------------------------------
echo "[matrix] cargo test --no-run (once)" >&2
TEST_ARGS=()
if [ -n "$TESTS_FILTER" ]; then
  # a file under harness/tests/ selects that test target, anything else is a test-name filter
  if [ -f "$TESTS_DIR/harness/tests/$TESTS_FILTER.rs" ]; then TEST_ARGS=(--test "$TESTS_FILTER"); else TEST_ARGS=("$TESTS_FILTER"); fi
fi
DESCRIBE="$TESTS_DIR/target/debug/describe-runtime"
(cd "$TESTS_DIR" && cargo test --no-run "${TEST_ARGS[@]}" && cargo build --bin describe-runtime) > "$OUT/build.log" 2>&1 || {
  tail -n 40 "$OUT/build.log" >&2
  die "harness build failed, see $OUT/build.log"
}

# ---- run --------------------------------------------------------------------
safe() { printf '%s' "$1" | tr -c 'A-Za-z0-9._+-' '_'; }
CELLS="$OUT/cells"
rm -rf "$CELLS"; mkdir -p "$CELLS"

for i in "${!SO_LABELS[@]}"; do
  so_label="${SO_LABELS[$i]}"; so_val="${SO_VALUES[$i]}"
  while IFS=$'\t' read -r label spec kind delta; do
    dir="$OUT/runs/$(safe "$so_label")/$(safe "$label")"
    mkdir -p "$dir"
    rm -f "$dir/report.jsonl"
    echo "[matrix] $so_label x $label" >&2
    rc=0
    # captured output: libtest's own "test X ... ok|FAILED" lines are the source of truth
    (cd "$TESTS_DIR" && \
      MARINADE_SO="$so_val" HARNESS_BASE="$MAINNET" HARNESS_TARGET="$spec" \
      HARNESS_REPORT="$dir/report.jsonl" PROPTEST_CASES="$FUZZ_CASES" \
      cargo test "${TEST_ARGS[@]}") > "$dir/test.log" 2>&1 || rc=$?
    python3 -I "$SCRIPT_DIR/parse_libtest.py" "$dir/test.log" > "$dir/results.jsonl"
    "$DESCRIBE" "$spec" 2>/dev/null > "$dir/runtime.json" || true
    unmodeled="$(jq -c '.unmodeled // []' "$dir/runtime.json" 2>/dev/null || echo '[]')"
    [ -n "$unmodeled" ] || unmodeled='[]'
    first_log_err="$(grep -m1 -E '^error(\[|:)|could not compile' "$dir/test.log" || true)"
    jq -s \
      --arg so "$so_label" --arg target "$label" --arg kind "$kind" --arg spec "$spec" \
      --arg delta "$delta" --argjson rc "$rc" --argjson unmodeled "$unmodeled" \
      --arg logerr "$first_log_err" --arg dir "${dir#"$OUT"/}" '
      . as $r
      | ([ $r[] | select(.status == "FAILED") ]) as $f
      | ([ $r[] | select(.status != "ignored") ] | length) as $ran
      | {so: $so, target: $target, kind: $kind, spec: $spec, delta: $delta, cargo_exit: $rc,
         total: $ran, passed: ([ $r[] | select(.status == "ok") ] | length),
         failed: ( if $ran == 0 then ["<no test ran>"]
                   elif ($f | length) == 0 and $rc != 0 then ["<cargo test failed>"]
                   else [ $f[] | "\(.binary | sub("^tests/"; "") | sub("\\.rs$"; ""))::\(.test)" ] end ),
         first_error: ( if ($f | length) > 0 then ($f[0].error // "(no message)")
                        elif $ran == 0 then "no test matched or the harness did not run"
                        elif $rc != 0 then (if $logerr == "" then "cargo test failed" else $logerr end) else "" end ),
         unmodeled: $unmodeled, dir: $dir}' "$dir/results.jsonl" > "$CELLS/$(safe "$so_label")__$(safe "$label").json"
  done < "$TARGETS"
done

# ---- classify and summarise -------------------------------------------------
jq -s '
  group_by(.so)
  | map( . as $g
      | ($g | map(select(.kind == "baseline")) | .[0]) as $b
      | ($b.failed // []) as $bf
      | $g | map(
          . as $c
          | ($c.failed - $bf) as $new
          | $c + {new_failures: $new, new_unmodeled: ($c.unmodeled - ($b.unmodeled // [])),
                   status: ( if $c.kind == "baseline" then (if ($c.failed | length) > 0 then "broken" else "pass" end)
                             elif ($new | length) > 0 then "finding"
                             elif ($c.failed | length) > 0 then "fail (as baseline)"
                             elif (($c.unmodeled - ($b.unmodeled // [])) | length) > 0 then "unmodeled"
                             else "pass" end) } ) )
  | flatten
  | {generated: (now | todate),
     broken: [ .[] | select(.status == "broken") | .so ] | unique,
     findings: [ .[] | select(.status == "finding") | {so, target, delta, new_failures, first_error} ],
     baseline_unmodeled: ([ .[] | select(.kind == "baseline") | .unmodeled[] ] | unique),
     not_testable: [ .[] | select(.kind != "baseline" and (.new_unmodeled | length > 0)) | {so, target, unmodeled: .new_unmodeled} ],
     cells: . }' "$CELLS"/*.json > "$OUT/summary.json"

jq -r '
  def cell: if .status == "pass" then "pass (\(.passed)/\(.total))"
            elif .status == "unmodeled" then "unmodeled (\(.passed)/\(.total))"
            else "**\(.status | ascii_upcase)** (\(.failed | length) failed): `\(.first_error | gsub("\\|"; "/") | .[0:140])`" end;
  (.cells | map(.target) | unique) as $_ |
  "# Cluster compatibility matrix\n",
  ( if (.broken | length) > 0 then "**MAINNET BROKEN** for: \(.broken | join(", ")). The baseline (mainnet profile) fails, so the program is already broken on the live runtime.\n"
    elif (.findings | length) > 0 then "**\(.findings | length) finding(s):** a target fails where the mainnet baseline passes.\n"
    else "No findings: every target passes wherever the mainnet baseline passes.\n" end ),
  "| binary | target | result |", "|---|---|---|",
  ( .cells[] | "| \(.so) | `\(.target)` | \(cell) |" ),
  "",
  ( if (.findings | length) > 0 then
      "## Findings\n",
      ( .findings[] | "* **\(.so)** on `\(.target)`: \(.new_failures | join(", "))\n  * cause (target delta): \(.delta)\n  * first error: `\(.first_error)`" ),
      ""
    else empty end ),
  ( if (.baseline_unmodeled | length) > 0 then
      "## Unmodeled already on the mainnet baseline\n",
      ( .baseline_unmodeled[] | "* \(.)" ),
      ""
    else empty end ),
  ( if (.not_testable | length) > 0 then
      "## Not testable in-process (unmodeled by the harness, beyond the baseline)\n",
      ( .not_testable | group_by(.target) | .[] | "* `\(.[0].target)`: \(.[0].unmodeled | join("; "))" ),
      ""
    else empty end ),
  "Logs and reports: `runs/<binary>/<target>/{test.log,results.jsonl,runtime.json,report.jsonl}`; target deltas: `targets.tsv`, `diff/`."
' "$OUT/summary.json" > "$OUT/summary.md"

cat "$OUT/summary.md"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  { cat "$OUT/summary.md"; echo; } >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$(jq '.broken | length' "$OUT/summary.json")" -gt 0 ]; then exit 2; fi
if [ "$(jq '.findings | length' "$OUT/summary.json")" -gt 0 ]; then exit 1; fi
exit 0
