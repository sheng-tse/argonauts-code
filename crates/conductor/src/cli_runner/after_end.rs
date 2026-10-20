//! The command a parser asks for once its process has ended, such as OpenCode's export of a
//! session whose last step its stream never printed. It runs in the session's own sandbox and
//! environment, reads nothing from stdin, and is given up on after [`TIMEOUT`].

use std::process::Stdio;
use std::time::Duration;

use argonauts_core::CommandSpec;
use argonauts_core::session_runner::{SandboxMode, SandboxProfile};

use super::context::Context;

/// Long enough for a private OpenCode server to start and read its own session store.
const TIMEOUT: Duration = Duration::from_secs(30);

/// What `command` printed, or `None` when it couldn't run in the session's sandbox, failed, or
/// took too long. Why goes to the log, since nothing on screen waits for it.
pub(super) async fn run(
    ctx: &Context,
    command: CommandSpec,
    profile: &SandboxProfile,
) -> Option<String> {
    let shown = command.display_line();
    let command = match profile.mode {
        SandboxMode::None => command,
        SandboxMode::Os | SandboxMode::Container => match ctx.sandbox.wrap(command, profile) {
            Ok(wrapped) => wrapped,
            Err(err) => {
                tracing::warn!(%shown, %err, "the command after the session couldn't be sandboxed");
                return None;
            }
        },
    };
    let mut process = tokio::process::Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in &command.env_remove {
        process.env_remove(name);
    }
    process.envs(command.env_set.iter().cloned());
    let output = match tokio::time::timeout(TIMEOUT, process.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            tracing::warn!(%shown, %err, "the command after the session didn't start");
            return None;
        }
        Err(_) => {
            tracing::warn!(%shown, "the command after the session took too long");
            return None;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let why = stderr.lines().rev().find(|line| !line.trim().is_empty());
        tracing::warn!(%shown, status = %output.status, why = why.unwrap_or(""), "the command after the session failed");
        return None;
    }
    String::from_utf8(output.stdout).ok()
}
