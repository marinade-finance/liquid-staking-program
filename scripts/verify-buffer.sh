set -ex
solana-verify get-buffer-hash -um $1
solana-verify get-executable-hash target/deploy/marinade_finance.so
