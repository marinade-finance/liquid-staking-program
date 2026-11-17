use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Profile {
    pub schema: u32,
    pub name: String,
    pub cluster: String,
    pub rpc: String,
    pub fetched_at: String,
    pub slot: u64,
    pub epoch: u64,
    pub version: Version,
    pub features: BTreeMap<String, Feature>,
    pub sysvars: Sysvars,
    pub programs: BTreeMap<String, Program>,
    #[serde(default)]
    pub buffers: BTreeMap<String, Buffer>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Version {
    #[serde(rename = "solana-core", default)]
    pub solana_core: String,
    #[serde(rename = "feature-set", default)]
    pub feature_set: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Feature {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_at: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Rent {
    pub lamports_per_byte_year: u64,
    pub exemption_threshold: f64,
    pub burn_percent: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct EpochSchedule {
    pub slots_per_epoch: u64,
    pub leader_schedule_slot_offset: u64,
    pub warmup: bool,
    pub first_normal_epoch: u64,
    pub first_normal_slot: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Sysvars {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rent: Option<Rent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch_schedule: Option<EpochSchedule>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Program {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Upgradeable loader: programdata account address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub programdata: Option<String>,
    /// Upgradeable loader: slot of the last deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployed_slot: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Buffer {
    pub for_feature: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub module: String,
    pub target_program: String,
    pub sha256: String,
    pub len: u64,
    pub file: String,
}

pub fn status_label(f: &Feature) -> String {
    match (f.status.as_str(), f.activated_at) {
        ("active", Some(s)) => format!("active@{s}"),
        (s, _) => s.to_string(),
    }
}
