//! Finds the `argonauts` and fake agent binaries. Each comes from an environment variable, or
//! is built into the target directory the test runs from, so tests use the code under test.
//!
//! Each binary is built once per run, however many test processes the run starts: `cargo
//! test` starts one per test binary and nextest one per test. The first process to take the
//! lock beside a binary builds it and stamps it with the run's id, and the rest wait, read the
//! stamp and use the binary as it is.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context as _, anyhow, bail};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

pub const ARGONAUTS_BIN_ENV: &str = "ARGONAUTS_BIN";

pub const FAKE_CLI_BIN_ENV: &str = "ARGONAUTS_FAKE_CLI_BIN";

/// nextest's id for one run, the same in every test process it starts.
const RUN_ID_ENV: &str = "NEXTEST_RUN_ID";

/// The `argonauts` binary: `$ARGONAUTS_BIN`, or `argonauts-code` built once.
pub fn argonauts_bin() -> anyhow::Result<PathBuf> {
    static BUILT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    from_env_or_build(ARGONAUTS_BIN_ENV, &BUILT, "argonauts-code", "argonauts")
}

/// The fake agent: `$ARGONAUTS_FAKE_CLI_BIN`, or built once from this crate.
pub fn fake_cli_bin() -> anyhow::Result<PathBuf> {
    static BUILT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    from_env_or_build(
        FAKE_CLI_BIN_ENV,
        &BUILT,
        "argonauts-testkit",
        "argonauts-fake-cli",
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
    let path = profile_dir.join(bin);
    let cargo_build = || cargo_build(&profile_dir, package, bin);
    match run_id() {
        Some(run) => once_per_run(&profile_dir.join("test-bins"), &path, &run, cargo_build)?,
        None => cargo_build()?,
    }
    if !path.is_file() {
        bail!("cargo built {bin}, but {} isn't there", path.display());
    }
    Ok(path)
}

/// What every process of one run shares: nextest's run id, or else the `cargo test` that
/// started this process, by its pid and start time, so a later cargo given the same pid is a
/// new run. `None` for a test binary started any other way, which builds for itself.
fn run_id() -> Option<String> {
    if let Some(run) = std::env::var(RUN_ID_ENV).ok().filter(|run| !run.is_empty()) {
        return Some(run);
    }
    let parent = Pid::from_u32(std::os::unix::process::parent_id());
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[parent]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let cargo = system
        .process(parent)
        .filter(|process| process.name() == "cargo")?;
    Some(format!("cargo {parent} {}", cargo.start_time()))
}

/// Runs `build` unless this run has already built `binary`. The lock holds the run's other
/// processes while one builds, and the stamp names the run the binary was last built in. A
/// build that fails leaves the stamp as it was, so the next process tries again and reports
/// its own error.
fn once_per_run(
    stamps: &Path,
    binary: &Path,
    run: &str,
    build: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let name = binary
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", binary.display()))?
        .to_string_lossy();
    fs::create_dir_all(stamps).with_context(|| format!("creating {}", stamps.display()))?;
    let lock_path = stamps.join(format!("{name}.lock"));
    // Closing the file at the end of this function lets the next process in.
    let lock =
        File::create(&lock_path).with_context(|| format!("opening {}", lock_path.display()))?;
    lock.lock()
        .with_context(|| format!("locking {}", lock_path.display()))?;

    let stamp = stamps.join(format!("{name}.run"));
    let built_in_this_run = fs::read_to_string(&stamp).is_ok_and(|built_in| built_in == run);
    if built_in_this_run && binary.is_file() {
        return Ok(());
    }
    build()?;
    fs::write(&stamp, run).with_context(|| format!("writing {}", stamp.display()))
}

fn cargo_build(profile_dir: &Path, package: &str, bin: &str) -> anyhow::Result<()> {
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
        .args([
            "build",
            "--quiet",
            "--locked",
            "--package",
            package,
            "--bin",
            bin,
        ])
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
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// A target folder whose builds count themselves.
    struct Target {
        _dir: tempfile::TempDir,
        stamps: PathBuf,
        binary: PathBuf,
        builds: AtomicUsize,
    }

    impl Target {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            Self {
                stamps: dir.path().join("test-bins"),
                binary: dir.path().join("argonauts"),
                builds: AtomicUsize::new(0),
                _dir: dir,
            }
        }

        /// Asks for the binary in `run`. The build takes a moment, so a test asking at the
        /// same time arrives while it runs.
        fn ask(&self, run: &str) -> anyhow::Result<()> {
            once_per_run(&self.stamps, &self.binary, run, || {
                self.builds.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(50));
                fs::write(&self.binary, "built")?;
                Ok(())
            })
        }

        fn builds(&self) -> usize {
            self.builds.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn a_binary_is_built_once_however_many_tests_of_a_run_ask_at_once() {
        let target = Target::new();
        thread::scope(|scope| {
            let asking: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| target.ask("run-1")))
                .collect();
            for asked in asking {
                asked.join().unwrap().unwrap();
            }
        });
        assert_eq!(target.builds(), 1);
    }

    #[test]
    fn each_run_builds_the_binary_again() {
        let target = Target::new();
        for run in ["run-1", "run-1", "run-2", "run-1"] {
            target.ask(run).unwrap();
        }
        assert_eq!(target.builds(), 3);
    }

    #[test]
    fn a_binary_gone_since_its_run_built_it_is_built_again() {
        let target = Target::new();
        target.ask("run-1").unwrap();
        fs::remove_file(&target.binary).unwrap();
        target.ask("run-1").unwrap();
        assert_eq!(target.builds(), 2);
        assert!(target.binary.is_file());
    }

    #[test]
    fn a_failed_build_is_tried_again_by_the_next_test() {
        let target = Target::new();
        let failed = once_per_run(&target.stamps, &target.binary, "run-1", || {
            bail!("it doesn't compile")
        });
        assert_eq!(failed.unwrap_err().to_string(), "it doesn't compile");
        target.ask("run-1").unwrap();
        assert_eq!(target.builds(), 1);
    }
}
