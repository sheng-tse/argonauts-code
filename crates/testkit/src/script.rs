//! Fake programs written as shell scripts.

use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, ensure};

/// Writes `text` to `path` as an executable script. `sh` writes it, so no handle of this
/// process ever has it open for writing: on Linux a child that another test forks meanwhile
/// would hold that handle until it execs, and running the script then fails as busy.
pub fn write_script(path: &Path, text: &str) -> anyhow::Result<()> {
    let status = Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s' \"$2\" > \"$1\" && chmod 755 \"$1\"",
            "sh",
        ])
        .arg(path)
        .arg(text)
        .status()
        .context("running sh")?;
    ensure!(status.success(), "couldn't write {}", path.display());
    Ok(())
}
