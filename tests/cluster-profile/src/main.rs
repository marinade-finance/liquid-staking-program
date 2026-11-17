mod agave;
mod buffers;
mod diff;
mod elf;
mod model;
mod rpc;
mod snapshot;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "cluster-profile", about = "Snapshot and diff Solana cluster runtime profiles")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Snapshot a cluster: features, sysvars, core programs, pending-upgrade buffers.
    Snapshot {
        #[arg(long)]
        cluster: String,
        /// Override the default public RPC endpoint.
        #[arg(long)]
        rpc: Option<String>,
        /// Profile name (default: the cluster name).
        #[arg(long)]
        name: Option<String>,
        /// Output directory (writes <out>/<name>.json and <out>/programs/<sha256>.so).
        #[arg(long)]
        out: PathBuf,
        /// Local agave feature-set/src/lib.rs (repeatable, `label=path` or `path`); disables downloads.
        #[arg(long = "agave-src")]
        agave_src: Vec<String>,
        /// Cache dir for downloaded agave sources.
        #[arg(long)]
        cache_dir: Option<PathBuf>,
        /// Extra pending-upgrade buffer: <feature>:<program>:<buffer> (repeatable).
        #[arg(long)]
        buffer: Vec<String>,
    },
    /// Markdown report of what differs between a base profile and others.
    Diff {
        base: PathBuf,
        others: Vec<PathBuf>,
        /// Also write the machine-readable report here.
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// One line per upcoming feature (`<id> <module>`), sorted, no header.
    ListUpcoming { base: PathBuf, others: Vec<PathBuf> },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Snapshot { cluster, rpc, name, out, agave_src, cache_dir, buffer } => {
            snapshot::run(snapshot::Args { cluster, rpc, name, out, agave_src, cache_dir, buffer })?;
        }
        Cmd::Diff { base, others, json } => diff::run(&base, &others, json.as_deref())?,
        Cmd::ListUpcoming { base, others } => diff::list_upcoming(&base, &others)?,
    }
    Ok(())
}
