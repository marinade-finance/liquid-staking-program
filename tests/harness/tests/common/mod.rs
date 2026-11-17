#![allow(dead_code)]
use anyhow::Result;
use harness::env::{env_from_spec, sol, TestEnv};
use harness::invariants::Checker;
use harness::marinade::{Marinade, SetupOpts, User};
use harness::profile;
use solana_address::Address;

pub fn target_env() -> Result<TestEnv> {
    env_from_spec(&profile::target_spec())
}

pub struct Fixture {
    pub env: TestEnv,
    pub m: Marinade,
    pub chk: Checker,
    pub validators: Vec<(Address, u32)>,
    pub users: Vec<User>,
}

pub fn so_sha(env: &TestEnv) -> String {
    env.marinade_so.sha256.clone()
}

/// Env on `spec` + Marinade instance + `nvals` equal-score validators + funded users.
pub fn fixture(spec: &str, nvals: usize, nusers: usize) -> Result<Fixture> {
    let mut env = env_from_spec(spec)?;
    let m = Marinade::setup(&mut env, &SetupOpts::default())?;
    let mut validators = vec![];
    for _ in 0..nvals {
        validators.push(m.add_new_validator(&mut env, 100)?);
    }
    let users = (0..nusers).map(|_| m.new_user(&mut env, sol(100_000))).collect();
    let chk = Checker::new(&env, &m);
    Ok(Fixture { env, m, chk, validators, users })
}

impl Fixture {
    pub fn check(&mut self, op: &str) {
        if let Err(e) = self.chk.check_after(&self.env, &self.m, op) {
            panic!("{e}");
        }
    }
}
