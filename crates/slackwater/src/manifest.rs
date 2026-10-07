//! Provenance rows appended to `data/MANIFEST.jsonl`.

use std::{
    io::Write,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Provenance {
    pub git_head: String,
    /// Uncommitted changes under `crates/` or `config/` when the run started.
    pub uncommitted: bool,
    pub config_sha256: String,
    pub fetched_at_unix: u64,
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    ensure!(
        out.status.success(),
        "{program} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// Read before a fetch starts, so that a missing `git` stops the run before
/// any request is made.
pub fn provenance(config_path: &Path) -> anyhow::Result<Provenance> {
    Ok(Provenance {
        git_head: run("git", &["rev-parse", "HEAD"])?,
        uncommitted: !run("git", &["status", "--porcelain", "--", "crates", "config"])?.is_empty(),
        config_sha256: sha256(config_path)?,
        fetched_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    })
}

pub fn sha256(path: &Path) -> anyhow::Result<String> {
    let name = path.to_str().context("path is not UTF-8")?;
    Ok(run("sha256sum", &[name])?
        .split_whitespace()
        .next()
        .context("sha256sum printed nothing")?
        .to_owned())
}

pub fn append<T: Serialize>(path: &Path, row: &T) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{}", serde_json::to_string(row)?)
        .with_context(|| format!("appending to {}", path.display()))
}
