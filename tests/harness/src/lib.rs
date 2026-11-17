//! In-process LiteSVM harness for the Marinade liquid staking program.
//!
//! * [`profile`]: spec strings -> [`profile::Runtime`] (feature set, rent, programs)
//! * [`env`]: [`env::TestEnv`], a LiteSVM configured from a runtime, with epoch control
//! * [`marinade`]: fixture + typed instruction builders/ops + readers
//! * [`crank`]: one "epoch crank" like marcrank's do-work
//! * [`invariants`]: accounting invariants checked after every op
//! * [`topup`]: operational mode (`HARNESS_RENT_TOPUP=1`): marcrank's stake rent top-up + known live limitations
//! * [`report`]: JSON-lines outcome reports (`HARNESS_REPORT`)
//!
//! See `tests/ARCHITECTURE.md` for the shared design.

pub mod convert;
pub mod crank;
pub mod effects;
pub mod env;
pub mod invariants;
pub mod marinade;
pub mod profile;
pub mod report;
pub mod topup;

pub use solana_address::Address as Pubkey;
pub use solana_keypair::Keypair;
pub use solana_signer::Signer;

/// Repo root (`liquid-staking-program/`), derived from this crate's manifest dir (`tests/harness`).
pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// `tests/` directory.
pub fn tests_dir() -> std::path::PathBuf {
    repo_root().join("tests")
}

/// Small convenience on keypairs.
pub trait KeypairExt {
    fn pubkey_addr(&self) -> Pubkey;
}
impl KeypairExt for Keypair {
    fn pubkey_addr(&self) -> Pubkey {
        self.pubkey()
    }
}
