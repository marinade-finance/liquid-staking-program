set -ex
solana-verify get-program-hash -um MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
solana-verify get-executable-hash target/deploy/marinade_finance.so
