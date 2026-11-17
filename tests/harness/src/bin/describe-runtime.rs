//! Resolve runtime spec strings and print what the engine will run, as one JSON object per spec:
//! rent, program hashes, and everything the engine cannot model. The matrix and backtest scripts
//! use it instead of scraping test logs.
//!
//!   describe-runtime <spec>...

use std::process::ExitCode;

fn main() -> ExitCode {
    let specs: Vec<String> = std::env::args().skip(1).collect();
    if specs.is_empty() {
        eprintln!("usage: describe-runtime <spec>...");
        return ExitCode::from(2);
    }
    let mut code = ExitCode::SUCCESS;
    for spec in specs {
        match harness::profile::resolve(&spec) {
            Ok(rt) => {
                let programs: serde_json::Map<String, serde_json::Value> = rt
                    .programs
                    .iter()
                    .map(|(id, bin)| (id.to_string(), serde_json::Value::from(bin.sha256.clone())))
                    .collect();
                let out = serde_json::json!({
                    "spec": spec,
                    "label": rt.label,
                    "rent": {
                        "lamports_per_byte_year": rt.rent.lamports_per_byte_year,
                        "exemption_threshold": rt.rent.exemption_threshold,
                        "burn_percent": rt.rent.burn_percent,
                    },
                    "active_features": rt.active_ids.len(),
                    "bundled_programs": rt.bundled,
                    "programs": programs,
                    "unmodeled": rt.unmodeled,
                    "warnings": rt.warnings,
                });
                println!("{out}");
            }
            Err(e) => {
                println!("{}", serde_json::json!({ "spec": spec, "error": format!("{e:#}") }));
                code = ExitCode::FAILURE;
            }
        }
    }
    code
}
