# Marinade fuzz harness

A [crucible](https://github.com/asymmetric-research/crucible) fuzz harness for `marinade_finance`,
run on FuzzCorp against a fork of mainnet state.

## Build

`./build-bundle.sh` writes a bundle to `build/bundle/`. It needs `programs/marinade_program.so`
(from `anchor build`) and unpacks `fixtures-mainnet.tar.gz`.

## CI

`.github/workflows/fuzzcorp.yml` builds and uploads the bundle on every PR and push to `main`.
Requires secret `FUZZ_API_KEY` and vars `FUZZ_ORGANIZATION=marinade`, `FUZZ_PROJECT=liquid-staking-program`.
