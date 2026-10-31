//! Runs check commands in the task's worktree, each in its own session with a time limit.
//! [`SandboxedChecks`] is the `CheckRunner` every run uses, so every check it takes, yours or
//! the ones its main agent picked, runs inside the run's sandbox as its agents' own commands
//! do, and can do nothing they couldn't. Only `checks.setup` runs outside it, since it installs
//! dependencies, and only your own settings name it. `run_check` runs one with no sandbox of
//! ours. Either way a check gets only an allowlisted environment (never keys, tokens, or agent
//! sockets) and no terminal.

pub mod env;
pub mod runner;
pub mod setup;
pub mod summary;

pub use runner::{NO_COMMAND_CHECKS, SandboxedChecks, no_command_checks};

use std::ffi::OsString;
use std::io::{self, PipeWriter};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use argonauts_core::{CheckResult, StopSignal};
use tokio::io::AsyncReadExt;
use tokio::net::unix::pipe;
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep_until};

use crate::limits::control::StopHandle;
use crate::process::{new_session, signal_group};
use crate::session::run_host::millis;

/// Variables a check keeps from Argonauts Code's environment, besides `LC_*`, so build and test
/// tools can find themselves.
const CHECK_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TERM",
    "TMPDIR",
    "TZ",
    "CI",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "GOPATH",
    "GOROOT",
    "GOCACHE",
    "GOMODCACHE",
    "JAVA_HOME",
    "NVM_DIR",
    "PYENV_ROOT",
    "VIRTUAL_ENV",
];

pub const OUTPUT_TAIL_BYTES: usize = 16 * 1024;

/// How long to keep reading after a check exits, in case something it started still writes.
const EXIT_GRACE: Duration = Duration::from_secs(1);

/// A command that can't start, times out, or is stopped shows up as a failed result that says why.
pub async fn run_check(
    command: &[String],
    cwd: &Path,
    timeout: Duration,
    stop: &StopHandle,
) -> CheckResult {
    let started = Instant::now();
    let ran = CheckProcess::new(command, cwd, check_env(std::env::vars_os()));
    let (exit_code, output) = match ran {
        Ok(process) => match execute(&process, timeout, stop).await {
            Ok(ran) => (ran.exit_code, ran.output_with_note()),
            Err(err) => (None, format!("could not run the checks: {err}")),
        },
        Err(err) => (None, format!("could not run the checks: {err}")),
    };
    CheckResult {
        command: command.to_vec(),
        passed: exit_code == Some(0),
        exit_code,
        duration_ms: millis(started.elapsed()),
        output,
    }
}

/// One check process: an argument vector, where it runs, and its whole environment.
#[derive(Debug, Clone)]
pub(crate) struct CheckProcess {
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
}

impl CheckProcess {
    pub(crate) fn new(
        argv: &[String],
        cwd: &Path,
        env: Vec<(OsString, OsString)>,
    ) -> io::Result<Self> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the command is empty"))?;
        Ok(Self {
            program: program.clone(),
            args: args.to_vec(),
            cwd: cwd.to_path_buf(),
            env,
        })
    }
}

/// How a check process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ran {
    /// `None` when it ended early or by a signal.
    pub exit_code: Option<i32>,
    /// Why Argonauts Code ended it, such as `timed out after 30m`.
    pub ended_early: Option<String>,
    /// The last [`OUTPUT_TAIL_BYTES`] of stdout and stderr together.
    pub output: String,
}

impl Ran {
    pub(crate) fn passed(&self) -> bool {
        self.ended_early.is_none() && self.exit_code == Some(0)
    }

    /// The output with a last line saying why Argonauts Code ended the check, if it did.
    pub(crate) fn output_with_note(&self) -> String {
        let mut text = self.output.clone();
        if let Some(why) = &self.ended_early {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("[argonauts] check ");
            text.push_str(why);
        }
        text
    }
}

/// Runs `process` in a new session, keeps the tail of its output, and kills its whole process
/// group at the end, whatever happened.
pub(crate) async fn execute(
    process: &CheckProcess,
    timeout: Duration,
    stop: &StopHandle,
) -> io::Result<Ran> {
    let (reader, writer) = io::pipe()?;
    let mut child = spawn_check(process, writer)?;
    let group = child.id().unwrap_or(0);
    let mut output = pipe::Receiver::from_owned_fd(OwnedFd::from(reader))?;

    let mut tail = Tail::new(OUTPUT_TAIL_BYTES);
    let mut chunk = vec![0u8; 8192];
    let mut output_open = true;
    let mut status = None;
    let mut ended_early: Option<String> = None;
    let mut drain_until: Option<Instant> = None;
    let deadline = Instant::now() + timeout;

    while output_open || status.is_none() {
        tokio::select! {
            read = output.read(&mut chunk), if output_open => match read {
                Ok(0) | Err(_) => output_open = false,
                Ok(n) => tail.push(&chunk[..n]),
            },
            exit = child.wait(), if status.is_none() => {
                status = Some(exit?);
                drain_until = Some(Instant::now() + EXIT_GRACE);
            }
            () = sleep_until(drain_until.unwrap_or(deadline)), if drain_until.is_some() => break,
            () = sleep_until(deadline), if status.is_none() && ended_early.is_none() => {
                ended_early = Some(format!("timed out after {}", format_duration(timeout)));
                kill(group);
            }
            () = stop.stopped(), if status.is_none() && ended_early.is_none() => {
                ended_early = Some("stopped by you".to_owned());
                kill(group);
            }
        }
    }
    kill(group);

    let exit_code = match ended_early {
        Some(_) => None,
        None => status.and_then(|s| s.code()),
    };
    Ok(Ran {
        exit_code,
        ended_early,
        output: tail.into_string(),
    })
}

fn spawn_check(process: &CheckProcess, output: PipeWriter) -> io::Result<Child> {
    let mut command = Command::new(&process.program);
    command
        .args(&process.args)
        .current_dir(&process.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output))
        .kill_on_drop(true)
        .env_clear()
        .envs(process.env.iter().map(|(k, v)| (k, v)))
        .env("GIT_TERMINAL_PROMPT", "0");
    new_session(&mut command);
    // Returning drops the command's copies of the write end, so EOF can arrive.
    command.spawn()
}

fn kill(group: u32) {
    if group == 0 {
        return;
    }
    if let Err(err) = signal_group(group, StopSignal::Kill) {
        tracing::warn!("could not stop the checks: {err}");
    }
}

fn check_env(vars: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    vars.filter(|(name, _)| {
        name.to_str()
            .is_some_and(|name| CHECK_ENV.contains(&name) || name.starts_with("LC_"))
    })
    .collect()
}

pub(crate) fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0 => format!("{}ms", duration.as_millis()),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// The last `cap` bytes of a stream, cut to start at a line boundary once anything is dropped.
struct Tail {
    buf: Vec<u8>,
    cap: usize,
    dropped: bool,
}

impl Tail {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
            dropped: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        // Trim in batches so pushing stays cheap.
        if self.buf.len() > self.cap * 2 {
            self.buf.drain(..self.buf.len() - self.cap);
            self.dropped = true;
        }
    }

    fn into_string(mut self) -> String {
        if self.buf.len() > self.cap {
            self.buf.drain(..self.buf.len() - self.cap);
            self.dropped = true;
        }
        let mut kept = self.buf.as_slice();
        if self.dropped
            && let Some(newline) = kept.iter().position(|&b| b == b'\n')
        {
            kept = &kept[newline + 1..];
        }
        let text = String::from_utf8_lossy(kept);
        if self.dropped {
            format!("…\n{text}")
        } else {
            text.into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    async fn check(parts: &[&str], timeout: Duration) -> CheckResult {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("present.txt"), "x").unwrap();
        run_check(&argv(parts), dir.path(), timeout, &StopHandle::new()).await
    }

    #[tokio::test]
    async fn passing_check() {
        let result = check(&["test", "-f", "present.txt"], Duration::from_secs(10)).await;
        assert!(result.passed);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.command, ["test", "-f", "present.txt"]);
    }

    #[tokio::test]
    async fn failing_check_keeps_combined_output_in_order() {
        let script = "echo out; echo err >&2; echo more; exit 4";
        let result = check(&["/bin/sh", "-c", script], Duration::from_secs(10)).await;
        assert!(!result.passed);
        assert_eq!(result.exit_code, Some(4));
        assert_eq!(result.output, "out\nerr\nmore\n");
    }

    #[tokio::test]
    async fn timeout_kills_the_check_and_its_children() {
        let started = std::time::Instant::now();
        let result = check(
            &["/bin/sh", "-c", "echo starting; sleep 30 & sleep 30"],
            Duration::from_millis(200),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!result.passed);
        assert_eq!(result.exit_code, None);
        assert!(result.output.starts_with("starting\n"));
        assert!(
            result
                .output
                .ends_with("[argonauts] check timed out after 200ms")
        );
    }

    #[tokio::test]
    async fn stop_kills_a_running_check() {
        let dir = tempfile::tempdir().unwrap();
        let stop = StopHandle::new();
        let trigger = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.stop();
        });
        let result = run_check(
            &argv(&["sleep", "30"]),
            dir.path(),
            Duration::from_secs(60),
            &stop,
        )
        .await;
        assert!(!result.passed);
        assert_eq!(result.output, "[argonauts] check stopped by you");
    }

    #[tokio::test]
    async fn missing_or_empty_commands_fail_cleanly() {
        let result = check(&["argonauts-no-such-check"], Duration::from_secs(5)).await;
        assert!(!result.passed);
        assert!(
            result.output.starts_with("could not run the checks"),
            "{}",
            result.output
        );
        let result = check(&[], Duration::from_secs(5)).await;
        assert!(!result.passed);
        assert!(result.output.contains("empty"));
    }

    #[tokio::test]
    async fn checks_get_a_small_environment_and_no_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let script = "env | grep -E '^(ANTHROPIC_API_KEY|OPENAI_API_KEY|GH_TOKEN|SSH_AUTH_SOCK|CARGO_MANIFEST_DIR)=' || echo clean
             test -n \"$PATH\" && echo \"prompt=$GIT_TERMINAL_PROMPT\"
             (: < /dev/tty) 2>/dev/null && echo tty || echo no-tty";
        let result = run_check(
            &argv(&["/bin/sh", "-c", script]),
            dir.path(),
            Duration::from_secs(10),
            &StopHandle::new(),
        )
        .await;
        assert_eq!(result.output, "clean\nprompt=0\nno-tty\n");
    }

    #[test]
    fn check_env_is_an_allowlist() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/a"),
            ("LC_ALL", "C"),
            ("SSH_AUTH_SOCK", "/tmp/agent"),
            ("ANTHROPIC_API_KEY", "sk-x"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "t"),
            ("AWS_SECRET_ACCESS_KEY", "s"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let kept: Vec<_> = check_env(vars.into_iter())
            .into_iter()
            .map(|(k, _)| k.into_string().unwrap())
            .collect();
        assert_eq!(kept, ["PATH", "HOME", "LC_ALL"]);
    }

    #[test]
    fn tail_keeps_the_end_from_a_line_start() {
        let mut tail = Tail::new(10);
        tail.push(b"line one\nline two\nline three\n");
        assert_eq!(tail.into_string(), "…\n");
        let mut tail = Tail::new(12);
        tail.push(b"aaaa\nbbbb\ncccc\n");
        assert_eq!(tail.into_string(), "…\nbbbb\ncccc\n");
        let mut tail = Tail::new(64);
        tail.push(b"short\n");
        assert_eq!(tail.into_string(), "short\n");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format_duration(Duration::from_millis(250)), "250ms");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
        assert_eq!(format_duration(Duration::from_secs(1800)), "30m");
    }
}
