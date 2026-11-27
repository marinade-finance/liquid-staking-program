# Mainnet deployment

The program upgrade authority is a multisig PDA, and the IDL authority is an SPL Governance (Realms) wallet.
Nobody holds a key that can upgrade alone. A deployer writes a buffer, independent people verify it, and the multisig executes the upgrade.

| What | Address |
|---|---|
| program | `MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD` |
| programData | `4PQH9YmfuKrVyZaibkLYpJZPv2FPaybhq2GAuBcWMSBf` |
| state | `8szGkuLTAux9XMgZ2vtY39jVSowEcpBfFfD8hXSEqdGC` |
| upgrade authority (multisig signer PDA) | `551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp` |
| multisig program | `msigmtwzgXJHj2ext4XJjCDmpbcMuufFb5cHuwg6Xdt` |
| multisig account | `magrsHFQxkkioAy45VWnZnFBBdKVdy2ZiRoRGYT9Wed` |
| IDL authority (Realms governance) | `42VJbDihcS81YJPbuhHnHgvo1ehu42j8VK9sNwrnAarR` |

Tools: Docker, Solana CLI 4.3.0 (cargo-build-sbf 4.3.0, platform-tools v1.57), `solana-verify` >= 0.5.1, Anchor `0.32.1` via `avm` for the IDL only.
The program is an SBPF v3 binary; `IMAGE` below is `solanafoundation/solana-verifiable-build:4.3.0` pinned by digest.
Use a reliable mainnet RPC; the public endpoint drops buffer writes.

## 1. Build

Build the audited commit in the image twice and once locally; all three hashes must match, and the ELF must be SBPF v3.

```bash
export IMAGE=solanafoundation/solana-verifiable-build@sha256:12fd4c0a0790f0fc41ef74b0cdb6bccc167ba6137eb1adb749bac649481c86bd
git checkout <COMMIT>
solana-verify build --arch v3 --base-image $IMAGE --library-name marinade_finance $PWD
solana-verify get-executable-hash target/deploy/marinade_finance.so
solana-verify build --arch v3 --base-image $IMAGE --library-name marinade_finance $PWD
solana-verify get-executable-hash target/deploy/marinade_finance.so
readelf -h target/deploy/marinade_finance.so | grep Flags   # Flags: 0x3
CARGO_TARGET_DIR=/tmp/lsp-target cargo build-sbf --arch v3 --tools-version v1.57 \
  --manifest-path programs/marinade-finance/Cargo.toml --sbf-out-dir /tmp/lsp-local
solana-verify get-executable-hash /tmp/lsp-local/marinade_finance.so
```

Pass the image by digest and the repo as an absolute path, or `solana-verify` breaks the build.
The image runs as root and leaves `target/` owned by root, so the local build uses its own target directory.
With the Anchor CLI the SBPF version goes after `--`: `anchor build -- --arch v3`.

## 2. Write the buffer

The buffer costs ~6.4 SOL in rent, returned to the spill address on upgrade.
Rerun the same command to resume an interrupted write.

```bash
solana-keygen new --no-bip39-passphrase -o buffer.json
solana program write-buffer target/deploy/marinade_finance.so --buffer buffer.json \
  -u <RPC_URL> --use-rpc --with-compute-unit-price 100000 --max-sign-attempts 50
```

## 3. Hand the buffer to the multisig

The upgrade executes only when the buffer authority is the multisig signer.
Check the hash before and after the move: until then the deployer key could rewrite the buffer.

```bash
solana-verify get-buffer-hash -u <RPC_URL> <BUFFER>
solana program set-buffer-authority <BUFFER> --new-buffer-authority 551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp -u <RPC_URL>
solana program show <BUFFER> -u <RPC_URL>
solana-verify get-buffer-hash -u <RPC_URL> <BUFFER>
```

## 4. Verify the buffer independently

Each signer checks the buffer from a clean clone.

```bash
git clone https://github.com/marinade-finance/liquid-staking-program
cd liquid-staking-program
git checkout <COMMIT>
solana program show <BUFFER> -u <RPC_URL>
```

The authority must be `551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp`.

Both hashes must be equal:

```bash
solana-verify build --arch v3 --base-image solanafoundation/solana-verifiable-build@sha256:12fd4c0a0790f0fc41ef74b0cdb6bccc167ba6137eb1adb749bac649481c86bd --library-name marinade_finance $PWD
solana-verify get-executable-hash target/deploy/marinade_finance.so
solana-verify get-buffer-hash -u <RPC_URL> <BUFFER>
```

## 5. Propose and execute the upgrade

A multisig owner proposes "Upgrade program" in the [multisig UI](https://marinade-finance.github.io/multisig-ui/#/magrsHFQxkkioAy45VWnZnFBBdKVdy2ZiRoRGYT9Wed) with the program id and the buffer.
The UI sets the proposer's wallet as the spill address, which receives the buffer rent.
Once the threshold approves, anyone executes it. Then confirm:

```bash
solana-verify get-program-hash -u <RPC_URL> MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
```

## 6. Verified build on the explorer

The explorer [verified-build tab](https://explorer.solana.com/address/MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD/verified-build) trusts only build parameters stored in an [otter-verify](https://github.com/otter-sec/otter-verify) PDA signed by the upgrade authority.
So the PDA write goes through the multisig like the upgrade.
Its build parameters are the ones of step 1:

```bash
solana-verify verify-from-repo --arch v3 --base-image solanafoundation/solana-verifiable-build@sha256:12fd4c0a0790f0fc41ef74b0cdb6bccc167ba6137eb1adb749bac649481c86bd --library-name marinade_finance \
  --commit-hash <COMMIT> --program-id MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD https://github.com/marinade-finance/liquid-staking-program
```

## 7. IDL

Only after the upgrade in step 5 executes.
The IDL is updated through a Realms proposal of the governance above, not through the multisig.
Write the IDL buffer and hand it to the governance, then propose `IdlSetBuffer` as a custom instruction.
Run these outside the repo, in a directory whose `Anchor.toml` has only a `[provider]` section: the repo's `[toolchain]` pin breaks newer Anchor CLIs.
Anchor 0.32.1 writes the 0.30+ IDL format; Anchor TS clients older than 0.30 cannot read it from chain.

```bash
~/.avm/bin/anchor-0.32.1 idl write-buffer --filepath <REPO>/target/idl/marinade_finance.json MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
~/.avm/bin/anchor-0.32.1 idl set-authority --program-id MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD --new-authority 42VJbDihcS81YJPbuhHnHgvo1ehu42j8VK9sNwrnAarR <IDL_BUFFER>
~/.avm/bin/anchor-0.32.1 idl set-buffer --print-only --buffer <IDL_BUFFER> MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
```

The printed base64 instruction must list `42VJ…` as the signer; it goes into the Realms proposal.

## 8. After the upgrade

- Add the release to the [README](../README.md) with the commit, the upgrade tx and the audit.
- Merge the release into the `mainnet` branch and tag it.

## 9. Rollback

Before step 5, keep the running program: `solana program dump -u <RPC_URL> MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD previous.so`.
To roll back, write `previous.so` to a buffer and upgrade as in steps 2–5.
Once SIMD-0500 is active, a pre-v3 binary can no longer be deployed; the rollback is then to pause the program and fix forward.
