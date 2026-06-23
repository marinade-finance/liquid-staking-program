#!/usr/bin/env bash
#
# Verifiable-build check for the marinade_finance program.
#
# Builds the program reproducibly (inside the projectserum/build:<anchor>
# Docker image) and compares the resulting bytecode against an on-chain
# account, printing the sha256 checksums of both sides.
#
# Usage:
#   .buildkite/verify.sh [TARGET]
#
#   TARGET  Optional. The address to compare against. May be either:
#             - a deployed program id (default: the one in Anchor.toml), or
#             - a Buffer account (e.g. a staged upgrade buffer).
#           The account type is auto-detected; the correct upgradeable-loader
#           header is stripped (45 bytes for ProgramData, 37 for a Buffer).
#
# For a program id we also run the authoritative `anchor verify`; for a buffer
# (which `anchor verify` does not support) the byte comparison here is the gate.
#
# Requires: cargo (rustup) and Docker on the agent. The build runs inside
# Docker, so a host Solana toolchain is not needed.
#
# Env:
#   CLUSTER  RPC url or moniker (default: https://api.mainnet-beta.solana.com)
set -euo pipefail

. "$HOME/.cargo/env"

# --- 1. Figure out which toolchain + program from Anchor.toml ---------------
ANCHOR_VERSION=$(grep -E '^[[:space:]]*anchor_version' Anchor.toml | sed -E 's/.*"([^"]+)".*/\1/')
PROGRAM_ID=$(grep -E '^[[:space:]]*marinade_finance[[:space:]]*=' Anchor.toml | sed -E 's/.*"([^"]+)".*/\1/')
TARGET="${1:-$PROGRAM_ID}"
CLUSTER="${CLUSTER:-https://api.mainnet-beta.solana.com}"
SO="target/verifiable/marinade_finance.so"
echo "+++ anchor ${ANCHOR_VERSION}  target ${TARGET}  cluster ${CLUSTER}"

# --- 2. Install avm + the pinned anchor (idempotent) ------------------------
export PATH="$HOME/.cargo/bin:$HOME/.avm/bin:$PATH"
command -v avm >/dev/null 2>&1 || cargo install --git https://github.com/coral-xyz/anchor avm --locked
avm install "$ANCHOR_VERSION"
# Call the versioned binary directly so avm doesn't try to auto-install the
# Solana CLI on the host (the build happens inside Docker anyway).
ANCHOR="$HOME/.avm/bin/anchor-$ANCHOR_VERSION"

# --- 3. Detect what kind of account TARGET is -------------------------------
KIND=$(python3 - "$TARGET" "$CLUSTER" <<'PY'
import sys, json, urllib.request
target, cluster = sys.argv[1], sys.argv[2]
req = urllib.request.Request(cluster,
    data=json.dumps({"jsonrpc":"2.0","id":1,"method":"getAccountInfo",
                     "params":[target,{"encoding":"jsonParsed"}]}).encode(),
    headers={"Content-Type":"application/json"})
v = json.load(urllib.request.urlopen(req, timeout=30))["result"]["value"]
print((v or {}).get("data", {}).get("parsed", {}).get("type", "unknown"))
PY
)
echo "+++ target account type: ${KIND}"

# --- 4. Build + (for programs) authoritative anchor verify ------------------
set +e
VERIFY_RC=0
if [ "$KIND" = "program" ]; then
  ( cd programs/marinade-finance && "$ANCHOR" verify --provider.cluster "$CLUSTER" "$TARGET" )
  VERIFY_RC=$?
else
  # Buffer (or anything anchor verify can't target): just build reproducibly.
  "$ANCHOR" build --verifiable -p marinade_finance
  VERIFY_RC=$?
fi
set -e
[ "$VERIFY_RC" -eq 0 ] || echo "+++ (build/anchor-verify exited ${VERIFY_RC}; continuing to print checksums)"

# --- 5. Print checksums + compare (authoritative gate for buffers) ----------
echo "+++ checksums"
set +e
python3 - "$SO" "$TARGET" "$CLUSTER" <<'PY'
import sys, json, base64, hashlib, urllib.request

so_path, target, cluster = sys.argv[1], sys.argv[2], sys.argv[3]

def rpc(method, params):
    req = urllib.request.Request(
        cluster,
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30))["result"]

sha = lambda b: hashlib.sha256(b).hexdigest()

local = open(so_path, "rb").read()

# Resolve the account that actually holds the code, and the loader header size.
acct = rpc("getAccountInfo", [target, {"encoding": "jsonParsed"}])["value"]
kind = (acct or {}).get("data", {}).get("parsed", {}).get("type", "unknown")

if kind == "program":
    # program account points at its ProgramData account (45-byte header)
    code_addr = acct["data"]["parsed"]["info"]["programData"]
    header = 45
elif kind == "buffer":
    # buffer holds the code directly (37-byte header)
    code_addr = target
    header = 37
else:
    sys.stderr.write(f"unsupported account type for {target!r}: {kind}\n")
    sys.exit(2)

raw = base64.b64decode(rpc("getAccountInfo", [code_addr, {"encoding": "base64"}])["value"]["data"][0])
onchain = raw[header:]              # strip the upgradeable-loader metadata header
trimmed = onchain.rstrip(b"\x00")   # actual deployed code, padding removed
comparable = onchain[:len(local)]   # the slice anchor compares the local build against
tail = onchain[len(local):]

print(f"account kind              : {kind}")
print(f"code account              : {code_addr}")
print(f"header stripped           : {header} bytes")
print(f"local  .so size           : {len(local)}")
print(f"onchain code size (padded): {len(onchain)}")
print(f"onchain code size (real)  : {len(trimmed)}")
print()
print(f"local   sha256            : {sha(local)}")
print(f"onchain sha256 (trimmed)  : {sha(trimmed)}")
print(f"onchain sha256 (==local len): {sha(comparable)}")
print()
match = (comparable == local) and all(b == 0 for b in tail)
print("RESULT: MATCH" if match else "RESULT: MISMATCH")
sys.exit(0 if match else 1)
PY
COMPARE_RC=$?
set -e

# --- 6. Exit code -----------------------------------------------------------
# Program: anchor verify is authoritative. Buffer: our byte comparison is.
if [ "$KIND" = "program" ]; then
  exit "$VERIFY_RC"
else
  exit "$COMPARE_RC"
fi
