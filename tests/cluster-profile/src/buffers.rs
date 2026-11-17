//! Buffers of core-BPF upgrades/replacements, taken from the harness' effects model
//! (`harness/src/effects.rs`, PROGRAM_SWAPS) so both sides share one table.
//! More can be supplied at run time with `--buffer <feature>:<program>:<buffer>`.

#[derive(Clone, Debug)]
pub struct BufferSpec {
    pub feature: String,
    pub module: String,
    pub program: String,
    pub buffer: String,
}

pub fn all(extra: &[String]) -> anyhow::Result<Vec<BufferSpec>> {
    let mut v: Vec<BufferSpec> = crate::effects::PROGRAM_SWAPS
        .iter()
        .map(|k| BufferSpec {
            feature: k.feature_id.into(),
            module: k.feature_module.into(),
            program: k.program_id.into(),
            buffer: k.source_buffer.into(),
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
