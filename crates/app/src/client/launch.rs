//! Starting a run's run host: `argonauts host --repo <root> --run <id>`, detached in its
//! own session, stdin null, stdout and stderr to `run_host.log`.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Context as _;
use argonauts_core::RunId;
use argonauts_core::layout::run_files;

pub(crate) trait Launcher: Send + Sync {
    fn launch(&self, repo_root: &Path, run: &RunId, run_dir: &Path) -> anyhow::Result<()>;

    /// How the run host last launched for `run_dir` failed, such as "exited with status 1",
    /// once it has. `None` while it runs, and after it exits cleanly.
    fn failed(&self, _run_dir: &Path) -> Option<String> {
        None
    }

    /// Whether the run host last launched for `run_dir` exited cleanly, as one does that took
    /// what it was launched for, such as a stop, did it, and had nothing left to run.
    fn exited_cleanly(&self, _run_dir: &Path) -> bool {
        false
    }
}

/// Runs the `argonauts` binary. Tests can point it at a freshly built one.
pub(crate) struct ProcessLauncher {
    binary: PathBuf,
    /// How each run host this app launched last exited: `None` cleanly, else how it failed.
    exits: Arc<Mutex<HashMap<PathBuf, Option<String>>>>,
}

impl ProcessLauncher {
    /// This process's own binary.
    pub(crate) fn current() -> anyhow::Result<Self> {
        Ok(Self {
            binary: std::env::current_exe()?,
            exits: Arc::default(),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_binary(binary: PathBuf) -> Self {
        Self {
            binary,
            exits: Arc::default(),
        }
    }

    fn exits(&self) -> MutexGuard<'_, HashMap<PathBuf, Option<String>>> {
        self.exits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn command(&self, repo_root: &Path, run: &RunId) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .arg("host")
            .arg("--repo")
            .arg(repo_root)
            .arg("--run")
            .arg(run.as_str());
        command
    }
}

impl Launcher for ProcessLauncher {
    fn launch(&self, repo_root: &Path, run: &RunId, run_dir: &Path) -> anyhow::Result<()> {
        let log_path = run_dir.join(run_files::RUN_HOST_LOG);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .with_context(|| format!("opening {}", log_path.display()))?;
        let mut command = self.command(repo_root, run);
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        // SAFETY: setsid is async-signal-safe and touches no memory the parent shares. Its own
        // session keeps the run host running when the app's terminal closes.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid()
                    .map(drop)
                    .map_err(std::io::Error::from)
            });
        }
        self.exits().remove(run_dir);
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {}", self.binary.display()))?;
        // Reaped by a thread so an exited run host never lingers as a zombie of the app. Its
        // exit is kept, so a wait for its Welcome can stop at once: a failure says why, and a
        // clean exit means it did what it was launched for without greeting anyone.
        let exits = self.exits.clone();
        let run_dir = run_dir.to_owned();
        let _reaper = std::thread::Builder::new()
            .name("argonauts-run-host-reaper".to_owned())
            .spawn(move || {
                let exit = match child.wait() {
                    Ok(status) if status.success() => None,
                    Ok(status) => Some(format!("exited ({status})")),
                    Err(err) => Some(format!("was lost ({err})")),
                };
                exits
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(run_dir, exit);
            });
        Ok(())
    }

    fn failed(&self, run_dir: &Path) -> Option<String> {
        self.exits().get(run_dir).cloned().flatten()
    }

    fn exited_cleanly(&self, run_dir: &Path) -> bool {
        matches!(self.exits().get(run_dir), Some(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_host_gets_the_repo_and_the_run_as_arguments() {
        let launcher = ProcessLauncher::with_binary(PathBuf::from("/usr/local/bin/argonauts"));
        let run = RunId::new("run-20260930-101500-00ab").unwrap();
        let command = launcher.command(Path::new("/code/app"), &run);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "host",
                "--repo",
                "/code/app",
                "--run",
                "run-20260930-101500-00ab"
            ]
        );
    }

    #[test]
    fn a_launched_run_host_writes_to_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-argonauts");
        std::fs::write(&script, "#!/bin/sh\necho \"args: $*\"\n").unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let run = RunId::new("run-20260930-101500-00ab").unwrap();
        ProcessLauncher::with_binary(script)
            .launch(Path::new("/code/app"), &run, dir.path())
            .unwrap();
        let log = dir.path().join(run_files::RUN_HOST_LOG);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut text = String::new();
        while std::time::Instant::now() < deadline {
            text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains("args:") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            text.trim(),
            "args: host --repo /code/app --run run-20260930-101500-00ab"
        );
    }

    #[test]
    fn a_failed_run_host_is_remembered_until_the_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-argonauts");
        std::fs::write(&script, "#!/bin/sh\nexit 3\n").unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let run = RunId::new("run-20260930-101500-00ab").unwrap();
        let launcher = ProcessLauncher::with_binary(script.clone());
        launcher
            .launch(Path::new("/code/app"), &run, dir.path())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while launcher.failed(dir.path()).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the exit was never seen"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(launcher.failed(dir.path()).unwrap().contains('3'));

        std::fs::write(&script, "#!/bin/sh\nsleep 1\n").unwrap();
        launcher
            .launch(Path::new("/code/app"), &run, dir.path())
            .unwrap();
        assert_eq!(
            launcher.failed(dir.path()),
            None,
            "a new launch starts clean"
        );
    }
}
