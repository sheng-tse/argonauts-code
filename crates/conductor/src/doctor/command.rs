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
    let child = command
        .spawn()
        .map_err(|err| format!("could not run {}: {err}", path.display()))?;
    let group = child.id();
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(format!("could not run {}: {err}", path.display())),
        Err(_) => {
            if let Some(group) = group
                && let Err(err) = signal_group(group, StopSignal::Kill)
            {
                tracing::warn!("could not stop `{}`: {err}", path.display());
            }
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
