//! The Argonauts MCP servers, over stdio. `argonauts mcp serve` lets you or a front end start
//! and watch runs; it holds only a `ConductorApi`, so it can ask for a decision but never grant
//! one. `argonauts mcp session` gives each agent session its read-only status tool and, for a
//! step with an output schema, its report tool.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::api::ConductorApi;
use argonauts_core::settings::agents::McpSettings;
use serde_json::Value;

pub mod session;
pub mod user;

/// Lets you start and watch runs from inside another agent. `ConductorApi` can't answer a
/// checkpoint, so this server can ask for a decision but never grant one. It always refuses to
/// start, since the default options count the repo as untrusted. Front ends and the CLI pass
/// the repo's real settings and trust through [`serve_user_with`].
pub async fn serve_user(api: Arc<dyn ConductorApi>) -> anyhow::Result<()> {
    serve_user_with(api, FrontEndOptions::default()).await
}

/// What a front end such as Hermes or OpenClaw may do through [`serve_user_with`].
#[derive(Clone, Default)]
pub struct FrontEndOptions {
    /// `mcp.*` as resolved for the repo at start. The server never raises any of it.
    pub mcp: McpSettings,
    /// The repo was set up with `argonauts init` and you trusted its settings. The server
    /// refuses to start otherwise.
    pub repo_trusted: bool,
    /// Rereads both while the server runs, since a front end may keep it for days. Each tool
    /// call and tool list uses the stricter of the start values and the current ones, and
    /// refuses everything once the repo isn't trusted. Without it the start values hold.
    pub reload: Option<FrontEndReloader>,
}

impl fmt::Debug for FrontEndOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrontEndOptions")
            .field("mcp", &self.mcp)
            .field("repo_trusted", &self.repo_trusted)
            .field("reload", &self.reload.is_some())
            .finish()
    }
}

/// Reads `mcp.*` and the repo's trust as they are now. `None` means the read failed, and the
/// start values hold, so a reloader that can read the settings but not the trust should say
/// the repo is untrusted.
pub type FrontEndReloader = Arc<dyn Fn() -> Option<FrontEndSettings> + Send + Sync>;

/// `mcp.*` and the repo's trust, as a [`FrontEndReloader`] reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct FrontEndSettings {
    pub mcp: McpSettings,
    pub repo_trusted: bool,
}

/// [`serve_user`] for front ends: both MCP protocol generations, guidance and pause, long
/// polling, and each run started with the client's name, `mcp.run_budget`, and the daily cap.
pub async fn serve_user_with(
    api: Arc<dyn ConductorApi>,
    options: FrontEndOptions,
) -> anyhow::Result<()> {
    let (stdin, stdout) = rmcp::transport::io::stdio();
    user::serve_on(api, options, stdin, stdout).await
}

/// The one tool agent sessions get: a read-only `argonauts_status` that rereads `status_file`
/// on each call.
pub async fn serve_status(status_file: &Path) -> anyhow::Result<()> {
    serve_session(Some(status_file), None).await
}

/// The `argonauts_report` tool, for a step with an output schema.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportTool {
    /// The step's output schema, which is the tool's input schema.
    pub schema: Value,
    /// Where the last valid report goes, `<session_dir>/report.json`.
    pub report_file: PathBuf,
}

/// `argonauts mcp session`: `argonauts_status` when there's a status file, plus
/// `argonauts_report` when `report` is set.
/// Each report call is validated, and its errors go back as a tool error, at most three times.
pub async fn serve_session(
    status_file: Option<&Path>,
    report: Option<ReportTool>,
) -> anyhow::Result<()> {
    let (stdin, stdout) = rmcp::transport::io::stdio();
    session::serve_on(status_file.map(Path::to_owned), report, stdin, stdout).await
}
