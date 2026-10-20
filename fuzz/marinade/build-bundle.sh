#!/usr/bin/env bash
# Build the Marinade crucible fuzz harness and assemble a FuzzCorp bundle.
#
# Output: fuzz/marinade/build/bundle/ — ready for `fuzz-up upload bundle`.
#
# Inputs it expects to be present (CI generates them; see .github/workflows/fuzzcorp.yml):
#   - programs/marinade_program.so  — the target program, built from this repo's program source
#     with the pinned anchor 0.27 / solana 1.14.29 toolchain (`anchor build`). Loaded at runtime.
#   - idls/marinade.json            — the program IDL from the same build. Read at compile time by
#     declare_fuzz_program!.
# Fixtures ship compressed (one archive instead of ~100 files) and are unpacked here — they are
# embedded at compile time via include_bytes!.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

if [ ! -d fixtures/mainnet ]; then
  echo "==> unpacking fixtures"
  mkdir -p fixtures
  tar -xzf fixtures-mainnet.tar.gz -C fixtures
fi

for f in programs/marinade_program.so idls/marinade.json; do
  if [ ! -f "$f" ]; then
    echo "error: $f is missing — build the program first (anchor build; see the workflow)." >&2
    exit 1
  fi
done

echo "==> building harness (release, --features invariant_test)"
cargo build --release --locked --features invariant_test

bundle="$here/build/bundle"
rm -rf "$bundle"
mkdir -p "$bundle/bin" "$bundle/run/programs"

cp "$here/target/release/invariant_test" "$bundle/bin/invariant_test"
cp "$here/programs/marinade_program.so"  "$bundle/run/programs/marinade_program.so"

# ------------------------------------------------------------------ coverage ---
# Source-level coverage needs two things the execution .so cannot provide, and it
# fails SILENTLY when either is missing (lines_found: 0, nothing rendered):
#
#   1. An UNSTRIPPED binary. `cargo build-bpf` strips target/deploy/*.so, so the
#      DWARF lives only in the pre-strip artifact. Under the solana 1.14.29 line
#      that is target/bpfel-unknown-unknown/release/ -- not the sbf-solana-solana/
#      path used by 1.16+ -- so search rather than hardcoding either.
#   2. The sources, staged so that stripping SourcesOriginalPath off each absolute
#      SF: path in the LCOV lands inside SourcesPathInBundle. The program is built
#      in a container mounted at /work, so DW_AT_comp_dir is /work and the SF paths
#      are /work/programs/marinade-finance/src/... We mirror the repo layout under
#      srcs/ and strip the shallow "/work/" prefix, which depends only on comp_dir
#      and not on directory depth.
repo="$(git -C "$here" rev-parse --show-toplevel)"

# Pick the first candidate that actually CARRIES DWARF. "not stripped" is not the
# test: `anchor build` leaves the symbol table intact while emitting zero compile
# units unless CARGO_PROFILE_RELEASE_DEBUG=2 is set, and such a file sails through
# every size/`file` check only to yield an empty coverage profile on the server.
#
# Do NOT make this depend on llvm-dwarfdump being installed. It is absent on stock
# macOS and on the ubuntu-latest runner until `apt-get install llvm`, and the old
# version of this function returned -1 in that case, which the caller read as
# "unknown -> accept". Every DWARF verdict this harness ever recorded came from a
# `command not found` masked by 2>/dev/null + `grep -c` (which prints 0 and exits 1,
# swallowed by `|| echo 0`). So the gate reported on a file it had never read.
#
# The ELF section table is enough to answer the question and needs no toolchain, so
# read it directly and only use dwarfdump as a refinement when it genuinely exists.
dwarf_cu_count() {
  local so="$1" dd=""
  command -v llvm-dwarfdump >/dev/null 2>&1 && dd=llvm-dwarfdump
  [ -z "$dd" ] && command -v dwarfdump >/dev/null 2>&1 && dd=dwarfdump
  [ -z "$dd" ] && [ -x /Library/Developer/CommandLineTools/usr/bin/dwarfdump ] \
    && dd=/Library/Developer/CommandLineTools/usr/bin/dwarfdump
  if [ -n "$dd" ]; then
    local n
    n=$("$dd" --debug-info "$so" 2>/dev/null | grep -c 'DW_TAG_compile_unit\|Compile Unit:')
    [ "${n:-0}" -gt 0 ] && { echo "$n"; return; }
  fi
  # Toolchain-free fallback: a .debug_line of non-zero size is what the coverage
  # mapper actually consumes. Report 1 (present) or 0 (absent) -- never "unknown".
  python3 - "$so" <<'PYDW' 2>/dev/null || echo 0
import sys,struct
d=open(sys.argv[1],'rb').read()
if len(d)<0x40 or d[:4]!=b'\x7fELF': print(0); raise SystemExit
sh=struct.unpack_from('<Q',d,0x28)[0]; se=struct.unpack_from('<H',d,0x3a)[0]
sn=struct.unpack_from('<H',d,0x3c)[0]; ss=struct.unpack_from('<H',d,0x3e)[0]
so=struct.unpack_from('<Q',d,sh+ss*se+0x18)[0]
n=0
for i in range(sn):
    o=sh+i*se; nm=struct.unpack_from('<I',d,o)[0]; sz=struct.unpack_from('<Q',d,o+0x20)[0]
    if d[so+nm:d.index(b'\0',so+nm)].decode(errors='replace')=='.debug_line' and sz>0: n=1
print(n)
PYDW
}

# ELF e_machine of the program actually being fuzzed. The symbols MUST come from a
# build of the same architecture: 263 is Solana Bytecode Format (cargo-build-sbf),
# 247 is plain EM_BPF (the deprecated cargo-build-bpf that `anchor build` invokes).
# Both execute, but crucible resolves coverage only for the 263 artifact, and a
# tree that has been built both ways leaves BOTH lying around -- so pick by match,
# not by whichever directory is listed first.
e_machine() { od -An -tu2 -j18 -N2 "$1" 2>/dev/null | tr -d ' '; }
prog_mach="$(e_machine "$here/programs/marinade_program.so")"

# MEASURED, not assumed: crucible's coverage mapper resolves PCs only for the 263
# (Solana Bytecode Format) artifact. A 247 (deprecated EM_BPF) program carries a
# complete, valid DWARF -- 125 line-program units, 46 first-party source files,
# real addresses -- and the mapper still reports
#     [COVERAGE] DWARF source map loaded: 0 PCs resolved, 0 functions
#     [LCOV] Source-level coverage: 0 source files
# The LCOV then contains NO source files, so the cover task fails with
# "SourcesOriginalPath ... does not match any source file in the coverage profile"
# for EVERY prefix -- deep, shallow or leading-slash. That error names the prefix,
# so it reads as a path bug and sends you tuning SourcesOriginalPath for as long as
# you are willing to keep at it. It is an ARCHITECTURE bug. Fail here instead.
#
# `anchor build` on the pinned anchor 0.27 / solana 1.14.29 line invokes the
# deprecated cargo-build-bpf and yields 247. Build the program with
#   cargo build-sbf --tools-version v1.44 --arch v1
# instead (see the workflow). v1.44 is the OLDEST cached platform-tools carrying an
# sbpf target; anything older has no --arch v1, and anything newer runs a rustc that
# rejects ahash 0.7.6's `feature(stdsimd)` (removed in rust 1.78) -- which is why the
# workflow bumps ahash to 0.7.8 at build time.
if [ "$prog_mach" != "263" ]; then
  echo "FATAL: programs/marinade_program.so has e_machine=$prog_mach, expected 263." >&2
  echo "  Source-level coverage would render EMPTY (0 PCs resolved) while every CI" >&2
  echo "  step stays green. Rebuild with: cargo build-sbf --tools-version v1.44 --arch v1" >&2
  exit 1
fi

symbols=""
for cand in \
  "$repo/target/sbpfv1-solana-solana/release/marinade_finance.so" \
  "$repo/target/sbpf-solana-solana/release/marinade_finance.so" \
  "$repo/target/sbf-solana-solana/release/marinade_finance.so" \
  "$repo/target/bpfel-unknown-unknown/release/marinade_finance.so"; do
  [ -f "$cand" ] || continue
  cand_mach="$(e_machine "$cand")"
  if [ -n "$prog_mach" ] && [ "$cand_mach" != "$prog_mach" ]; then
    echo "==> coverage: skipping $(basename "$(dirname "$(dirname "$cand")")") -- e_machine $cand_mach != program $prog_mach" >&2
    continue
  fi
  cus="$(dwarf_cu_count "$cand")"
  if [ "$cus" = "0" ]; then
    echo "==> coverage: skipping $cand -- no DWARF (0 compile units)." >&2
    continue
  fi
  symbols="$cand"
  [ "$cus" != "-1" ] && echo "==> coverage: $(basename "$cand") e_machine=$cand_mach, $cus compile units"
  break
done

if [ -n "$symbols" ]; then
  # srcs mirrors the CONTENTS of programs/, i.e. srcs/marinade-finance/src/... so
  # that stripping the "<comp_dir>/programs/" prefix off an SF: path lands exactly
  # on a staged file. The server REJECTS an empty SourcesOriginalPath outright
  # ("SourcesOriginalPath is required when SourcesPathInBundle is set"), so the
  # prefix always ends in programs/ even when the DWARF carries no comp_dir.
  # srcs holds the CONTENTS of the crate's src/, paired with a prefix that ends at
  # .../src/. The shallower pairing (prefix "programs/" + srcs/marinade-finance/src)
  # is structurally identical to what exponent uses and our own guard accepts it,
  # but the SERVER rejects it here with "does not match any source file" -- so trust
  # the server, not the model, and use the prefix that names the crate explicitly.
  # "Carries DWARF" does not predict whether coverage renders -- the line program's
  # addresses have to point at the code. platform-tools v1.44 emits them shifted left
  # 32 bits, which maps 0 PCs and makes the cover task blame SourcesOriginalPath.
  # Fail here rather than after an upload and a two-hour prefix hunt.
  python3 "$here/verify-dwarf-addrs.py" "$symbols" || exit 1

  # The DWARF line table names first-party files as
  #   programs/marinade-finance/src/<...>.rs        (46 of them, verified by reading
  #                                                  .debug_line directly)
  # so mirror the repo's programs/ directory under srcs/ and strip the SHALLOW
  # "programs/" prefix. This is byte-for-byte the pairing exponent uses, which is the
  # only marinade-shaped configuration observed to render (5836/6265).
  mkdir -p "$bundle/symbols" "$bundle/srcs/marinade-finance"
  cp "$symbols" "$bundle/symbols/marinade_symbols.so"
  cp -R "$repo/programs/marinade-finance/src" "$bundle/srcs/marinade-finance/src"

  # Reading DW_AT_comp_dir alone is NOT enough, and assuming "absent comp_dir
  # means repo-relative" is exactly what broke this: the cover task rejected
  # SourcesOriginalPath "programs/" with "does not match any source file in the
  # coverage profile", so the project rendered lines_found: 0 while every CI step
  # stayed green. These SBF artifacts often carry NO comp_dir at all, and their
  # unit paths are a mix (programs/<crate>/src/..., bare src/lib.rs for deps).
  # Derive the prefix from the actual compile-unit paths, and FAIL rather than
  # guess -- a wrong prefix is invisible until someone opens an empty dashboard.
  # NOTE the suffix: srcs/ mirrors the CONTENTS of programs/ (srcs/marinade-finance/
  # src/...), so the prefix must end at programs/ for the strip to land on a staged
  # file. Deriving a longer prefix would resolve to srcs/lib.rs and find nothing.
  # Fixed, then PROVEN against the staged tree -- not derived. derive-sources-prefix.sh
  # takes a hint argument and echoes it back when it cannot read the DWARF, so a run
  # with no usable dwarfdump "derives" exactly the hint and reports success. That is
  # how a prefix nothing had verified came to be printed as "derived".
  sources_original='programs/'
  # Prove it: every first-party file the line table names must land on a staged file
  # after the prefix is stripped. Zero resolvable files means empty coverage.
  # The XXXXXX is required: GNU mktemp rejects a template without it
  # ("too few X's in template"), while BSD/macOS mktemp accepts the bare name -- so
  # omitting it works locally and fails only on the Linux runner.
  sfcheck="$(mktemp -t sfcheck.XXXXXX)"
  cat > "$sfcheck" <<'PYSF'
import sys,struct,os
so,srcs,pre=sys.argv[1],sys.argv[2],sys.argv[3]
d=open(so,'rb').read()
sh=struct.unpack_from('<Q',d,0x28)[0]; se=struct.unpack_from('<H',d,0x3a)[0]
sn=struct.unpack_from('<H',d,0x3c)[0]; ss=struct.unpack_from('<H',d,0x3e)[0]
stro=struct.unpack_from('<Q',d,sh+ss*se+0x18)[0]
sec={}
for i in range(sn):
    o=sh+i*se; n=struct.unpack_from('<I',d,o)[0]
    off=struct.unpack_from('<Q',d,o+0x18)[0]; sz=struct.unpack_from('<Q',d,o+0x20)[0]
    sec[d[stro+n:d.index(b'\0',stro+n)].decode(errors='replace')]=(off,sz)
if '.debug_line' not in sec: print("0 0"); raise SystemExit
off,sz=sec['.debug_line']; L=d[off:off+sz]
def uleb(b,i):
    r=s=0
    while True:
        x=b[i]; i+=1; r|=(x&0x7f)<<s; s+=7
        if not x&0x80: return r,i
def cstr(b,i):
    j=b.index(b'\0',i); return b[i:j].decode('utf-8','replace'), j+1
pos=0; files=set()
while pos < len(L)-4:
    ul=struct.unpack_from('<I',L,pos)[0]
    if ul==0 or ul==0xffffffff: break
    end=pos+4+ul; p=pos+4
    ver=struct.unpack_from('<H',L,p)[0]; p+=2
    if ver not in (2,3,4): break
    hl=struct.unpack_from('<I',L,p)[0]; p+=4
    p+=1
    if ver>=4: p+=1
    p+=3
    ob=L[p]; p+=1; p+=ob-1
    dirs=['']
    while True:
        s,p=cstr(L,p)
        if not s: break
        dirs.append(s)
    while True:
        nm,p=cstr(L,p)
        if not nm: break
        di,p=uleb(L,p); _,p=uleb(L,p); _,p=uleb(L,p)
        base=dirs[di] if di<len(dirs) else ''
        files.add(f"{base}/{nm}" if base else nm)
    pos=end
cand=[f for f in files if f.startswith(pre) and f.endswith('.rs')]
ok=sum(1 for f in cand if os.path.isfile(os.path.join(srcs,f[len(pre):])))
print(f"{ok} {len(cand)}")
PYSF
  resolved="$(python3 "$sfcheck" "$symbols" "$bundle/srcs" "$sources_original" 2>/dev/null || echo '0 0')"
  rm -f "$sfcheck"
  ok="${resolved%% *}"; total="${resolved##* }"
  if [ "${ok:-0}" -eq 0 ]; then
    echo "==> coverage: ERROR -- SourcesOriginalPath '$sources_original' resolves" >&2
    echo "    $ok of $total first-party DWARF files against $bundle/srcs." >&2
    echo "    The cover task would render EMPTY coverage while CI stays green." >&2
    exit 1
  fi
  echo "==> coverage: SourcesOriginalPath '$sources_original' resolves $ok/$total first-party files"
  coverage_params=$'\n              "symbols_path_in_bundle":   "symbols/marinade_symbols.so",\n              "sources_path_in_bundle":   "srcs",\n              "sources_original_path":   "'"$sources_original"$'",'
  echo "==> coverage: staged $(basename "$symbols") + programs/marinade-finance/src"
  echo "==> coverage: SourcesOriginalPath = ${sources_original:-<empty>}"
else
  coverage_params=""
  echo "==> coverage: WARNING -- no marinade_finance.so WITH DWARF was found; the" >&2
  echo "    bundle will run but render NO source-level coverage. Build the program" >&2
  echo "    with CARGO_PROFILE_RELEASE_DEBUG=2 and CARGO_PROFILE_RELEASE_STRIP=none;" >&2
  echo "    without them anchor emits a symbol table but zero compile units." >&2
fi

commit="$(git -C "$here" rev-parse HEAD)"

# Lineage name is "marinade", not "marinade_invariants". The rename is deliberate:
# corpora are keyed per lineage and FuzzCorp exposes no delete, so renaming is the
# only way to retire a corpus. The old lineage held 7810 inputs generated against an
# earlier program build; replaying them produced an lcov with none of this program's
# own source in it, so the cover task failed with "SourcesOriginalPath ... does not
# match any source file" even though the manifest was correct. A fresh lineage lets
# explore build a corpus against the binary actually being shipped. The name also
# matches the rest of the fleet (loopscale, marginfi, shielded-pool).
cat > "$bundle/manifest.fc.json" <<EOF
{
  "version": 3,
  "revision": { "commit": "$commit" },
  "lineages": [
    {
      "name": "marinade",
      "confs": [
        {
          "name": "invariant_test",
          "driver": {
            "type": "crucible",
            "params": {$coverage_params
              "binary_path_in_bundle": "bin/invariant_test",
              "harness_run_dir_in_bundle": "run"
            }
          },
          "architecture": { "name": "amd64" },
          "cores": 4,
          "memory_kib": 4194304,
          "yield_time_minutes": 120
        }
      ]
    }
  ]
}
EOF

echo "==> bundle assembled at $bundle"
find "$bundle" -type f
