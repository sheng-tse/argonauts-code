//! A workflow an agent's CLI runs itself, such as one Claude Code's `Workflow` tool starts, as
//! Claude Code shows its own call: `Workflow(add-due-dates)`, and under it `Running in
//! background · /workflows to monitor and save` until it ends, then `✔ Completed in 4m 12s ·
//! 6 agents · 212k tokens`. Its phases and agents are in the workflow view. Nothing is drawn
//! before its first report: until the script exists there is no workflow to show. The tool's
//! own answer (paths, run ids, resume hints) is for the model, so only the raw log shows it.

use std::time::Duration;

use argonauts_core::runs::events::session::{CliWorkflowRun, SubagentStatus};
use serde_json::Value;

use crate::base::format;
use crate::base::text::sanitize_line;
use crate::cells::cell::{ToolCall, ToolItem, ToolKind, ToolState, WorkflowEnd, WorkflowResult};

const TOOL: &str = "Workflow";

/// What the working line says while the agent writes a workflow's script: until the script
/// exists there is no workflow to show, so only that line moves.
pub const WRITING: &str = "Writing a workflow";

/// Whether a call starts a workflow the CLI runs itself.
pub fn is_call(agent_id: &str, name: &str) -> bool {
    agent_id == "claude-code" && name == TOOL
}

/// The row from the workflow's latest report.
pub fn row(workflow: &CliWorkflowRun) -> ToolCall {
    let (state, ended) = match workflow.status {
        SubagentStatus::Done => (ToolState::Done, Some(WorkflowEnd::Completed)),
        SubagentStatus::Failed => (ToolState::Failed, Some(WorkflowEnd::Failed)),
        _ => (ToolState::Running, None),
    };
    let mut call = ToolCall::call(ToolKind::Agent, TOOL, name_of(&workflow.name)).with_state(state);
    if let Some(key) = &workflow.call {
        call = call.with_key(key.clone());
    }
    call.push(ToolItem::Workflow(match ended {
        Some(how) => WorkflowResult::Ended {
            how,
            counts: counts(workflow),
        },
        None => WorkflowResult::Running,
    }));
    if state == ToolState::Failed
        && let Some(summary) = workflow.summary.as_deref().map(str::trim)
        && !summary.is_empty()
    {
        call.push(ToolItem::Text {
            text: sanitize_line(summary).into_owned(),
            tone: crate::cells::cell::Tone::Error,
        });
    }
    call
}

/// `4m 12s · 6 agents · 212k tokens`, the workflow's own totals.
fn counts(workflow: &CliWorkflowRun) -> String {
    let mut counts = Vec::new();
    if workflow.duration_ms > 0 {
        counts.push(format::duration(Duration::from_millis(
            workflow.duration_ms,
        )));
    }
    match workflow.agents.len() {
        0 => {}
        1 => counts.push("1 agent".to_owned()),
        agents => counts.push(format!("{agents} agents")),
    }
    if workflow.tokens > 0 {
        counts.push(format!("{} tokens", format::tokens(workflow.tokens)));
    }
    counts.join(" · ")
}

/// The row of a call that failed before its workflow reported anything, such as a script that
/// didn't parse: it says why in its first line. A call that hasn't failed draws nothing until
/// its workflow's first report.
pub fn failed_start(input: &Value, output: Option<&str>) -> ToolCall {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut call = ToolCall::call(ToolKind::Agent, TOOL, name_of(name));
    let why = output
        .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or("It didn't start");
    call.push(ToolItem::Text {
        text: sanitize_line(why.trim()).into_owned(),
        tone: crate::cells::cell::Tone::Error,
    });
    call.finish(false);
    call
}

fn name_of(name: &str) -> String {
    let name = sanitize_line(name.trim()).into_owned();
    if name.is_empty() {
        "workflow".to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::runs::events::session::{CliAgent, CliAgentState, CliPhase};

    fn agent(index: u32, phase: u32, state: CliAgentState) -> CliAgent {
        CliAgent {
            index,
            label: format!("agent:{index}"),
            phase: Some(phase),
            model: None,
            state,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            started_at_ms: None,
            try_number: 1,
            prompt: None,
            result: None,
            ..CliAgent::default()
        }
    }

    fn workflow(agents: Vec<CliAgent>, status: SubagentStatus) -> CliWorkflowRun {
        let phase = |index: u32, title: &str| CliPhase {
            index,
            title: title.to_owned(),
            detail: String::new(),
        };
        CliWorkflowRun {
            id: "w1".to_owned(),
            call: Some("toolu_1".to_owned()),
            name: "add-due-dates".to_owned(),
            description: String::new(),
            phases: vec![phase(1, "build"), phase(2, "review"), phase(3, "judge")],
            agents,
            status,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        }
    }

    /// While it runs the row says where to watch it, and its phases stay in the workflow view.
    #[test]
    fn a_running_workflow_says_where_to_watch_it() {
        let running = workflow(
            vec![
                agent(1, 1, CliAgentState::Done),
                agent(2, 2, CliAgentState::Running),
            ],
            SubagentStatus::Running,
        );
        let call = row(&running);
        assert_eq!(call.title.plain(), "Workflow(add-due-dates)");
        assert_eq!(call.state, ToolState::Running);
        assert_eq!(call.key.as_deref(), Some("toolu_1"));
        assert_eq!(call.body, [ToolItem::Workflow(WorkflowResult::Running)]);
    }

    #[test]
    fn a_finished_workflow_gives_its_time_its_agents_and_its_tokens() {
        let mut done = workflow(
            (1..=6)
                .map(|at| agent(at, 1, CliAgentState::Done))
                .collect(),
            SubagentStatus::Done,
        );
        done.duration_ms = 252_000;
        done.tokens = 212_400;
        let call = row(&done);
        assert_eq!(call.state, ToolState::Done);
        let ended = WorkflowResult::Ended {
            how: WorkflowEnd::Completed,
            counts: "4m 12s · 6 agents · 212k tokens".to_owned(),
        };
        assert_eq!(call.body, [ToolItem::Workflow(ended)]);
    }

    #[test]
    fn a_failed_workflow_says_why_once() {
        let mut failed = workflow(Vec::new(), SubagentStatus::Failed);
        failed.summary = Some("Dynamic workflow \"x\" failed: boom".to_owned());
        let call = row(&failed);
        assert_eq!(call.state, ToolState::Failed);
        let errors = call
            .body
            .iter()
            .filter(|item| matches!(item, ToolItem::Text { .. }))
            .count();
        assert_eq!(errors, 1);
    }

    #[test]
    fn a_script_that_never_started_reads_as_its_first_line() {
        let call = failed_start(
            &serde_json::json!({"script": "x"}),
            Some("Workflow script has a syntax error and was not launched:\nUnexpected"),
        );
        assert_eq!(call.state, ToolState::Failed);
        match call.body.as_slice() {
            [ToolItem::Text { text, .. }] => assert!(text.starts_with("Workflow script has")),
            other => panic!("{other:?}"),
        }
    }
}
