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

Tools: Docker, Solana CLI, `solana-verify` built from git, Anchor `0.27.0` and `0.31.1` via `avm`.
Released `solana-verify` 0.4.15 and 0.5.2 fail on this program's BPF toolchain.
Use a reliable mainnet RPC; the public endpoint drops buffer writes.

## 1. Build

Build the audited commit twice; both hashes must match.

```bash
cargo install --git https://github.com/solana-foundation/solana-verifiable-build --rev 26ba8211eddbfab49d30826d15a2408332b89543 --locked solana-verify --root ~/.local/solana-verify-master
export SVB=~/.local/solana-verify-master/bin/solana-verify
git checkout <COMMIT>
$SVB build --bpf --base-image projectserum/build@sha256:0e11aced57c448c7da9bf0a563a146398ac8b88f357b8fc2e1be314b42320686 --library-name marinade_finance $PWD
$SVB get-executable-hash target/deploy/marinade_finance.so
~/.avm/bin/anchor-0.27.0 build --verifiable
$SVB get-executable-hash target/verifiable/marinade_finance.so
```

Pass the image by digest without a tag and the repo as an absolute path, or `solana-verify` breaks the build.
Call Anchor 0.27.0 by full path: a newer `anchor` on `PATH` fails on the `[toolchain]` pin.

## 2. Write the buffer

The buffer costs ~6.4 SOL in rent, returned to the spill address on upgrade.
Rerun the same command to resume an interrupted write.

```bash
solana-keygen new --no-bip39-passphrase -o buffer.json
solana program write-buffer target/verifiable/marinade_finance.so --buffer buffer.json \
  -u <RPC_URL> --use-rpc --with-compute-unit-price 100000 --max-sign-attempts 50
```

## 3. Hand the buffer to the multisig

The upgrade executes only when the buffer authority is the multisig signer.
Check the hash before and after the move: until then the deployer key could rewrite the buffer.

```bash
$SVB get-buffer-hash -u <RPC_URL> <BUFFER>
solana program set-buffer-authority <BUFFER> --new-buffer-authority 551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp -u <RPC_URL>
solana program show <BUFFER> -u <RPC_URL>
$SVB get-buffer-hash -u <RPC_URL> <BUFFER>
```

## 4. Verify the buffer independently

Each signer checks the buffer from a clean clone, in either of two ways.

```bash
git clone https://github.com/marinade-finance/liquid-staking-program
cd liquid-staking-program
git checkout <COMMIT>
solana program show <BUFFER> -u <RPC_URL>
```

The authority must be `551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp`.

With `solana-verify` only (installed as in step 1); both hashes must be equal:

```bash
$SVB build --bpf --base-image projectserum/build@sha256:0e11aced57c448c7da9bf0a563a146398ac8b88f357b8fc2e1be314b42320686 --library-name marinade_finance $PWD
$SVB get-executable-hash target/deploy/marinade_finance.so
$SVB get-buffer-hash -u <RPC_URL> <BUFFER>
```

With Anchor 0.27.0, which rebuilds and compares in one step:

```bash
cd programs/marinade-finance
~/.avm/bin/anchor-0.27.0 verify -p marinade_finance --provider.cluster <RPC_URL> <BUFFER>
```

## 5. Propose and execute the upgrade

A multisig owner proposes "Upgrade program" in the [multisig UI](https://marinade-finance.github.io/multisig-ui/#/magrsHFQxkkioAy45VWnZnFBBdKVdy2ZiRoRGYT9Wed) with the program id and the buffer.
The UI sets the proposer's wallet as the spill address, which receives the buffer rent.
Once the threshold approves, anyone executes it. Then confirm:

```bash
$SVB get-program-hash -u <RPC_URL> MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
```

## 6. Verified build on the explorer

The explorer [verified-build tab](https://explorer.solana.com/address/MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD/verified-build) trusts only build parameters stored in an [otter-verify](https://github.com/otter-sec/otter-verify) PDA signed by the upgrade authority.
So the PDA write goes through the multisig like the upgrade.

## 7. IDL

Only after the upgrade in step 5 executes.
The IDL is updated through a Realms proposal of the governance above, not through the multisig.
Write the IDL buffer and hand it to the governance, then propose `IdlSetBuffer` as a custom instruction.
Run these outside the repo, in a directory whose `Anchor.toml` has only a `[provider]` section: the repo's `[toolchain]` pin breaks newer Anchor CLIs.

```bash
~/.avm/bin/anchor-0.31.1 idl write-buffer --filepath <REPO>/target/idl/marinade_finance.json MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
~/.avm/bin/anchor-0.31.1 idl set-authority --program-id MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD --new-authority 42VJbDihcS81YJPbuhHnHgvo1ehu42j8VK9sNwrnAarR <IDL_BUFFER>
~/.avm/bin/anchor-0.31.1 idl set-buffer --print-only --buffer <IDL_BUFFER> MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
```

The printed base64 instruction must list `42VJ…` as the signer; it goes into the Realms proposal.

## 8. After the upgrade

- Add the release to the [README](../README.md) with the commit, the upgrade tx and the audit.
- Merge the release into the `mainnet` branch and tag it.
