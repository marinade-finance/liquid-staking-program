# marinade-anchor
Marinade-finance liquid staking program for the Solana blockchain

# Deployments
Branch `mainnet` contains the version deployed on mainnet, keep it up to date with every deployment.

Even though the audited and deployed commit hashes might differ, it is critical that the program source is identical.

## 2026-07-16 (v2.1.0): fix delinquent stakes and introduce deposit fees

commit: [`0f031c4`](https://github.com/marinade-finance/liquid-staking-program/pull/84)

tx: [wyCLBNG716ScBE1rAU7FC2EmqHJxcho3LCofb2vLBcCDxVfXn6SF8b3gfjda1cUEhdYeKwbF2j4AmhimxNA9PUh](https://solscan.io/tx/wyCLBNG716ScBE1rAU7FC2EmqHJxcho3LCofb2vLBcCDxVfXn6SF8b3gfjda1cUEhdYeKwbF2j4AmhimxNA9PUh)

audits:

* [Neodyme](https://docs.marinade.finance/marinade-protocol/security/audits#id-2026)

## 2023-11-14 (v2.0): upgrade with Anchor v0.27.0

commit: [`1bd5133`](https://github.com/marinade-finance/liquid-staking-program/pull/8)

audits:

* [Neodyme](https://marinade.finance/docs/Neodyme_2023.pdf)
* [Sec3](https://marinade.finance/docs/Sec3_2023.pdf)

# Older audits & code reviews

## Q4/2021

* [Kudelski Security](https://marinade.finance/docs/KudelskiSecurity.pdf)
* [Ackee Blockchain](https://marinade.finance/docs/AckeeBlockchain.pdf)
* [Neodyme](https://marinade.finance/docs/Neodyme.pdf)

# Documentation

[Marinade Finance Docs](https://docs.marinade.finance)

[Backend Design](Docs/Backend-Design.md)

# Integration Testing

Note: integration tests are not included in this repo. Tests will be published later.
