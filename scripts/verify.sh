set -euo pipefail
SO="${1:-target/deploy/marinade_finance.so}"
[ -f "$SO" ] || { echo "missing $SO, run the solana-verify build first" >&2; exit 1; }
PROGRAM_HASH=$(solana-verify get-program-hash -um MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD | tail -n1)
LOCAL_HASH=$(solana-verify get-executable-hash "$SO" | tail -n1)
echo "on-chain program: $PROGRAM_HASH"
echo "$SO: $LOCAL_HASH"
[ "$PROGRAM_HASH" = "$LOCAL_HASH" ] || { echo "MISMATCH" >&2; exit 1; }
echo "verified"
