//! Spec strings -> [`Runtime`].
//!
//! Grammar: `<profile.json|builtin>[,+F][,-F][,rent=N][,program:<id>=<file.so>]`
//! (`F` = base58 feature id or module name). Tokens apply left to right.

use crate::effects;
use agave_feature_set::{FeatureSet, FEATURE_NAMES};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use solana_address::Address;
use solana_epoch_schedule::EpochSchedule;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

pub const STAKE_ID: &str = "Stake11111111111111111111111111111111111111";
pub const BPF_LOADER_UPGRADEABLE: &str = "BPFLoaderUpgradeab1e11111111111111111111111";
pub const BPF_LOADER_2: &str = "BPFLoader2111111111111111111111111111111111";
pub const NATIVE_LOADER: &str = "NativeLoader1111111111111111111111111111111";

pub fn addr(s: &str) -> Address {
    Address::from_str(s).unwrap_or_else(|_| panic!("bad address {s}"))
}

#[derive(Clone, Debug)]
pub struct ProgramBin {
    pub bytes: Arc<Vec<u8>>,
    pub loader: Address,
    pub sha256: String,
    pub source: String,
}

impl ProgramBin {
    pub fn new(bytes: Vec<u8>, loader: Address, source: impl Into<String>) -> Self {
        let sha256 = hex(&Sha256::digest(&bytes));
        Self { bytes: Arc::new(bytes), loader, sha256, source: source.into() }
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

/// A fully resolved cluster runtime.
#[derive(Clone)]
pub struct Runtime {
    pub label: String,
    pub spec: String,
    pub feature_set: FeatureSet,
    /// every feature id considered active, including ones unknown to the engine
    pub active_ids: BTreeSet<String>,
    pub rent: effects::RentParams,
    pub epoch_schedule: EpochSchedule,
    /// program binaries to load over the engine's bundled ones
    pub programs: BTreeMap<Address, ProgramBin>,
    /// true: start from the engine's bundled core programs (stake v5, token, ...)
    pub bundled: bool,
    /// things the engine cannot model (active unknown features, missing buffers, ...)
    pub unmodeled: Vec<String>,
    pub warnings: Vec<String>,
    /// Marinade binary inside the profile (if any), for `MARINADE_SO=profile`
    pub marinade_so: Option<ProgramBin>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Runtime({:?}, rent={}x{}, {} active features, {} programs, unmodeled={})",
            self.label,
            self.rent.lamports_per_byte_year,
            self.rent.exemption_threshold,
            self.active_ids.len(),
            self.programs.len(),
            self.unmodeled.len()
        )
    }
}

impl Runtime {
    pub fn is_active(&self, id: &str) -> bool {
        self.active_ids.contains(id)
    }

    pub fn rent_sysvar(&self) -> solana_rent::Rent {
        rent_sysvar(self.rent)
    }

    pub fn stake_rent(&self) -> u64 {
        self.rent_sysvar().minimum_balance(200)
    }
}

#[allow(deprecated)]
pub fn rent_sysvar(p: effects::RentParams) -> solana_rent::Rent {
    solana_rent::Rent {
        lamports_per_byte: p.lamports_per_byte_year,
        exemption_threshold: p.exemption_threshold.to_le_bytes(),
        burn_percent: p.burn_percent,
    }
}

// ---------------------------------------------------------------- spec parsing

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Enable(String),
    Disable(String),
    Rent(u64),
    Program(String, PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub base: String,
    pub ops: Vec<Op>,
}

pub fn parse_spec(spec: &str) -> Result<Spec> {
    let mut parts = spec.split(',').map(str::trim).filter(|s| !s.is_empty());
    let base = parts.next().ok_or_else(|| anyhow!("empty spec"))?.to_string();
    let mut ops = vec![];
    for p in parts {
        if let Some(f) = p.strip_prefix('+') {
            ops.push(Op::Enable(f.to_string()));
        } else if let Some(f) = p.strip_prefix('-') {
            ops.push(Op::Disable(f.to_string()));
        } else if let Some(n) = p.strip_prefix("rent=") {
            ops.push(Op::Rent(n.parse().with_context(|| format!("bad rent in {p:?}"))?));
        } else if let Some(rest) = p.strip_prefix("program:") {
            let (id, file) = rest.split_once('=').ok_or_else(|| anyhow!("bad token {p:?}, want program:<id>=<file.so>"))?;
            ops.push(Op::Program(id.to_string(), PathBuf::from(file)));
        } else {
            bail!("unrecognized spec token {p:?} in {spec:?}");
        }
    }
    Ok(Spec { base, ops })
}

// ---------------------------------------------------------------- profile json

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct FeatureEntry {
    pub name: String,
    pub module: Option<String>,
    pub status: String,
    pub activated_at: Option<u64>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct RentJson {
    pub lamports_per_byte_year: u64,
    pub exemption_threshold: f64,
    pub burn_percent: u8,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct EpochScheduleJson {
    pub slots_per_epoch: u64,
    pub leader_schedule_slot_offset: u64,
    pub warmup: bool,
    pub first_normal_epoch: u64,
    pub first_normal_slot: u64,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct SysvarsJson {
    pub rent: Option<RentJson>,
    pub epoch_schedule: Option<EpochScheduleJson>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct ProgramEntry {
    pub name: String,
    pub owner: String,
    pub builtin: bool,
    pub absent: bool,
    pub sha256: String,
    pub file: String,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct BufferEntry {
    pub for_feature: String,
    pub target_program: String,
    pub sha256: String,
    pub file: String,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct ProfileJson {
    pub schema: u32,
    pub name: String,
    pub features: BTreeMap<String, FeatureEntry>,
    pub sysvars: SysvarsJson,
    pub programs: BTreeMap<String, ProgramEntry>,
    pub buffers: BTreeMap<String, BufferEntry>,
}

fn id_of_name(profile: Option<&ProfileJson>, name: &str) -> Option<String> {
    if Address::from_str(name).is_ok() {
        return Some(name.to_string());
    }
    if let Some(p) = profile {
        for (id, f) in &p.features {
            if f.module.as_deref() == Some(name) {
                return Some(id.clone());
            }
        }
    }
    if name == "deprecate_rent_exemption_threshold" {
        return Some(effects::DEPRECATE_RENT_EXEMPTION_THRESHOLD.to_string());
    }
    for (id, m, _) in effects::SIMD_0437_STEPS {
        if *m == name {
            return Some(id.to_string());
        }
    }
    for s in effects::PROGRAM_SWAPS {
        if s.feature_module == name {
            return Some(s.feature_id.to_string());
        }
    }
    for u in effects::UNMODELED {
        if u.feature_module == name {
            return Some(u.feature_id.to_string());
        }
    }
    None
}

fn module_of(profile: Option<&ProfileJson>, id: &str) -> Option<String> {
    profile.and_then(|p| p.features.get(id)).and_then(|f| f.module.clone()).or_else(|| {
        effects::SIMD_0437_STEPS.iter().find(|(i, _, _)| *i == id).map(|(_, m, _)| m.to_string())
    })
}

fn find_file(p: &Path, profile_dir: Option<&Path>) -> Result<PathBuf> {
    if p.is_absolute() && p.exists() {
        return Ok(p.to_path_buf());
    }
    let mut cands = vec![p.to_path_buf()];
    if let Some(d) = profile_dir {
        cands.push(d.join(p));
    }
    cands.push(crate::tests_dir().join(p));
    cands.push(crate::tests_dir().join("profiles").join(p));
    cands.push(crate::repo_root().join(p));
    for c in cands {
        if c.exists() {
            return Ok(c);
        }
    }
    bail!("file {} not found", p.display())
}

fn loader_for(owner: &str) -> Address {
    if owner.is_empty() {
        addr(BPF_LOADER_UPGRADEABLE)
    } else {
        addr(owner)
    }
}

fn warn(rt: &mut Runtime, msg: String) {
    eprintln!("[harness] WARNING ({}): {msg}", rt.label);
    rt.warnings.push(msg);
}

fn unmodeled(rt: &mut Runtime, msg: String) {
    eprintln!("[harness] UNMODELED ({}): {msg}", rt.label);
    rt.unmodeled.push(msg);
}

fn mk_runtime(label: &str, spec: &str) -> Runtime {
    Runtime {
        label: label.to_string(),
        spec: spec.to_string(),
        feature_set: FeatureSet::default(),
        active_ids: BTreeSet::new(),
        rent: effects::GENESIS_RENT,
        epoch_schedule: EpochSchedule::custom(432_000, 432_000, false),
        programs: BTreeMap::new(),
        bundled: false,
        unmodeled: vec![],
        warnings: vec![],
        marinade_so: None,
    }
}

fn activate(rt: &mut Runtime, id: &str, slot: u64) -> bool {
    rt.active_ids.insert(id.to_string());
    let a = addr(id);
    if FEATURE_NAMES.contains_key(&a) {
        rt.feature_set.activate(&a, slot);
        true
    } else {
        false
    }
}

fn deactivate(rt: &mut Runtime, id: &str) {
    rt.active_ids.remove(id);
    let a = addr(id);
    if FEATURE_NAMES.contains_key(&a) {
        rt.feature_set.deactivate(&a);
    }
}

fn recompute_rent(rt: &mut Runtime) {
    let ids = rt.active_ids.clone();
    rt.rent = effects::rent_for(&|id| ids.contains(id));
}

/// Resolve a spec string into a runtime.
pub fn resolve(spec: &str) -> Result<Runtime> {
    let parsed = parse_spec(spec)?;
    let mut profile_dir: Option<PathBuf> = None;
    let profile: Option<ProfileJson>;
    let mut rt;
    if parsed.base == "builtin" {
        profile = None;
        rt = mk_runtime("builtin", spec);
        rt.bundled = true;
        rt.feature_set = litesvm::LiteSVM::mainnet_feature_set();
        rt.active_ids = rt.feature_set.active().keys().map(|k| k.to_string()).collect();
        recompute_rent(&mut rt);
    } else {
        let name = parsed.base.clone();
        let path = if name.ends_with(".json") {
            find_file(Path::new(&name), None)?
        } else {
            find_file(&Path::new("profiles").join(format!("{name}.json")), None)
                .or_else(|_| find_file(Path::new(&name), None))?
        };
        profile_dir = path.parent().map(Path::to_path_buf);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let p: ProfileJson = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if p.schema != 1 {
            bail!("unsupported profile schema {} in {}", p.schema, path.display());
        }
        rt = mk_runtime(if p.name.is_empty() { &name } else { &p.name }, spec);
        for (id, f) in &p.features {
            match f.status.as_str() {
                "active" => {
                    if !activate(&mut rt, id, f.activated_at.unwrap_or(0)) {
                        unmodeled(
                            &mut rt,
                            format!(
                                "feature {id} ({}) is active on the cluster but unknown to agave-feature-set 4.3.2",
                                f.module.clone().unwrap_or_else(|| f.name.clone())
                            ),
                        );
                    }
                }
                _ => {}
            }
        }
        if let Some(r) = &p.sysvars.rent {
            rt.rent = effects::RentParams {
                lamports_per_byte_year: r.lamports_per_byte_year,
                exemption_threshold: r.exemption_threshold,
                burn_percent: r.burn_percent,
            };
        }
        if let Some(e) = &p.sysvars.epoch_schedule {
            rt.epoch_schedule = EpochSchedule {
                slots_per_epoch: e.slots_per_epoch,
                leader_schedule_slot_offset: e.leader_schedule_slot_offset,
                warmup: e.warmup,
                first_normal_epoch: e.first_normal_epoch,
                first_normal_slot: e.first_normal_slot,
            };
        }
        // consistency of the profile's Rent with the feature model
        let ids = rt.active_ids.clone();
        let modeled = effects::rent_for(&|id| ids.contains(id));
        if modeled != rt.rent {
            let msg = format!(
                "profile Rent {:?} differs from effects::rent_for(active features) {:?}; using the profile value",
                rt.rent, modeled
            );
            warn(&mut rt, msg);
        }
        // program binaries
        for (id, pe) in &p.programs {
            if pe.builtin || pe.absent || pe.file.is_empty() {
                continue;
            }
            let file = path.parent().unwrap().join(&pe.file);
            let bytes = std::fs::read(&file).with_context(|| format!("reading program {}", file.display()))?;
            let bin = ProgramBin::new(bytes, loader_for(&pe.owner), file.display().to_string());
            if !pe.sha256.is_empty() && pe.sha256 != bin.sha256 {
                bail!("sha256 mismatch for {} ({}): profile says {}, file is {}", id, pe.name, pe.sha256, bin.sha256);
            }
            if id == &crate::marinade::PROGRAM_ID_STR.to_string() {
                rt.marinade_so = Some(bin);
            } else {
                rt.programs.insert(addr(id), bin);
            }
        }
        profile = Some(p);
    }
    // name resolution can also use the mainnet profile for `builtin`
    let fallback: Option<ProfileJson> = if profile.is_none() {
        find_file(Path::new("profiles/mainnet.json"), None)
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
    } else {
        None
    };
    let resolver: Option<&ProfileJson> = profile.as_ref().or(fallback.as_ref());

    for op in &parsed.ops {
        match op {
            Op::Rent(n) => {
                rt.rent.lamports_per_byte_year = *n;
                rt.rent.exemption_threshold = 1.0;
            }
            Op::Program(id, file) => {
                let f = find_file(file, profile_dir.as_deref())?;
                let a = addr(id);
                let loader = rt.programs.get(&a).map(|p| p.loader).unwrap_or_else(|| addr(BPF_LOADER_UPGRADEABLE));
                let bytes = std::fs::read(&f)?;
                rt.programs.insert(a, ProgramBin::new(bytes, loader, f.display().to_string()));
            }
            Op::Enable(name) | Op::Disable(name) => {
                let enable = matches!(op, Op::Enable(_));
                let id = id_of_name(resolver, name)
                    .ok_or_else(|| anyhow!("unknown feature {name:?} (not a base58 id nor a known module name)"))?;
                let label = module_of(resolver, &id).unwrap_or_else(|| id.clone());
                if enable {
                    if !activate(&mut rt, &id, 0) {
                        let modeled = effects::affects_rent(&id)
                            || effects::PROGRAM_SWAPS.iter().any(|s| s.feature_id == id);
                        let what = if modeled {
                            "only its modeled activation effects (rent / program swap) are applied"
                        } else {
                            "nothing simulated"
                        };
                        unmodeled(&mut rt, format!("+{label} ({id}): unknown to agave-feature-set 4.3.2, {what}"));
                    }
                } else {
                    deactivate(&mut rt, &id);
                }
                if effects::affects_rent(&id) {
                    recompute_rent(&mut rt);
                }
                for sw in effects::PROGRAM_SWAPS.iter().filter(|s| s.feature_id == id) {
                    let pid = addr(sw.program_id);
                    if enable {
                        let buf = resolver.and_then(|p| p.buffers.values().find(|b| b.for_feature == id));
                        match (buf, &profile) {
                            (Some(b), Some(_)) if !b.file.is_empty() => {
                                let file = profile_dir.as_ref().unwrap().join(&b.file);
                                let bytes = std::fs::read(&file)?;
                                let loader = rt.programs.get(&pid).map(|p| p.loader).unwrap_or_else(|| addr(BPF_LOADER_UPGRADEABLE));
                                rt.programs.insert(pid, ProgramBin::new(bytes, loader, format!("buffer for {label}")));
                            }
                            _ if rt.bundled && sw.feature_module == "replace_spl_token_with_p_token" => {
                                // the engine's bundled programs follow this feature natively
                            }
                            _ => unmodeled(
                                &mut rt,
                                format!("+{label}: no buffer binary for {} in the profile, program swap ({:?}) not applied", sw.program_id, sw.kind),
                            ),
                        }
                    } else if !(rt.bundled && sw.feature_module == "replace_spl_token_with_p_token") {
                        unmodeled(
                            &mut rt,
                            format!("-{label}: program swap of {} is not reverted (previous binary unknown)", sw.program_id),
                        );
                    }
                }
                for u in effects::UNMODELED.iter().filter(|u| u.feature_id == id) {
                    unmodeled(&mut rt, format!("{label}: {}", u.note));
                }
            }
        }
    }
    // label
    rt.label = spec.to_string();
    Ok(rt)
}

/// `HARNESS_BASE` / `HARNESS_TARGET` resolution (ARCHITECTURE.md).
pub fn base_spec() -> String {
    std::env::var("HARNESS_BASE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        if crate::tests_dir().join("profiles/mainnet.json").exists() {
            "profiles/mainnet.json".into()
        } else {
            "builtin".into()
        }
    })
}

pub fn target_spec() -> String {
    std::env::var("HARNESS_TARGET").ok().filter(|s| !s.is_empty()).unwrap_or_else(base_spec)
}

/// The binary `MARINADE_SO=profile` stands for: the Marinade binary inside the BASE profile (`HARNESS_BASE`, the
/// cluster the tests start on: the binary live on mainnet), whatever the TARGET runtime is. A base without one
/// (`builtin`, a profile that does not carry Marinade) falls back to the committed mainnet profile.
pub fn live_binary() -> Result<ProgramBin> {
    static LIVE: std::sync::OnceLock<std::result::Result<ProgramBin, String>> = std::sync::OnceLock::new();
    LIVE.get_or_init(|| {
        let load = |spec: String| resolve(&spec).ok().and_then(|rt| rt.marinade_so);
        load(base_spec())
            .or_else(|| load(crate::tests_dir().join("profiles/mainnet.json").display().to_string()))
            .ok_or_else(|| "MARINADE_SO=profile but neither the base runtime nor the committed mainnet profile has a Marinade binary".to_string())
    })
    .clone()
    .map_err(|e| anyhow!(e))
}

/// Path of the Marinade `.so` under test: `MARINADE_SO` (default `<repo>/target/deploy/marinade_finance.so`);
/// the value `profile` means the binary inside the base profile.
pub fn marinade_so(base: &Runtime) -> Result<ProgramBin> {
    let v = std::env::var("MARINADE_SO").ok().filter(|s| !s.is_empty());
    if v.as_deref() == Some("profile") {
        let _ = base;
        return live_binary();
    }
    let path = match v {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() { p } else { find_file(&p, None)? }
        }
        None => crate::repo_root().join("target/deploy/marinade_finance.so"),
    };
    let bytes = std::fs::read(&path).with_context(|| format!("reading Marinade binary {}", path.display()))?;
    Ok(ProgramBin::new(bytes, addr(BPF_LOADER_UPGRADEABLE), path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_tokens() {
        let s = parse_spec("builtin,+a,-b,rent=5080,program:Stake11111111111111111111111111111111111111=x.so").unwrap();
        assert_eq!(s.base, "builtin");
        assert_eq!(s.ops.len(), 4);
        assert_eq!(s.ops[0], Op::Enable("a".into()));
        assert_eq!(s.ops[1], Op::Disable("b".into()));
        assert_eq!(s.ops[2], Op::Rent(5080));
        assert!(matches!(&s.ops[3], Op::Program(id, f) if id.starts_with("Stake") && f == Path::new("x.so")));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_spec("").is_err());
        assert!(parse_spec("builtin,wat").is_err());
        assert!(parse_spec("builtin,rent=abc").is_err());
        assert!(parse_spec("builtin,program:x").is_err());
    }

    #[test]
    fn builtin_is_mainnet_rent() {
        let rt = resolve("builtin").unwrap();
        assert_eq!(rt.rent.lamports_per_byte_year, 5080);
        assert_eq!(rt.stake_rent(), 1_666_240);
    }

    #[test]
    fn builtin_toggles_recompute_rent() {
        let rt = resolve("builtin,-set_lamports_per_byte_to_6333,-set_lamports_per_byte_to_5080").unwrap();
        assert_eq!(rt.stake_rent(), 2_282_880, "{:?}", rt.rent);
        let rt = resolve("builtin,+set_lamports_per_byte_to_2575").unwrap();
        assert_eq!(rt.rent.lamports_per_byte_year, 2575);
        let rt = resolve("builtin,rent=696").unwrap();
        assert_eq!(rt.stake_rent(), 328 * 696);
        assert!(resolve("builtin,+no_such_feature").is_err());
    }

    #[test]
    fn profile_json_reports_unmodeled() {
        if !crate::tests_dir().join("profiles/testnet.json").exists() {
            return;
        }
        let rt = resolve("profiles/testnet.json").unwrap();
        assert!(
            rt.unmodeled.iter().any(|u| u.contains("RMsTKfD6hZnBhhNvgGBeKNrqCNkeoP3DYYxNtcuWtRg")),
            "remove_inactive_stakes must be reported as unmodeled: {:?}",
            rt.unmodeled
        );
        assert!(rt.programs.contains_key(&addr(STAKE_ID)));
    }

    #[test]
    fn module_toggles_on_profile() {
        if !crate::tests_dir().join("profiles/mainnet.json").exists() {
            return;
        }
        let rt = resolve("profiles/mainnet.json,+virtual_address_space_adjustments,+account_data_direct_mapping,+direct_account_pointers_in_program_input,+set_lamports_per_byte_to_2575").unwrap();
        assert_eq!(rt.rent.lamports_per_byte_year, 2575);
        let text = std::fs::read_to_string(crate::tests_dir().join("profiles/mainnet.json")).unwrap();
        let p: ProfileJson = serde_json::from_str(&text).unwrap();
        for m in ["virtual_address_space_adjustments", "account_data_direct_mapping", "direct_account_pointers_in_program_input"] {
            let id = id_of_name(Some(&p), m).unwrap();
            assert!(rt.feature_set.is_active(&addr(&id)), "{m} must be active in the engine feature set");
            assert!(rt.unmodeled.iter().all(|u| !u.contains(&id)), "{m} is known to the engine");
        }
        let rt2 = resolve("builtin,+virtual_address_space_adjustments").unwrap();
        assert!(rt2.active_ids.len() > resolve("builtin").unwrap().active_ids.len() - 1);
    }
}
