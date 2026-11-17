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
    /// HARNESS_LENIENT=1: record failures instead of stopping at the first one (see `finish`)
    pub lenient: bool,
    pub findings: Vec<String>,
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
    let lenient = std::env::var("HARNESS_LENIENT").map(|v| v == "1").unwrap_or(false);
    Ok(Fixture { env, m, chk, validators, users, lenient, findings: vec![] })
}

impl Fixture {
    /// HARNESS_RENT_TOPUP=1 on the binary live on mainnet: is this failure one of `topup::known_live_limitations`?
    /// Prints a KNOWN-LIVE-LIMITATION line when it is. Anything else is a real failure.
    pub fn known_live_limitation(&self, op: &str, e: &TxFailure) -> bool {
        let test = std::thread::current().name().unwrap_or("?").to_string();
        harness::topup::waive(&self.env.marinade_so.sha256, &test, op, &e.to_string()).is_some()
    }

    pub fn check(&mut self, op: &str) {
        if let Err(e) = self.chk.check_after(&self.env, &self.m, op) {
            if self.lenient {
                self.findings.push(e);
            } else {
                panic!("{e}");
            }
        }
    }

    /// op result: strict -> panic on failure; lenient -> record and continue. Invariants are checked afterwards.
    pub fn step<T>(&mut self, name: &str, r: Result<T, TxFailure>) -> Option<T> {
        let v = match r {
            Ok(v) => Some(v),
            Err(e) => {
                if self.known_live_limitation(name, &e) {
                    // documented limitation of the live binary: skip the op, keep going
                    None
                } else if self.lenient {
                    self.findings.push(format!("{name} failed: {e}"));
                    None
                } else {
                    panic!("{name} failed: {e}");
                }
            }
        };
        self.check(name);
        v
    }

    pub fn finish(&mut self) {
        if !self.findings.is_empty() {
            panic!("{} finding(s):\n{}", self.findings.len(), self.findings.iter().map(|x| format!(" - {x}")).collect::<Vec<_>>().join("\n"));
        }
    }
}

use harness::crank::{Crank, CrankReport};
use harness::env::TxFailure;

impl Fixture {
    /// Run a crank body with invariants checked after every crank op.
    pub fn crank<R>(&mut self, body: impl FnOnce(&mut Crank, &mut TestEnv) -> Result<R>) -> Result<(R, CrankReport)> {
        let Fixture { env, m, chk, lenient, findings, .. } = self;
        let m: &Marinade = m;
        let lenient = *lenient;
        let mut found: Vec<String> = vec![];
        let (r, report) = {
            let mut c = Crank::new(m).with_hook(|e, op| match chk.check_after(e, m, op) {
                Err(msg) if lenient => {
                    found.push(msg);
                    Ok(())
                }
                other => other,
            });
            if lenient {
                c.strict = false;
            }
            let r = body(&mut c, env)?;
            (r, c.report)
        };
        findings.extend(found);
        if lenient {
            for o in report.failures() {
                findings.push(format!("{} failed: {}", o.name, o.error.clone().unwrap_or_default()));
            }
        }
        Ok((r, report))
    }

    /// unwrap an op result and check the invariants right after it
    pub fn ok<T>(&mut self, name: &str, r: Result<T, TxFailure>) -> T {
        let v = match r {
            Ok(v) => v,
            Err(e) => {
                if self.known_live_limitation(name, &e) {
                    // ends the test early; `run_reported` counts it as a pass
                    std::panic::resume_unwind(Box::new(harness::report::Waived(format!("{name}: {}", e.marinade_name().unwrap_or("?")))));
                }
                panic!("{name} failed: {e}")
            }
        };
        self.check(name);
        v
    }

    /// deposit from user `u` then crank the stake-delta window of this epoch
    pub fn deposit_and_stake(&mut self, u: usize, lamports: u64) -> Result<()> {
        let r = self.m.deposit(&mut self.env, &self.users[u], lamports);
        self.ok("deposit", r);
        self.crank(|c, env| c.stake_delta(env))?;
        Ok(())
    }
}

/// Run a test body on the TARGET runtime, reporting the outcome when HARNESS_REPORT is set.
pub fn scenario(name: &str, nvals: usize, body: impl FnOnce(&mut Fixture) -> Result<()> + std::panic::UnwindSafe) {
    let spec = profile::target_spec();
    let spec2 = spec.clone();
    let sha = {
        let rt = profile::resolve(&spec).unwrap();
        profile::marinade_so(&rt).map(|b| b.sha256).unwrap_or_default()
    };
    harness::report::run_reported(&format!("scenarios::{name}"), &spec, &spec, &sha, move || {
        let mut f = fixture(&spec2, nvals, 3)?;
        body(&mut f)
    });
}
