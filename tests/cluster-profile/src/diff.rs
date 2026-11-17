use crate::model::*;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

pub fn load(p: &Path) -> Result<Profile> {
    let s = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parsing {}", p.display()))
}

#[derive(Serialize, Debug, Clone)]
pub struct FeatureRow {
    pub id: String,
    pub module: String,
    pub name: String,
    pub sources: Vec<String>,
    /// profile name -> "active@slot" | "pending" | "inactive" | "unknown"
    pub status: BTreeMap<String, String>,
}

#[derive(Serialize, Debug, Default)]
pub struct Report {
    pub base: String,
    pub others: Vec<String>,
    pub versions: BTreeMap<String, Version>,
    pub upcoming_for_base: Vec<FeatureRow>,
    pub pending_anywhere: Vec<FeatureRow>,
    pub active_on_base_not_other: Vec<FeatureRow>,
    pub master_only_inactive: Vec<FeatureRow>,
    pub sysvar_diffs: Vec<String>,
    pub program_diffs: Vec<ProgramDiff>,
    pub buffers: Vec<BufferRow>,
}

#[derive(Serialize, Debug)]
pub struct ProgramDiff {
    pub id: String,
    pub name: String,
    /// profile -> sha256 | "builtin" | "absent"
    pub state: BTreeMap<String, String>,
}

#[derive(Serialize, Debug)]
pub struct BufferRow {
    pub buffer: String,
    pub for_feature: String,
    pub module: String,
    pub target_program: String,
    /// profile -> sha256 of the buffer ELF, or "absent"
    pub present: BTreeMap<String, String>,
    /// profile -> whether the buffer ELF equals the target program binary currently deployed there
    pub equals_deployed: BTreeMap<String, bool>,
}

fn row(id: &str, profiles: &[&Profile]) -> FeatureRow {
    let mut r = FeatureRow { id: id.into(), module: String::new(), name: String::new(), sources: vec![], status: BTreeMap::new() };
    for p in profiles {
        match p.features.get(id) {
            Some(f) => {
                if r.module.is_empty() {
                    r.module = f.module.clone().unwrap_or_default();
                    r.name = f.name.clone();
                }
                for x in &f.sources {
                    if !r.sources.contains(x) {
                        r.sources.push(x.clone());
                    }
                }
                r.status.insert(p.name.clone(), status_label(f));
            }
            None => {
                r.status.insert(p.name.clone(), "unknown".into());
            }
        }
    }
    r.sources.sort();
    if r.module.is_empty() {
        r.module = id.to_string();
    }
    r
}

fn st<'a>(p: &'a Profile, id: &str) -> &'a str {
    p.features.get(id).map(|f| f.status.as_str()).unwrap_or("unknown")
}

pub fn compute(base: &Profile, others: &[Profile]) -> Report {
    let all: Vec<&Profile> = std::iter::once(base).chain(others.iter()).collect();
    let mut r = Report { base: base.name.clone(), others: others.iter().map(|o| o.name.clone()).collect(), ..Default::default() };
    for p in &all {
        r.versions.insert(p.name.clone(), p.version.clone());
    }
    let ids: BTreeSet<&String> = all.iter().flat_map(|p| p.features.keys()).collect();
    for id in ids {
        let any_other_live = others.iter().any(|o| matches!(st(o, id), "active" | "pending"));
        if st(base, id) != "active" && any_other_live {
            r.upcoming_for_base.push(row(id, &all));
        }
        if all.iter().any(|p| st(p, id) == "pending") {
            r.pending_anywhere.push(row(id, &all));
        }
        if st(base, id) == "active" && others.iter().any(|o| st(o, id) == "inactive") {
            r.active_on_base_not_other.push(row(id, &all));
        }
        let rw = row(id, &all);
        if rw.sources == ["master"] && all.iter().all(|p| st(p, id) != "active" && st(p, id) != "pending") {
            r.master_only_inactive.push(rw);
        }
    }
    let key = |x: &FeatureRow| (x.module.clone(), x.id.clone());
    r.upcoming_for_base.sort_by_key(key);
    r.pending_anywhere.sort_by_key(key);
    r.active_on_base_not_other.sort_by_key(key);
    r.master_only_inactive.sort_by_key(key);

    // sysvars
    for o in others {
        if base.sysvars.rent != o.sysvars.rent {
            r.sysvar_diffs.push(format!("rent: {} = {}, {} = {}", base.name, serde_json::to_string(&base.sysvars.rent).unwrap(), o.name, serde_json::to_string(&o.sysvars.rent).unwrap()));
        }
        if base.sysvars.epoch_schedule != o.sysvars.epoch_schedule {
            r.sysvar_diffs.push(format!(
                "epoch_schedule: {} = {}, {} = {}",
                base.name,
                serde_json::to_string(&base.sysvars.epoch_schedule).unwrap(),
                o.name,
                serde_json::to_string(&o.sysvars.epoch_schedule).unwrap()
            ));
        }
    }

    // programs
    let pstate = |p: &Profile, id: &str| -> String {
        match p.programs.get(id) {
            None => "absent".into(),
            Some(x) if x.absent == Some(true) => "absent".into(),
            Some(x) if x.builtin == Some(true) => "builtin".into(),
            Some(x) => x.sha256.clone().unwrap_or_else(|| "unknown".into()),
        }
    };
    let pids: BTreeSet<&String> = all.iter().flat_map(|p| p.programs.keys()).collect();
    for id in pids {
        let states: Vec<String> = all.iter().map(|p| pstate(p, id)).collect();
        if states.iter().any(|s| s != &states[0]) {
            let name = all.iter().find_map(|p| p.programs.get(id)).map(|x| x.name.clone()).unwrap_or_default();
            r.program_diffs.push(ProgramDiff {
                id: id.clone(),
                name,
                state: all.iter().map(|p| p.name.clone()).zip(states).collect(),
            });
        }
    }

    // buffers
    let bids: BTreeSet<&String> = all.iter().flat_map(|p| p.buffers.keys()).collect();
    for b in bids {
        let any = all.iter().find_map(|p| p.buffers.get(b)).unwrap();
        let mut br = BufferRow {
            buffer: b.clone(),
            for_feature: any.for_feature.clone(),
            module: any.module.clone(),
            target_program: any.target_program.clone(),
            present: BTreeMap::new(),
            equals_deployed: BTreeMap::new(),
        };
        for p in &all {
            match p.buffers.get(b) {
                Some(x) => {
                    br.present.insert(p.name.clone(), x.sha256.clone());
                    let eq = p.programs.get(&x.target_program).and_then(|t| t.sha256.as_ref()) == Some(&x.sha256);
                    br.equals_deployed.insert(p.name.clone(), eq);
                }
                None => {
                    br.present.insert(p.name.clone(), "absent".into());
                }
            }
        }
        r.buffers.push(br);
    }
    r
}

fn table(out: &mut String, rows: &[FeatureRow], names: &[String], with_desc: bool) {
    let _ = write!(out, "| feature | id |");
    for n in names {
        let _ = write!(out, " {n} |");
    }
    let _ = writeln!(out, "{}", if with_desc { " description |" } else { "" });
    let _ = write!(out, "|---|---|");
    for _ in names {
        let _ = write!(out, "---|");
    }
    let _ = writeln!(out, "{}", if with_desc { "---|" } else { "" });
    for r in rows {
        let _ = write!(out, "| `{}` | `{}` |", r.module, r.id);
        for n in names {
            let _ = write!(out, " {} |", r.status.get(n).map(String::as_str).unwrap_or("-"));
        }
        let _ = writeln!(out, "{}", if with_desc { format!(" {} |", r.name.replace('|', "\\|")) } else { String::new() });
    }
    let _ = writeln!(out);
}

pub fn markdown(r: &Report) -> String {
    let mut o = String::new();
    let names: Vec<String> = std::iter::once(r.base.clone()).chain(r.others.iter().cloned()).collect();
    let _ = writeln!(o, "# Cluster profile diff: {} vs {}\n", r.base, r.others.join(", "));
    let _ = writeln!(o, "## Versions\n");
    let _ = writeln!(o, "| profile | solana-core | feature-set |\n|---|---|---|");
    for n in &names {
        let v = &r.versions[n];
        let _ = writeln!(o, "| {n} | {} | {} |", v.solana_core, v.feature_set.map(|x| x.to_string()).unwrap_or_default());
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "## Upcoming for {} ({})\n", r.base, r.upcoming_for_base.len());
    let _ = writeln!(o, "Active or pending on another cluster, not active on {}.\n", r.base);
    if r.upcoming_for_base.is_empty() {
        let _ = writeln!(o, "None.\n");
    } else {
        table(&mut o, &r.upcoming_for_base, &names, true);
    }

    let _ = writeln!(o, "## Pending anywhere ({})\n", r.pending_anywhere.len());
    if r.pending_anywhere.is_empty() {
        let _ = writeln!(o, "None.\n");
    } else {
        table(&mut o, &r.pending_anywhere, &names, true);
    }

    let _ = writeln!(o, "## Active on {} but inactive elsewhere ({}) -- odd, investigate\n", r.base, r.active_on_base_not_other.len());
    if r.active_on_base_not_other.is_empty() {
        let _ = writeln!(o, "None.\n");
    } else {
        table(&mut o, &r.active_on_base_not_other, &names, true);
    }

    let _ = writeln!(o, "## Only known on agave master, inactive everywhere ({})\n", r.master_only_inactive.len());
    for f in &r.master_only_inactive {
        let _ = writeln!(o, "- `{}` `{}`: {}", f.module, f.id, f.name);
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "## Sysvar differences\n");
    if r.sysvar_diffs.is_empty() {
        let _ = writeln!(o, "None.\n");
    } else {
        for s in &r.sysvar_diffs {
            let _ = writeln!(o, "- {s}");
        }
        let _ = writeln!(o);
    }

    let _ = writeln!(o, "## Program differences\n");
    if r.program_diffs.is_empty() {
        let _ = writeln!(o, "None.\n");
    } else {
        let _ = write!(o, "| program | id |");
        for n in &names {
            let _ = write!(o, " {n} |");
        }
        let _ = write!(o, "\n|---|---|");
        for _ in &names {
            let _ = write!(o, "---|");
        }
        let _ = writeln!(o);
        for p in &r.program_diffs {
            let _ = write!(o, "| {} | `{}` |", p.name, p.id);
            for n in &names {
                let s = &p.state[n];
                let short = if s.len() == 64 { format!("`{}`", &s[..12]) } else { s.clone() };
                let _ = write!(o, " {short} |");
            }
            let _ = writeln!(o);
        }
        let _ = writeln!(o);
    }

    let _ = writeln!(o, "## Pending-upgrade buffers\n");
    if r.buffers.is_empty() {
        let _ = writeln!(o, "None found on any cluster.\n");
    } else {
        for b in &r.buffers {
            let _ = writeln!(o, "- `{}` (feature `{}` {}, target `{}`)", b.buffer, b.for_feature, b.module, b.target_program);
            for n in &names {
                let p = &b.present[n];
                let extra = match b.equals_deployed.get(n) {
                    Some(true) => " (already identical to deployed program)",
                    Some(false) => "",
                    None => "",
                };
                let _ = writeln!(o, "  - {n}: {}{extra}", if p.len() == 64 { &p[..12] } else { p.as_str() });
            }
        }
        let _ = writeln!(o);
    }
    o
}

pub fn run(base: &Path, others: &[std::path::PathBuf], json: Option<&Path>) -> Result<()> {
    let b = load(base)?;
    let os = others.iter().map(|p| load(p)).collect::<Result<Vec<_>>>()?;
    let r = compute(&b, &os);
    print!("{}", markdown(&r));
    if let Some(j) = json {
        crate::snapshot::write_json(j, &r)?;
    }
    Ok(())
}

pub fn list_upcoming(base: &Path, others: &[std::path::PathBuf]) -> Result<()> {
    let b = load(base)?;
    let os = others.iter().map(|p| load(p)).collect::<Result<Vec<_>>>()?;
    let r = compute(&b, &os);
    let mut lines: Vec<String> = r.upcoming_for_base.iter().map(|f| format!("{} {}", f.id, f.module)).collect();
    lines.sort();
    for l in lines {
        println!("{l}");
    }
    Ok(())
}
