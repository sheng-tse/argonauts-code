//! A worker that starts Claude Code's own workflow: its `Workflow` call, the result that says
//! the script started, and the task reports that follow. The stream is made by hand in the
//! shape of the 2.1.289 recording, with the result text and the notification 2.1.295 writes.

use std::fs;
use std::path::Path;

use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_core::events::session::{SubagentStatus, VendorWorkflow};
use argonauts_core::{Adapter, AgentEvent};

const CALL: &str = "toolu_01WfAddDueDates0000000001";

fn replay(name: &str) -> Vec<AgentEvent> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(format!("{name}-2.1.295.stdout.jsonl"));
    let stdout = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut parser = ClaudeCodeAdapter.parser();
    let mut events: Vec<AgentEvent> = stdout
        .lines()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect();
    events.extend(parser.finish(Some(0)));
    events
}

fn workflows(events: &[AgentEvent]) -> Vec<&VendorWorkflow> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Workflow { workflow } => Some(&**workflow),
            _ => None,
        })
        .collect()
}

#[test]
fn the_launch_result_leaves_its_paths_and_hints_to_the_raw_log() {
    let events = replay("workflow-tool");
    let finished: Vec<(bool, Option<&str>)> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished { id, ok, output, .. } if id == CALL => {
                Some((*ok, output.as_deref()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(finished, [(true, None)]);
    let started = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolStarted { id, name, .. } if id == CALL && name == "Workflow"))
        .count();
    assert_eq!(started, 1);
    let leaked = events.iter().any(|event| {
        serde_json::to_string(event)
            .is_ok_and(|text| text.contains("resumeFromRunId") || text.contains("Transcript dir"))
    });
    assert!(!leaked, "a resume hint or a path reached an event");
}

#[test]
fn its_task_reports_name_the_call_and_every_step() {
    let events = replay("workflow-tool");
    let seen = workflows(&events);
    let last = seen.last().copied().expect("the workflow reported");
    assert_eq!(last.call.as_deref(), Some(CALL));
    assert_eq!(last.name, "add-due-dates");
    let titles: Vec<&str> = last.phases.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["Build", "Review", "Judge"]);
    assert_eq!(last.agents.len(), 3);
    assert_eq!(last.status, SubagentStatus::Done);
    assert!(
        seen.iter()
            .any(|workflow| workflow.status == SubagentStatus::Running)
    );
}

#[test]
fn a_script_that_never_started_keeps_its_error() {
    let mut parser = ClaudeCodeAdapter.parser();
    let lines = [
        serde_json::json!({"type": "assistant", "message": {"id": "m1", "content": [
            {"type": "tool_use", "id": "t1", "name": "Workflow", "input": {"script": "x"}}]}}),
        serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result",
            "tool_use_id": "t1", "is_error": true,
            "content": "Workflow script has a syntax error and was not launched:\nUnexpected token"}]}}),
    ];
    let events: Vec<AgentEvent> = lines
        .iter()
        .flat_map(|line| parser.parse_stdout_line(&line.to_string()))
        .collect();
    let output = events.iter().find_map(|event| match event {
        AgentEvent::ToolFinished {
            ok: false, output, ..
        } => output.clone(),
        _ => None,
    });
    assert!(output.is_some_and(|text| text.contains("syntax error")));
}
