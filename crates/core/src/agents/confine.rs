//! Commands a CLI runs for Argonauts Code outside any session, such as OpenCode listing its own
//! sessions or its models. Each starts the CLI in a folder whose config may load plugins, which
//! is code the folder brings, so each runs as a read-only session of that CLI would.

use std::fmt::Debug;

use crate::CommandSpec;
use crate::base::future::BoxFuture;

/// Puts a CLI's own command inside the sandbox.
pub trait Confine: Send + Sync + Debug {
    /// `command`, which the CLI `cli` (an adapter id, such as `opencode`) runs, as it starts
    /// confined: it reads the folder it runs in, writes only the CLI's own state and temporary
    /// folders, and never reads your credentials or Argonauts Code's home. `Err` says why it
    /// can't be, and then it doesn't run.
    fn confine<'a>(
        &'a self,
        cli: &'a str,
        command: CommandSpec,
    ) -> BoxFuture<'a, Result<CommandSpec, String>>;
}

/// Starts each command as it is, for a test's fake CLI.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unconfined;

impl Confine for Unconfined {
    fn confine<'a>(
        &'a self,
        _cli: &'a str,
        command: CommandSpec,
    ) -> BoxFuture<'a, Result<CommandSpec, String>> {
        Box::pin(async move { Ok(command) })
    }
}
