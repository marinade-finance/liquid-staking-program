use crate::buffers::{self, BufferSpec};
use crate::model::*;
use crate::{agave, elf, rpc::*};
use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const PROGRAMS: &[(&str, &str)] = &[
    ("Stake11111111111111111111111111111111111111", "stake"),
    ("Vote111111111111111111111111111111111111111", "vote"),
    ("11111111111111111111111111111111", "system"),
    ("AddressLookupTab1e1111111111111111111111111", "address-lookup-table"),
    ("Config1111111111111111111111111111111111111", "config"),
    ("Feature111111111111111111111111111111111111", "feature-gate"),
    ("BPFLoaderUpgradeab1e11111111111111111111111", "bpf-loader-upgradeable"),
    ("BPFLoader2111111111111111111111111111111111", "bpf-loader"),
    ("LoaderV411111111111111111111111111111111111", "loader-v4"),
    ("ComputeBudget111111111111111111111111111111", "compute-budget"),
    ("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", "spl-token"),
    ("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb", "spl-token-2022"),
    ("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL", "spl-associated-token-account"),
    ("MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD", "marinade-finance"),
];

const NATIVE_LOADER: &str = "NativeLoader1111111111111111111111111111111";
const LOADER_V3: &str = "BPFLoaderUpgradeab1e11111111111111111111111";
const LOADER_V2: &str = "BPFLoader2111111111111111111111111111111111";
const LOADER_V1: &str = "BPFLoader1111111111111111111111111111111111";
const LOADER_V4: &str = "LoaderV411111111111111111111111111111111111";
const RENT: &str = "SysvarRent111111111111111111111111111111111";
const EPOCH_SCHEDULE: &str = "SysvarEpochSchedu1e111111111111111111111111";

pub fn sha_hex(d: &[u8]) -> String {
    Sha256::digest(d).iter().map(|b| format!("{b:02x}")).collect()
}

fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

/// bincode `Option<u64>`: tag byte + u64 LE.
pub fn parse_feature(data: Option<&[u8]>) -> (String, Option<u64>) {
    match data {
        None => ("inactive".into(), None),
        Some(d) => match d.first() {
            Some(1) => match u64_at(d, 1) {
                Some(s) => ("active".into(), Some(s)),
                None => ("pending".into(), None),
            },
            _ => ("pending".into(), None),
        },
    }
}

pub fn parse_rent(d: &[u8]) -> Option<Rent> {
    if d.len() < 17 {
        return None;
    }
    Some(Rent {
        lamports_per_byte_year: u64_at(d, 0)?,
        exemption_threshold: f64::from_le_bytes(d[8..16].try_into().ok()?),
        burn_percent: d[16],
    })
}

pub fn parse_epoch_schedule(d: &[u8]) -> Option<EpochSchedule> {
    if d.len() < 33 {
        return None;
    }
    Some(EpochSchedule {
        slots_per_epoch: u64_at(d, 0)?,
        leader_schedule_slot_offset: u64_at(d, 8)?,
        warmup: d[16] != 0,
        first_normal_epoch: u64_at(d, 17)?,
        first_normal_slot: u64_at(d, 25)?,
    })
}

fn b58(b: &[u8]) -> String {
    bs58::encode(b).into_string()
}

struct Store {
    dir: PathBuf,
}

impl Store {
    /// Cuts the ELF, writes `programs/<sha>.so`, returns (sha, len, relative file).
    fn put(&self, data: &[u8], what: &str) -> Result<(String, u64, String)> {
        let (elf, is_elf) = elf::cut(data);
        if !is_elf {
            eprintln!("warning: {what}: not an ELF, storing full {} bytes", data.len());
        }
        let sha = sha_hex(elf);
        let rel = format!("programs/{sha}.so");
        let p = self.dir.join(&rel);
        if !p.exists() {
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(&p, elf).with_context(|| format!("writing {}", p.display()))?;
        }
        Ok((sha, elf.len() as u64, rel))
    }
}

pub struct Args {
    pub cluster: String,
    pub rpc: Option<String>,
    pub name: Option<String>,
    pub out: PathBuf,
    pub agave_src: Vec<String>,
    pub cache_dir: Option<PathBuf>,
    pub buffer: Vec<String>,
}

pub fn run(a: Args) -> Result<PathBuf> {
    let (cluster_id, default_rpc) = match a.cluster.as_str() {
        "mainnet" | "mainnet-beta" => ("mainnet-beta", "https://api.mainnet-beta.solana.com"),
        "testnet" => ("testnet", "https://api.testnet.solana.com"),
        "devnet" => ("devnet", "https://api.devnet.solana.com"),
        o => anyhow::bail!("unknown cluster {o} (mainnet|testnet|devnet)"),
    };
    let rpc_url = a.rpc.clone().unwrap_or_else(|| default_rpc.to_string());
    let name = a.name.clone().unwrap_or_else(|| a.cluster.trim_end_matches("-beta").to_string());
    let rpc = Rpc::new(&rpc_url)?;
    let store = Store { dir: a.out.clone() };
    std::fs::create_dir_all(&a.out)?;

    eprintln!("[{name}] version / epoch");
    let ver = rpc.get_version()?;
    let ei = rpc.get_epoch_info()?;
    let core = ver["solana-core"].as_str().unwrap_or_default().to_string();

    eprintln!("[{name}] agave sources (solana-core {core})");
    let cache = a.cache_dir.clone().unwrap_or_else(agave::default_cache_dir);
    let sources = agave::load_sources(&a.agave_src, &cache, &[core.clone()])?;
    anyhow::ensure!(!sources.is_empty(), "no agave feature-set source available");

    // union of features; description/module from the first source (master first) that has it
    let mut defs: BTreeMap<String, (agave::FeatureDef, Vec<String>)> = BTreeMap::new();
    for s in &sources {
        for (id, d) in &s.features {
            defs.entry(id.clone()).or_insert_with(|| (d.clone(), vec![])).1.push(s.label.clone());
        }
    }
    eprintln!("[{name}] {} features in universe, fetching accounts", defs.len());
    let ids: Vec<String> = defs.keys().cloned().collect();
    let accts = rpc.get_multiple(&ids)?;
    let mut features = BTreeMap::new();
    for (id, acc) in ids.iter().zip(accts) {
        let (status, at) = parse_feature(acc.as_ref().map(|a| a.data.as_slice()));
        let (d, srcs) = &defs[id];
        features.insert(
            id.clone(),
            Feature {
                name: d.description.clone(),
                module: Some(d.module.clone()),
                sources: srcs.clone(),
                status,
                activated_at: at,
            },
        );
    }

    // programs + sysvars + buffers
    let specs: Vec<BufferSpec> = buffers::all(&a.buffer)?;
    let mut keys: Vec<String> = PROGRAMS.iter().map(|p| p.0.to_string()).collect();
    keys.push(RENT.into());
    keys.push(EPOCH_SCHEDULE.into());
    let nprog = PROGRAMS.len();
    for b in &specs {
        keys.push(b.buffer.clone());
    }
    eprintln!("[{name}] programs, sysvars, buffers");
    let accs = rpc.get_multiple(&keys)?;

    let rent = accs[nprog].as_ref().and_then(|a| parse_rent(&a.data));
    let es = accs[nprog + 1].as_ref().and_then(|a| parse_epoch_schedule(&a.data));
    anyhow::ensure!(rent.is_some(), "rent sysvar missing or malformed");

    // collect programdata addresses
    let mut pd_addr: Vec<Option<String>> = vec![None; nprog];
    for i in 0..nprog {
        if let Some(acc) = &accs[i] {
            if acc.owner == LOADER_V3 && acc.data.len() >= 36 && acc.data[0..4] == [2, 0, 0, 0] {
                pd_addr[i] = Some(b58(&acc.data[4..36]));
            }
        }
    }
    let pd_keys: Vec<String> = pd_addr.iter().flatten().cloned().collect();
    let pd_accs = if pd_keys.is_empty() { vec![] } else { rpc.get_multiple(&pd_keys)? };
    let pd_map: BTreeMap<&String, &Option<Account>> = pd_keys.iter().zip(pd_accs.iter()).collect();

    let mut programs = BTreeMap::new();
    for (i, (id, pname)) in PROGRAMS.iter().enumerate() {
        let mut p = Program { name: pname.to_string(), ..Default::default() };
        match &accs[i] {
            None => p.absent = Some(true),
            Some(acc) => {
                p.owner = Some(acc.owner.clone());
                if acc.owner == NATIVE_LOADER {
                    p.builtin = Some(true);
                } else if acc.owner == LOADER_V3 {
                    match pd_addr[i].as_ref().and_then(|k| pd_map.get(k)).and_then(|o| o.as_ref()) {
                        Some(pd) if pd.data.len() >= 45 && pd.data[0..4] == [3, 0, 0, 0] => {
                            p.programdata = pd_addr[i].clone();
                            p.deployed_slot = u64_at(&pd.data, 4);
                            eprintln!("  {pname}: programdata {} bytes", pd.data.len());
                            let (sha, len, file) = store.put(&pd.data[45..], pname)?;
                            p.sha256 = Some(sha);
                            p.len = Some(len);
                            p.file = Some(file);
                        }
                        _ => {
                            eprintln!("warning: {pname}: programdata missing or malformed");
                            p.absent = Some(true);
                        }
                    }
                } else if acc.owner == LOADER_V2 || acc.owner == LOADER_V1 {
                    let (sha, len, file) = store.put(&acc.data, pname)?;
                    p.sha256 = Some(sha);
                    p.len = Some(len);
                    p.file = Some(file);
                } else if acc.owner == LOADER_V4 && acc.data.len() >= 48 {
                    p.deployed_slot = u64_at(&acc.data, 0);
                    let (sha, len, file) = store.put(&acc.data[48..], pname)?;
                    p.sha256 = Some(sha);
                    p.len = Some(len);
                    p.file = Some(file);
                } else {
                    eprintln!("warning: {pname}: unhandled owner {}", acc.owner);
                }
            }
        }
        programs.insert(id.to_string(), p);
    }

    let mut bufs = BTreeMap::new();
    for (j, b) in specs.iter().enumerate() {
        let Some(acc) = &accs[nprog + 2 + j] else { continue };
        if acc.owner == LOADER_V3 && acc.data.len() >= 37 && acc.data[0..4] == [1, 0, 0, 0] {
            let (sha, len, file) = store.put(&acc.data[37..], &b.buffer)?;
            bufs.insert(
                b.buffer.clone(),
                Buffer {
                    for_feature: b.feature.clone(),
                    module: b.module.clone(),
                    target_program: b.program.clone(),
                    sha256: sha,
                    len,
                    file,
                },
            );
        } else {
            eprintln!("warning: buffer {} exists but is not a loader-v3 buffer account", b.buffer);
        }
    }

    let profile = Profile {
        schema: 1,
        name: name.clone(),
        cluster: cluster_id.into(),
        rpc: rpc_url,
        fetched_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        slot: ei["absoluteSlot"].as_u64().ok_or_else(|| anyhow!("getEpochInfo: no absoluteSlot"))?,
        epoch: ei["epoch"].as_u64().unwrap_or(0),
        version: Version { solana_core: core, feature_set: ver["feature-set"].as_u64() },
        features,
        sysvars: Sysvars { rent, epoch_schedule: es },
        programs,
        buffers: bufs,
    };
    let path = a.out.join(format!("{name}.json"));
    write_json(&path, &profile)?;
    let active = profile.features.values().filter(|f| f.status == "active").count();
    let pending = profile.features.values().filter(|f| f.status == "pending").count();
    eprintln!(
        "[{name}] wrote {} ({} features: {active} active, {pending} pending; {} buffers)",
        path.display(),
        profile.features.len(),
        profile.buffers.len()
    );
    Ok(path)
}

pub fn write_json<T: serde::Serialize>(path: &Path, v: &T) -> Result<()> {
    let mut s = serde_json::to_string_pretty(v)?;
    s.push('\n');
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_accounts() {
        assert_eq!(parse_feature(None), ("inactive".to_string(), None));
        assert_eq!(parse_feature(Some(&[0, 0, 0, 0, 0, 0, 0, 0, 0])), ("pending".to_string(), None));
        let mut d = vec![1u8];
        d.extend(123456789u64.to_le_bytes());
        assert_eq!(parse_feature(Some(&d)), ("active".to_string(), Some(123456789)));
    }

    #[test]
    fn rent_mainnet() {
        use base64::Engine;
        let d = base64::engine::general_purpose::STANDARD.decode("2BMAAAAAAAAAAAAAAADwPzI=").unwrap();
        assert_eq!(
            parse_rent(&d),
            Some(Rent { lamports_per_byte_year: 5080, exemption_threshold: 1.0, burn_percent: 50 })
        );
    }

    #[test]
    fn epoch_schedule() {
        let mut d = vec![];
        d.extend(432000u64.to_le_bytes());
        d.extend(432000u64.to_le_bytes());
        d.push(0);
        d.extend(0u64.to_le_bytes());
        d.extend(0u64.to_le_bytes());
        let e = parse_epoch_schedule(&d).unwrap();
        assert_eq!(e.slots_per_epoch, 432000);
        assert!(!e.warmup);
    }
}
