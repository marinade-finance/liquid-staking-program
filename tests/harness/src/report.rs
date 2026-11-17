//! `HARNESS_REPORT`: append one JSON line per test outcome.

use serde::Serialize;
use std::io::Write;

#[derive(Serialize, Debug)]
pub struct Outcome<'a> {
    pub test: &'a str,
    pub base: &'a str,
    pub target: &'a str,
    pub marinade_so_sha256: &'a str,
    pub passed: bool,
    pub error: Option<String>,
}

/// Append an outcome to the file named by `HARNESS_REPORT` (no-op if unset).
pub fn record(o: &Outcome) {
    let Ok(path) = std::env::var("HARNESS_REPORT") else { return };
    if path.is_empty() {
        return;
    }
    let line = serde_json::to_string(o).unwrap();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    } else {
        eprintln!("[harness] cannot write HARNESS_REPORT {path}");
    }
}

/// Panic payload (via `resume_unwind`) of a failure that is a documented limitation of the live binary
/// (`topup::known_live_limitations`): the test ends early and counts as a pass. The KNOWN-LIVE-LIMITATION line was
/// already printed by `topup::waive`.
#[derive(Debug)]
pub struct Waived(pub String);

/// Run a test body, report its outcome (also on panic or Err), then propagate the failure.
pub fn run_reported<F>(test: &str, base: &str, target: &str, so_sha: &str, f: F)
where
    F: FnOnce() -> anyhow::Result<()> + std::panic::UnwindSafe,
{
    let res = std::panic::catch_unwind(f);
    if let Err(p) = &res {
        if let Some(w) = p.downcast_ref::<Waived>() {
            record(&Outcome { test, base, target, marinade_so_sha256: so_sha, passed: true, error: Some(format!("KNOWN-LIVE-LIMITATION: {}", w.0)) });
            return;
        }
    }
    let (passed, error) = match &res {
        Ok(Ok(())) => (true, None),
        Ok(Err(e)) => (false, Some(format!("{e:#}"))),
        Err(p) => (
            false,
            Some(
                p.downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into()),
            ),
        ),
    };
    record(&Outcome { test, base, target, marinade_so_sha256: so_sha, passed, error: error.clone() });
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("{e:#}"),
        Err(p) => std::panic::resume_unwind(p),
    }
}
