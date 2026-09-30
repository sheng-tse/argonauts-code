//! The Argonauts MCP servers, over stdio: one for you or a front end to start and watch runs,
//! and one that each agent session gets for its status and report tools. Both are placeholders
//! that fail with "not implemented yet" until they are built.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::api::ConductorApi;
use argonauts_core::settings::agents::McpSettings;
use serde_json::Value;

/// Lets you start and watch runs from inside another agent. `ConductorApi` can't answer a
/// checkpoint, so this server can ask for a decision but never grant one.
pub async fn serve_user(api: Arc<dyn ConductorApi>) -> anyhow::Result<()> {
    serve_user_with(api, FrontEndOptions::default()).await
}

/// What a front end such as Hermes or OpenClaw may do through [`serve_user_with`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FrontEndOptions {
    /// `mcp.*` as resolved for the repo. The server never raises any of it.
    pub mcp: McpSettings,
    /// The repo was set up with `argonauts init` and you trusted its settings. The server
    /// refuses to start otherwise.
    pub repo_trusted: bool,
}

/// [`serve_user`] for front ends: both MCP protocol generations, guidance and pause, long
/// polling, and each run started with the client's name, `mcp.run_budget`, and the daily cap.
pub async fn serve_user_with(
    api: Arc<dyn ConductorApi>,
    options: FrontEndOptions,
) -> anyhow::Result<()> {
    let _ = (api, options);
    anyhow::bail!("argonauts-mcp: the user MCP server is not implemented yet")
}

/// The one tool agent sessions get: a read-only `argonauts_status` that rereads `status_file`
/// on each call.
pub async fn serve_status(status_file: &Path) -> anyhow::Result<()> {
    serve_session(status_file, None).await
}

/// The `argonauts_report` tool, for a step with an output schema.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportTool {
    /// The step's output schema, which is the tool's input schema.
    pub schema: Value,
    /// Where the last valid report goes, `<session_dir>/report.json`.
    pub report_file: PathBuf,
}

/// `argonauts mcp session`: `argonauts_status`, plus `argonauts_report` when `report` is set.
/// Each report call is validated, and its errors go back as a tool error, at most three times.
pub async fn serve_session(status_file: &Path, report: Option<ReportTool>) -> anyhow::Result<()> {
    let _ = (status_file, report);
    anyhow::bail!("argonauts-mcp: the session MCP server is not implemented yet")
}

#[cfg(test)]
mod tests {
    use argonauts_core::api::{
        ApiError, DiffScope, DiffView, EventStream, RawView, RunHandle, RunSummary, StartRun,
        StatusSummary, SteerAck,
    };
    use argonauts_core::events::session::QueuedMessage;
    use argonauts_core::future::BoxFuture;
    use argonauts_core::input::MentionTarget;
    use argonauts_core::names::{MessageId, SessionKey, WorkerRef};
    use argonauts_core::{CardId, RunId};

    use super::*;

    /// Refuses everything; the placeholder never calls it.
    struct NoConductor;

    fn refused<T: Send + 'static>() -> BoxFuture<'static, Result<T, ApiError>> {
        Box::pin(async { Err(ApiError::Refused("no conductor in this test".into())) })
    }

    impl ConductorApi for NoConductor {
        fn start(&self, _: StartRun) -> BoxFuture<'_, Result<RunHandle, ApiError>> {
            refused()
        }
        fn subscribe(&self, _: &RunId, _: u64) -> BoxFuture<'_, Result<EventStream, ApiError>> {
            refused()
        }
        fn steer(
            &self,
            _: &RunId,
            _: &SessionKey,
            _: String,
        ) -> BoxFuture<'_, Result<SteerAck, ApiError>> {
            refused()
        }
        fn mention(
            &self,
            _: &RunId,
            _: MentionTarget,
            _: String,
        ) -> BoxFuture<'_, Result<MessageId, ApiError>> {
            refused()
        }
        fn guidance(&self, _: &RunId, _: String) -> BoxFuture<'_, Result<MessageId, ApiError>> {
            refused()
        }
        fn take_back(
            &self,
            _: &RunId,
            _: Vec<MessageId>,
        ) -> BoxFuture<'_, Result<Vec<QueuedMessage>, ApiError>> {
            refused()
        }
        fn pause(&self, _: &RunId, _: bool) -> BoxFuture<'_, Result<(), ApiError>> {
            refused()
        }
        fn cancel_pause(&self, _: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
            refused()
        }
        fn resume(&self, _: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
            refused()
        }
        fn stop(&self, _: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
            refused()
        }
        fn stop_session(&self, _: &RunId, _: &SessionKey) -> BoxFuture<'_, Result<(), ApiError>> {
            refused()
        }
        fn reassign(
            &self,
            _: &RunId,
            _: &CardId,
            _: WorkerRef,
        ) -> BoxFuture<'_, Result<(), ApiError>> {
            refused()
        }
        fn list(&self) -> BoxFuture<'_, Result<Vec<RunSummary>, ApiError>> {
            refused()
        }
        fn continue_run(
            &self,
            _: &RunId,
            _: StartRun,
        ) -> BoxFuture<'_, Result<RunHandle, ApiError>> {
            refused()
        }
        fn status(&self, _: &RunId) -> BoxFuture<'_, Result<StatusSummary, ApiError>> {
            refused()
        }
        fn diff(
            &self,
            _: &RunId,
            _: &CardId,
            _: DiffScope,
        ) -> BoxFuture<'_, Result<DiffView, ApiError>> {
            refused()
        }
        fn raw(&self, _: &RunId, _: &SessionKey) -> BoxFuture<'_, Result<RawView, ApiError>> {
            refused()
        }
    }

    #[tokio::test]
    async fn the_placeholder_user_server_says_it_is_not_built() {
        let err = serve_user(Arc::new(NoConductor)).await.unwrap_err();
        assert!(err.to_string().contains("not implemented"), "{err}");
    }

    #[tokio::test]
    async fn the_placeholder_session_server_says_it_is_not_built() {
        let report = ReportTool {
            schema: serde_json::json!({"type": "object"}),
            report_file: "/nowhere/report.json".into(),
        };
        let err = serve_session(Path::new("/nowhere/status.json"), Some(report))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not implemented"));
        assert!(!FrontEndOptions::default().repo_trusted);
    }

    #[tokio::test]
    async fn the_placeholder_status_server_says_it_is_not_built() {
        let err = serve_status(Path::new("/nowhere/status.json"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not implemented"));
    }
}
