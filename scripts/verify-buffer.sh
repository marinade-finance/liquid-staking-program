set -euo pipefail
SO="${2:-target/deploy/marinade_finance.so}"
[ -f "$SO" ] || { echo "missing $SO, run the solana-verify build first" >&2; exit 1; }
BUFFER_HASH=$(solana-verify get-buffer-hash -um "$1" | tail -n1)
LOCAL_HASH=$(solana-verify get-executable-hash "$SO" | tail -n1)
echo "buffer $1: $BUFFER_HASH"
echo "$SO: $LOCAL_HASH"
[ "$BUFFER_HASH" = "$LOCAL_HASH" ] || { echo "MISMATCH" >&2; exit 1; }
echo "verified"
