//! A session that runs Claude Code's own workflow starts as every crew session on Claude Code
//! does, without your claude.ai connectors, and still has Argonauts Code's report tool.

use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_core::adapter::Adapter;
use argonauts_core::names::ModelRef;
use argonauts_core::session_runner::Access;
use argonauts_core::settings::Settings;
use serde_json::{Value, json};

use super::{STATUS_SERVER, session_request};
use crate::cli_runner::testing::spec_in;

#[test]
fn its_own_workflow_keeps_the_report_tool_and_leaves_your_connectors_out() {
    let work = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut spec = spec_in(
        work.path(),
        &scratch.path().join("session"),
        "add due dates",
    );
    spec.model = ModelRef::new("claude-code", Some("haiku".into()));
    spec.access = Access::Build;
    spec.json_schema = Some(json!({
        "type": "object",
        "properties": {"summary": {"type": "string"}},
        "required": ["summary"],
    }));
    spec.cli_workflow.ultracode = true;

    let request = session_request(&spec, &ClaudeCodeAdapter, &Settings::default(), None).unwrap();
    assert!(
        request.json_schema.is_none(),
        "the report comes through the tool"
    );
    let command = ClaudeCodeAdapter.command(&request).unwrap();
    let connectors = command
        .env_set
        .iter()
        .find(|(name, _)| name == "ENABLE_CLAUDEAI_MCP_SERVERS")
        .map(|(_, value)| value.as_str());
    assert_eq!(connectors, Some("false"), "{:?}", command.env_set);

    let at = command
        .args
        .iter()
        .position(|arg| arg == "--mcp-config")
        .expect("Argonauts Code's own tools");
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(&command.args[at + 1]).unwrap()).unwrap();
    let server = config["mcpServers"][STATUS_SERVER]["args"].to_string();
    assert!(server.contains("--report-schema"), "{server}");
    assert!(server.contains("--report-file"), "{server}");
}
