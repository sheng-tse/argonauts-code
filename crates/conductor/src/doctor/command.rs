//! Runs one of doctor's quick commands (`--version`, `--help`, a login check) in its own process
//! group, with a time limit and without agent keys. None of them calls a model.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use argonauts_core::{PROVIDER_KEY_ENV_VARS, StopSignal};
use tokio::process::Command;

use crate::process::signal_group;

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
    let group = child.id();
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(format!("could not run {}: {err}", path.display())),
        Err(_) => {
            stop(group, path);
            return Err(format!("`{} {}` timed out", path.display(), args.join(" ")));
        }
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(Answer {
        success: output.status.success(),
        text,
    })
}

/// [`ask`] for a command that may run long, such as an updater that downloads: it has
/// `timeout` in all, and is stopped sooner once it has printed nothing for `quiet`, as one
/// that hangs does.
pub(crate) async fn ask_while_it_talks(
    path: &Path,
    args: &[&str],
    timeout: Duration,
    quiet: Duration,
) -> Result<Answer, String> {
    use tokio::io::AsyncReadExt as _;
    use tokio::time::{Instant, sleep_until};

    let mut child = command(path, args)
        .spawn()
        .map_err(|err| format!("could not run {}: {err}", path.display()))?;
    let group = child.id();
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(format!("could not read {}", path.display()));
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
                stop(group, path);
                if Instant::now() >= deadline {
                    return Err(format!("{shown} timed out"));
                }
                return Err(format!(
                    "{shown} printed nothing for {}s, so it was stopped",
                    quiet.as_secs()
                ));
            }
        }
    }
    let status = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(err)) => return Err(format!("could not run {}: {err}", path.display())),
        Err(_) => {
            stop(group, path);
            return Err(format!("{shown} timed out"));
        }
    };
    let mut text = String::from_utf8_lossy(&out).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&err));
    Ok(Answer {
        success: status.success(),
        text,
    })
}

/// Stops the process group a command runs in, once it has run out of time.
fn stop(group: Option<u32>, path: &Path) {
    if let Some(group) = group
        && let Err(err) = signal_group(group, StopSignal::Kill)
    {
        tracing::warn!("could not stop `{}`: {err}", path.display());
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
