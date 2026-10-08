//! `argonauts mcp session`, started by a harness for one agent session: the read-only
//! `argonauts_status` tool, which returns the session's pre-filtered `status.json` and reads
//! nothing else, plus `argonauts_report` for a step with an output schema. A direct turn's
//! server also has `approve`, Claude Code's permission prompt tool, and the main session's own
//! tools, which `main_tools` registers.

pub mod ask;
pub mod goal;
pub mod guide;
pub mod lookup;
pub mod main_tools;
pub mod report;
pub mod role;
pub mod setup;
pub mod start_workflow;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{AskTool, ReportTool};

pub const STATUS_TOOL: &str = "argonauts_status";
pub const REPORT_TOOL: &str = "argonauts_report";
pub use ask::APPROVE_TOOL;
pub use goal::SET_GOAL_TOOL;
pub use start_workflow::NAME as START_WORKFLOW_TOOL;

pub struct SessionServer {
    status_file: Option<PathBuf>,
    report: Option<report::ReportState>,
    asking: Option<AskTool>,
}

impl SessionServer {
    pub fn new(status_file: Option<PathBuf>, report: Option<ReportTool>) -> Self {
        Self {
            status_file,
            report: report.map(report::ReportState::new),
            asking: None,
        }
    }

    pub fn with_asking(mut self, asking: Option<AskTool>) -> Self {
        self.asking = asking;
        self
    }

    /// `argonauts_status` when there's a status file, and `argonauts_report` when a schema was
    /// given.
    pub fn tools(&self) -> Vec<Tool> {
        let no_arguments = match json!({ "type": "object", "properties": {}, "additionalProperties": false })
        {
            Value::Object(schema) => schema,
            _ => JsonObject::new(),
        };
        let mut tools = Vec::new();
        if self.status_file.is_some() {
            tools.push(
                Tool::new(
                    STATUS_TOOL,
                    "The run's status as the conductor last wrote it for you: the goal, your \
                     task, and what the other agents are doing. It takes no arguments and \
                     changes nothing.",
                    Arc::new(no_arguments),
                )
                .annotate(ToolAnnotations::new().read_only(true)),
            );
        }
        if let Some(report) = &self.report {
            tools.push(Tool::new(
                REPORT_TOOL,
                format!(
                    "Hands in your step's report. It must match this tool's input schema. \
                     Errors come back so you can fix them, {} times at most; the last valid \
                     report counts.",
                    report::MAX_INVALID
                ),
                Arc::new(report.input_schema()),
            ));
        }
        if let Some(asking) = &self.asking {
            tools.extend(ask::tools(asking));
            tools.extend(role::tools(asking));
        }
        tools
    }

    /// Runs one tool. Every failure is a tool error the agent can read.
    pub async fn call(&self, name: &str, arguments: serde_json::Value) -> CallToolResult {
        if let Some(asking) = &self.asking
            && let Some(result) = ask::call(asking, name, &arguments).await
        {
            return result;
        }
        if let Some(asking) = &self.asking
            && let Some(result) = role::call(asking, name, &arguments).await
        {
            return result;
        }
        match (name, &self.report) {
            (STATUS_TOOL, _) if let Some(status_file) = &self.status_file => {
                status(status_file, &arguments).await
            }
            (REPORT_TOOL, Some(report)) => match report.submit(&arguments) {
                Ok(()) => CallToolResult::success(vec![ContentBlock::text(
                    "Report saved. A later valid call replaces it.",
                )]),
                Err(message) => error(message),
            },
            _ => error(format!("there is no tool named {name:?}")),
        }
    }
}

/// Rereads the status file on every call. It takes no arguments, so nothing a caller passes
/// can point it at another file.
async fn status(status_file: &Path, arguments: &Value) -> CallToolResult {
    if arguments
        .as_object()
        .is_some_and(|arguments| !arguments.is_empty())
    {
        return error(format!("{STATUS_TOOL} takes no arguments"));
    }
    match tokio::fs::read_to_string(status_file).await {
        Ok(text) => {
            let structured = serde_json::from_str::<Value>(&text)
                .ok()
                .filter(Value::is_object);
            let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
            result.structured_content = structured;
            result
        }
        Err(err) if err.kind() == ErrorKind::NotFound => error(
            "There is no status yet. The conductor writes it once the run is under way.".to_owned(),
        ),
        Err(err) => error(format!("The status can't be read now: {err}")),
    }
}

fn error(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Serves until the harness closes the stream.
pub async fn serve_on<R, W>(
    status_file: Option<PathBuf>,
    report: Option<ReportTool>,
    read: R,
    write: W,
) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    serve(SessionServer::new(status_file, report), read, write).await
}

/// Serves `server` until the harness closes the stream.
pub async fn serve<R, W>(server: SessionServer, read: R, write: W) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    server.serve((read, write)).await?.waiting().await?;
    Ok(())
}

impl ServerHandler for SessionServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("argonauts-session", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(crate::tool_list(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = serde_json::Value::Object(request.arguments.unwrap_or_default());
        Ok(self.call(&request.name, arguments).await.into())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use rmcp::model::{CallToolRequestParams, JsonObject};
    use rmcp::service::RunningService;
    use rmcp::{RoleClient, ServiceExt};
    use serde_json::{Value, json};

    use super::report::MAX_INVALID;
    use super::*;

    async fn client(
        status_file: &Path,
        report: Option<ReportTool>,
    ) -> RunningService<RoleClient, ()> {
        let (client, server) = tokio::io::duplex(1 << 16);
        let (read, write) = tokio::io::split(server);
        tokio::spawn(serve_on(Some(status_file.to_owned()), report, read, write));
        ().serve(client).await.expect("handshake")
    }

    async fn call(
        client: &RunningService<RoleClient, ()>,
        name: &str,
        arguments: Value,
    ) -> CallToolResult {
        let arguments: JsonObject = match arguments {
            Value::Object(arguments) => arguments,
            _ => JsonObject::new(),
        };
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        client.call_tool(params).await.expect("call")
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn failed(result: &CallToolResult) -> bool {
        result.is_error == Some(true)
    }

    fn step_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["done", "blocked"] },
                "summary": { "type": "string", "maxLength": 200 }
            },
            "required": ["status", "summary"],
            "additionalProperties": false
        })
    }

    #[tokio::test]
    async fn status_tool_reads_only_its_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let status_file = dir.path().join("status.json");
        let secret_file = dir.path().join("secret.txt");
        fs::write(&secret_file, "SECRET-CONTENTS").expect("write");
        let client = client(&status_file, None).await;

        let tools = client.list_all_tools().await.expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, STATUS_TOOL);
        assert_eq!(
            tools[0]
                .annotations
                .as_ref()
                .and_then(|hints| hints.read_only_hint),
            Some(true)
        );
        let schema = Value::Object(tools[0].input_schema.as_ref().clone());
        assert_eq!(schema["properties"], json!({}));

        let missing = call(&client, STATUS_TOOL, json!({})).await;
        assert!(failed(&missing));
        assert!(
            text(&missing).contains("no status yet"),
            "{}",
            text(&missing)
        );

        fs::write(&status_file, r#"{"goal":"first"}"#).expect("write");
        let first = call(&client, STATUS_TOOL, json!({})).await;
        assert!(!failed(&first));
        assert_eq!(text(&first), r#"{"goal":"first"}"#);
        assert_eq!(first.structured_content, Some(json!({ "goal": "first" })));

        fs::write(&status_file, r#"{"goal":"second"}"#).expect("write");
        let second = call(&client, STATUS_TOOL, json!({})).await;
        assert_eq!(
            text(&second),
            r#"{"goal":"second"}"#,
            "it rereads each call"
        );

        for arguments in [
            json!({ "path": secret_file }),
            json!({ "file": "secret.txt" }),
            json!({ "status_file": secret_file }),
        ] {
            let result = call(&client, STATUS_TOOL, arguments.clone()).await;
            assert!(failed(&result), "{arguments}");
            assert!(!text(&result).contains("SECRET-CONTENTS"), "{arguments}");
        }
        let reported = call(&client, REPORT_TOOL, json!({ "status": "done" })).await;
        assert!(failed(&reported), "no report tool without a schema");
        client.cancel().await.expect("close");
    }

    #[tokio::test]
    async fn without_a_status_file_only_the_report_tool_is_served() {
        let dir = tempfile::tempdir().expect("temp dir");
        let report = ReportTool {
            schema: step_schema(),
            report_file: dir.path().join("report.json"),
        };
        let (client, server) = tokio::io::duplex(1 << 16);
        let (read, write) = tokio::io::split(server);
        tokio::spawn(serve_on(None, Some(report), read, write));
        let client = ().serve(client).await.expect("handshake");

        let tools = client.list_all_tools().await.expect("tools");
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, vec![REPORT_TOOL]);
        assert!(failed(&call(&client, STATUS_TOOL, json!({})).await));
        client.cancel().await.expect("close");
    }

    #[tokio::test]
    async fn mcp_report_tool_validates_and_writes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let status_file = dir.path().join("status.json");
        let report_file = dir.path().join("report.json");
        let report = ReportTool {
            schema: step_schema(),
            report_file: report_file.clone(),
        };
        let client = client(&status_file, Some(report.clone())).await;

        let tools = client.list_all_tools().await.expect("tools");
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, vec![STATUS_TOOL, REPORT_TOOL]);
        assert_eq!(
            Value::Object(tools[1].input_schema.as_ref().clone()),
            step_schema()
        );

        let invalid = call(&client, REPORT_TOOL, json!({ "status": "maybe" })).await;
        assert!(failed(&invalid));
        let said = text(&invalid);
        assert!(said.contains("/status"), "{said}");
        assert!(said.contains("summary"), "{said}");
        assert!(!report_file.exists(), "an invalid report was written");

        let valid = json!({ "status": "done", "summary": "added the tests" });
        let saved = call(&client, REPORT_TOOL, valid.clone()).await;
        assert!(!failed(&saved), "{}", text(&saved));
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&report_file).expect("read")).expect("json");
        assert_eq!(written, valid);
        client.cancel().await.expect("close");

        // A fresh session: after MAX_INVALID bad reports another bad one is refused, and a
        // good one is still saved.
        let fresh_file = dir.path().join("fresh").join("report.json");
        fs::create_dir(dir.path().join("fresh")).expect("fresh session dir");
        let client = self::client(
            &status_file,
            Some(ReportTool {
                schema: step_schema(),
                report_file: fresh_file.clone(),
            }),
        )
        .await;
        for _ in 0..MAX_INVALID {
            let result = call(&client, REPORT_TOOL, json!({ "status": 3 })).await;
            assert!(failed(&result));
            assert!(!text(&result).contains("refused"), "{}", text(&result));
        }
        let refused = call(&client, REPORT_TOOL, json!({ "status": 3 })).await;
        assert!(failed(&refused));
        assert!(text(&refused).contains("refused"), "{}", text(&refused));
        assert!(!fresh_file.exists());
        let saved = call(&client, REPORT_TOOL, valid.clone()).await;
        assert!(!failed(&saved), "{}", text(&saved));
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&fresh_file).expect("read")).expect("json");
        assert_eq!(written, valid);
        client.cancel().await.expect("close");
    }
}
