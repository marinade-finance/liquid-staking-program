//! Known pending core-BPF upgrade buffers. Trivially editable: add one `Known` per line.
//! More can be supplied at run time with `--buffer <feature>:<program>:<buffer>`.

pub struct Known {
    /// Feature gate id (base58) whose activation upgrades `program` from `buffer`.
    pub feature: &'static str,
    /// Agave module name of that feature (informational).
    pub module: &'static str,
    /// Program that gets replaced.
    pub program: &'static str,
    /// Buffer account holding the new ELF.
    pub buffer: &'static str,
}

pub const KNOWN: &[Known] = &[
    Known {
        feature: "Gx4XFcrVMt4HUvPzTpTSVkdDVgcDSjKhDN1RqRS6KDuZ",
        module: "vote_state_v4",
        program: "Stake11111111111111111111111111111111111111",
        buffer: "BM11F4hqrpinQs28sEZfzQ2fYddivYs4NEAHF6QMjkJF",
    },
    Known {
        feature: "ptokFjwyJtrwCa9Kgo9xoDS59V4QccBGEaRFnRPnSdP",
        module: "replace_spl_token_with_p_token",
        program: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        buffer: "ptok6rngomXrDbWf5v5Mkmu5CEbB51hzSCPDoj9DrvF",
    },
];

#[derive(Clone, Debug)]
pub struct BufferSpec {
    pub feature: String,
    pub module: String,
    pub program: String,
    pub buffer: String,
}

pub fn all(extra: &[String]) -> anyhow::Result<Vec<BufferSpec>> {
    let mut v: Vec<BufferSpec> = KNOWN
        .iter()
        .map(|k| BufferSpec {
            feature: k.feature.into(),
            module: k.module.into(),
            program: k.program.into(),
            buffer: k.buffer.into(),
        })
        .collect();
    for e in extra {
        let p: Vec<&str> = e.split(':').collect();
        anyhow::ensure!(p.len() == 3, "--buffer expects <feature>:<program>:<buffer>, got {e}");
        if v.iter().any(|b| b.buffer == p[2]) {
            continue;
        }
        v.push(BufferSpec { feature: p[0].into(), module: String::new(), program: p[1].into(), buffer: p[2].into() });
    }
    Ok(v)
}
