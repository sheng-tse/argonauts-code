//! Finds the `argonauts` and fake vendor binaries. Each comes from an environment variable, or
//! is built once into the target directory the test runs from, so tests use the code under test.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context as _, anyhow, bail};

pub const ARGONAUTS_BIN_ENV: &str = "ARGONAUTS_BIN";

pub const FAKE_VENDOR_BIN_ENV: &str = "ARGONAUTS_FAKE_VENDOR_BIN";

/// The `argonauts` binary: `$ARGONAUTS_BIN`, or `argonauts-cli` built once.
pub fn argonauts_bin() -> anyhow::Result<PathBuf> {
    static BUILT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    from_env_or_build(ARGONAUTS_BIN_ENV, &BUILT, "argonauts-cli", "argonauts")
}

/// The fake vendor: `$ARGONAUTS_FAKE_VENDOR_BIN`, or built once from this crate.
pub fn fake_vendor_bin() -> anyhow::Result<PathBuf> {
    static BUILT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    from_env_or_build(
        FAKE_VENDOR_BIN_ENV,
        &BUILT,
        "argonauts-testkit",
        "argonauts-fake-vendor",
    )
}

fn from_env_or_build(
    env: &str,
    built: &OnceLock<Result<PathBuf, String>>,
    package: &str,
    bin: &str,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(env).filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    built
        .get_or_init(|| build(package, bin).map_err(|err| format!("{err:#}")))
        .clone()
        .map_err(|err| anyhow!(err))
}

fn build(package: &str, bin: &str) -> anyhow::Result<PathBuf> {
    let profile_dir = profile_dir()?;
    let target_dir = profile_dir
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", profile_dir.display()))?;
    let profile = profile_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "debug".to_owned());
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["build", "--quiet", "--package", package, "--bin", bin])
        .current_dir(workspace_root())
        .env("CARGO_TARGET_DIR", target_dir);
    match profile.as_str() {
        "debug" => {}
        "release" => {
            command.arg("--release");
        }
        other => {
            command.args(["--profile", other]);
        }
    }
    let output = command
        .output()
        .with_context(|| format!("running cargo build for {bin}"))?;
    if !output.status.success() {
        bail!(
            "cargo build for {bin} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let path = profile_dir.join(bin);
    if !path.is_file() {
        bail!("cargo built {bin}, but {} isn't there", path.display());
    }
    Ok(path)
}

/// `target/<profile>`, from where the test binary runs (`target/<profile>/deps/<test>`).
fn profile_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("finding the test binary")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", exe.display()))?;
    let dir = if dir
        .file_name()
        .is_some_and(|name| name == "deps" || name == "examples")
    {
        dir.parent().unwrap_or(dir)
    } else {
        dir
    };
    Ok(dir.to_owned())
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map_or_else(|| PathBuf::from("."), Path::to_owned)
}
