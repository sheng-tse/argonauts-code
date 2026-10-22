//! Runs one of doctor's quick commands (`--version`, `--help`, a login check) in its own process
//! group, with a time limit and without agent keys. None of them calls a model.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use argonauts_core::{PROVIDER_KEY_ENV_VARS, StopSignal};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

use crate::process::signal_group;

/// How long a stopped updater has to end on its own before it and everything it started are
/// killed: long enough for npm to put back what it was replacing.
const STOP_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    pub success: bool,
    /// Stdout, then stderr.
    pub text: String,
}

pub(crate) async fn ask(path: &Path, args: &[&str], timeout: Duration) -> Result<Answer, String> {
    let mut command = command(path, args);
    let child = command
        .spawn()
        .map_err(|err| format!("could not run {}: {err}", path.display()))?;
    let mut group = Group::of(&child, path);
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(format!("could not run {}: {err}", path.display())),
        Err(_) => {
            group.kill();
            return Err(format!("`{} {}` timed out", path.display(), args.join(" ")));
        }
    };
    group.ended();
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(Answer {
        success: output.status.success(),
        text,
    })
}

/// Why a long command gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unanswered {
    /// It couldn't run, failed to finish in time, or went quiet, as this says.
    Failed(String),
    /// `stop` was cancelled, and the command and everything it started were ended.
    Stopped,
}

/// [`ask`] for a command that may run long, such as an updater that downloads: it has
/// `timeout` in all, and is stopped sooner once it has printed nothing for `quiet`, as one
/// that hangs does, or once `stop` is cancelled.
pub(crate) async fn ask_while_it_talks(
    path: &Path,
    args: &[&str],
    timeout: Duration,
    quiet: Duration,
    stop: &CancellationToken,
) -> Result<Answer, Unanswered> {
    use tokio::io::AsyncReadExt as _;
    use tokio::time::{Instant, sleep_until};

    let failed = Unanswered::Failed;
    let mut child = command(path, args)
        .spawn()
        .map_err(|err| failed(format!("could not run {}: {err}", path.display())))?;
    let mut group = Group::of(&child, path);
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(failed(format!("could not read {}", path.display())));
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_open, mut err_open) = (true, true);
    let (mut out_buf, mut err_buf) = ([0u8; 4096], [0u8; 4096]);
    let deadline = Instant::now() + timeout;
    let mut heard = Instant::now();
    let shown = format!("`{} {}`", path.display(), args.join(" "));
    while out_open || err_open {
        tokio::select! {
            read = stdout.read(&mut out_buf), if out_open => match read {
                Ok(0) | Err(_) => out_open = false,
                Ok(n) => {
                    out.extend_from_slice(&out_buf[..n]);
                    heard = Instant::now();
                }
            },
            read = stderr.read(&mut err_buf), if err_open => match read {
                Ok(0) | Err(_) => err_open = false,
                Ok(n) => {
                    err.extend_from_slice(&err_buf[..n]);
                    heard = Instant::now();
                }
            },
            () = sleep_until(deadline.min(heard + quiet)) => {
                group.kill();
                if Instant::now() >= deadline {
                    return Err(failed(format!("{shown} timed out")));
                }
                return Err(failed(format!(
                    "{shown} printed nothing for {}s, so it was stopped",
                    quiet.as_secs()
                )));
            }
            () = stop.cancelled() => {
                group.end(&mut child).await;
                return Err(Unanswered::Stopped);
            }
        }
    }
    let status = tokio::select! {
        waited = tokio::time::timeout_at(deadline, child.wait()) => match waited {
            Ok(Ok(status)) => status,
            Ok(Err(err)) => {
                return Err(failed(format!("could not run {}: {err}", path.display())));
            }
            Err(_) => {
                group.kill();
                return Err(failed(format!("{shown} timed out")));
            }
        },
        () = stop.cancelled() => {
            group.end(&mut child).await;
            return Err(Unanswered::Stopped);
        }
    };
    group.ended();
    let mut text = String::from_utf8_lossy(&out).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&err));
    Ok(Answer {
        success: status.success(),
        text,
    })
}

/// The process group a command runs in, killed when it's dropped before the command's end, so
/// a command left behind, as an update stopped by quitting is, takes everything it started
/// with it. Killing only the command itself would leave npm's own children running.
struct Group {
    id: Option<u32>,
    path: PathBuf,
    ended: bool,
}

impl Group {
    fn of(child: &Child, path: &Path) -> Self {
        Self {
            id: child.id(),
            path: path.to_path_buf(),
            ended: false,
        }
    }

    /// The command ended on its own, so there is nothing left to stop.
    fn ended(&mut self) {
        self.ended = true;
    }

    fn signal(&self, signal: StopSignal) {
        if let Some(group) = self.id
            && let Err(err) = signal_group(group, signal)
        {
            tracing::warn!("could not stop `{}`: {err}", self.path.display());
        }
    }

    /// Ends the group now, once it has run out of time.
    fn kill(&mut self) {
        self.signal(StopSignal::Kill);
        self.ended = true;
    }

    /// Asks the group to end, gives `child` a moment to, then kills what is left.
    async fn end(&mut self, child: &mut Child) {
        self.signal(StopSignal::Terminate);
        let _ = tokio::time::timeout(STOP_GRACE, child.wait()).await;
        self.kill();
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if !self.ended {
            self.signal(StopSignal::Kill);
        }
    }
}

/// The command in its own process group, with no input and without agent keys.
fn command(path: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    for name in PROVIDER_KEY_ENV_VARS {
        command.env_remove(name);
    }
    command
}
