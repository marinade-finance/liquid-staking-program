//! Parse agave's `feature-set/src/lib.rs` (no dependency on agave crates).
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureDef {
    pub id: String,
    pub module: String,
    pub description: String,
}

/// Features (only ids that appear in the FEATURE_NAMES map) declared by one agave source file,
/// keyed by id.
pub fn parse_source(src: &str) -> BTreeMap<String, FeatureDef> {
    let decl = Regex::new(r#"declare_id!\(\s*"([1-9A-HJ-NP-Za-km-z]{32,44})"\s*\)"#).unwrap();
    let modre = Regex::new(r"(?:pub(?:\([a-z]+\))?\s+)?mod\s+(\w+)\s*\{").unwrap();
    let strre = Regex::new(r#""(?:[^"\\]|\\.)*""#).unwrap();

    // path (joined by ::) -> id
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut stack: Vec<Option<String>> = Vec::new();
    for raw in src.lines() {
        let line = raw.trim();
        if line.starts_with("//") {
            continue;
        }
        let path_now = |stack: &Vec<Option<String>>| -> String {
            stack.iter().flatten().cloned().collect::<Vec<_>>().join("::")
        };
        if let Some(c) = decl.captures(line) {
            let p = path_now(&stack);
            if !p.is_empty() {
                ids.insert(p, c[1].to_string());
            }
        }
        // strip strings and trailing comments, then walk braces in order
        let code = strre.replace_all(line, "\"\"").to_string();
        let code = code.split("//").next().unwrap_or("").to_string();
        let mods: Vec<(usize, String)> =
            modre.captures_iter(&code).map(|c| (c.get(0).unwrap().end() - 1, c[1].to_string())).collect();
        for (i, ch) in code.char_indices() {
            match ch {
                '{' => {
                    let name = mods.iter().find(|(pos, _)| *pos == i).map(|(_, n)| n.clone());
                    stack.push(name);
                }
                '}' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }

    // FEATURE_NAMES block
    let mut out = BTreeMap::new();
    let Some(start) = src.find("static FEATURE_NAMES") else { return out };
    let rest = &src[start..];
    let end = rest.find(".into_iter()").unwrap_or(rest.len());
    let block = &rest[..end];
    let ent = Regex::new(r#"\(\s*([\w:]+)::id\(\)\s*,\s*"((?:[^"\\]|\\.)*)"\s*,?\s*\)"#).unwrap();
    for c in ent.captures_iter(block) {
        let module = c[1].to_string();
        let Some(id) = ids.get(&module) else { continue };
        out.insert(
            id.clone(),
            FeatureDef { id: id.clone(), module, description: c[2].replace("\\\"", "\"").replace("\\\\", "\\") },
        );
    }
    out
}

pub struct Source {
    pub label: String,
    pub features: BTreeMap<String, FeatureDef>,
}

pub fn default_cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("cluster-profile")
}

fn url_for(r#ref: &str) -> String {
    format!("https://raw.githubusercontent.com/anza-xyz/agave/{}/feature-set/src/lib.rs", r#ref)
}

/// Fetch (with cache) agave's feature-set source at `git_ref`. Ok(None) = ref does not exist.
/// Branches (master, v4.5) are cached for a few hours, tags forever.
pub fn fetch_ref(cache: &Path, git_ref: &str, mutable: bool) -> Result<Option<String>> {
    std::fs::create_dir_all(cache)?;
    let f = cache.join(format!("agave-feature-set-{}.rs", git_ref.replace('/', "_")));
    if let Ok(md) = std::fs::metadata(&f) {
        let fresh = !mutable
            || md.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok())
                < Some(Duration::from_secs(6 * 3600));
        if fresh {
            return Ok(Some(std::fs::read_to_string(&f)?));
        }
    }
    match crate::rpc::http_get(&url_for(git_ref)) {
        Ok(Some(body)) => {
            std::fs::write(&f, &body)?;
            Ok(Some(body))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            if f.exists() {
                eprintln!("warning: {e:#}; using stale cache for {git_ref}");
                return Ok(Some(std::fs::read_to_string(&f)?));
            }
            Err(e)
        }
    }
}

/// Candidate refs for a `solana-core` version such as "4.4.0" or "4.5.0-beta.0".
pub fn candidate_refs(version: &str) -> Vec<(String, bool)> {
    let mut v = vec![(format!("v{version}"), false)];
    let base = version.split('-').next().unwrap_or(version);
    if base != version {
        v.push((format!("v{base}"), false));
    }
    let mut parts = base.split('.');
    if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
        v.push((format!("v{a}.{b}"), true)); // release branch
    }
    v
}

pub fn load_sources(
    local: &[String],
    cache: &Path,
    versions: &[String],
) -> Result<Vec<Source>> {
    let mut out: Vec<Source> = Vec::new();
    if !local.is_empty() {
        for l in local {
            let (label, path) = match l.split_once('=') {
                Some((a, b)) => (a.to_string(), b.to_string()),
                None => (
                    Path::new(l).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(l.clone()),
                    l.clone(),
                ),
            };
            let s = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
            out.push(Source { label, features: parse_source(&s) });
        }
        return Ok(out);
    }
    match fetch_ref(cache, "master", true)? {
        Some(s) => out.push(Source { label: "master".into(), features: parse_source(&s) }),
        None => eprintln!("warning: agave master feature-set source not found"),
    }
    let mut seen = std::collections::BTreeSet::new();
    for ver in versions {
        if !seen.insert(ver.clone()) {
            continue;
        }
        let mut found = false;
        for (r, mutable) in candidate_refs(ver) {
            if out.iter().any(|s| s.label == r) {
                found = true;
                break;
            }
            match fetch_ref(cache, &r, mutable) {
                Ok(Some(s)) => {
                    out.push(Source { label: r, features: parse_source(&s) });
                    found = true;
                    break;
                }
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("warning: fetching agave {r}: {e:#}");
                    continue;
                }
            }
        }
        if !found {
            eprintln!("warning: no agave source found for solana-core {ver}; continuing without it");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
use {solana_pubkey::Pubkey};

pub mod pico_inflation {
    solana_pubkey::declare_id!("4RWNif6C2WCNiKVW7otP4G7dkmkHGyKQWRpuZ1pxKU5m");
}

pub mod full_inflation {
    pub mod devnet_and_testnet {
        solana_pubkey::declare_id!("DT4n6ABDqs6w4bnfwrXT9rsprcPf6cdDga1egctaPkLC");
    }
    pub mod mainnet {
        pub mod certusone {
            pub mod vote {
                solana_pubkey::declare_id!("BzBBveUDymEYoYzcMWNQCx3cd4jQs7puaVFHLtsbB6fm");
            }
        }
    }
}

pub mod vote_state_v4 {
    solana_pubkey::declare_id!("Gx4XFcrVMt4HUvPzTpTSVkdDVgcDSjKhDN1RqRS6KDuZ");

    pub mod stake_program_buffer {
        solana_pubkey::declare_id!("BM11F4hqrpinQs28sEZfzQ2fYddivYs4NEAHF6QMjkJF");
    }
}

pub static FEATURE_NAMES: LazyLock<AHashMap<Pubkey, &'static str>> = LazyLock::new(|| {
    [
        (pico_inflation::id(), "pico inflation {braces}"),
        (
            full_inflation::devnet_and_testnet::id(),
            "full inflation on devnet and testnet",
        ),
        (
            full_inflation::mainnet::certusone::vote::id(),
            "community vote",
        ),
        (vote_state_v4::id(), "vote state v4"),
    ]
    .into_iter()
    .collect()
});
"#;

    #[test]
    fn parses_nested_modules_and_skips_buffers() {
        let m = parse_source(FIXTURE);
        assert_eq!(m.len(), 4);
        let a = &m["DT4n6ABDqs6w4bnfwrXT9rsprcPf6cdDga1egctaPkLC"];
        assert_eq!(a.module, "full_inflation::devnet_and_testnet");
        assert_eq!(a.description, "full inflation on devnet and testnet");
        assert_eq!(m["BzBBveUDymEYoYzcMWNQCx3cd4jQs7puaVFHLtsbB6fm"].module, "full_inflation::mainnet::certusone::vote");
        assert_eq!(m["Gx4XFcrVMt4HUvPzTpTSVkdDVgcDSjKhDN1RqRS6KDuZ"].module, "vote_state_v4");
        assert_eq!(m["4RWNif6C2WCNiKVW7otP4G7dkmkHGyKQWRpuZ1pxKU5m"].description, "pico inflation {braces}");
        // buffer declared inside a feature module is not a feature
        assert!(!m.contains_key("BM11F4hqrpinQs28sEZfzQ2fYddivYs4NEAHF6QMjkJF"));
    }

    #[test]
    fn candidate_refs_for_beta() {
        let c: Vec<String> = candidate_refs("4.5.0-beta.0").into_iter().map(|x| x.0).collect();
        assert_eq!(c, vec!["v4.5.0-beta.0", "v4.5.0", "v4.5"]);
    }
}
