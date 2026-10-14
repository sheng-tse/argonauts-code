//! A workflow an agent's CLI runs itself, such as one Claude Code's `Workflow` tool starts, as
//! one row: `Workflow(add-due-dates) · Claude Code's own · 3 agents`, its steps beneath as its
//! reports name them. The tool's own answer (paths, run ids, resume hints) is for the model,
//! so only the raw log shows it.

use argonauts_core::events::session::{SubagentStatus, VendorAgentState, VendorWorkflow};
use serde_json::Value;

use crate::cell::{StepMark, ToolCall, ToolItem, ToolKind, ToolState};
use crate::text::sanitize_line;
use crate::widgets::State;

const TOOL: &str = "Workflow";

/// Whether a call starts a workflow the CLI runs itself.
pub fn is_call(vendor: &str, name: &str) -> bool {
    vendor == "claude-code" && name == TOOL
}

/// The row from the workflow's latest report.
pub fn row(workflow: &VendorWorkflow) -> ToolCall {
    let state = match workflow.status {
        SubagentStatus::Done => ToolState::Done,
        SubagentStatus::Failed => ToolState::Failed,
        _ => ToolState::Running,
    };
    let agents = match workflow.agents.len() {
        0 => String::new(),
        1 => " · 1 agent".to_owned(),
        n => format!(" · {n} agents"),
    };
    let mut call = ToolCall::call(ToolKind::Agent, TOOL, name_of(&workflow.name))
        .with_state(state)
        .with_tail(format!(" · Claude Code's own{agents}"));
    if let Some(key) = &workflow.call {
        call = call.with_key(key.clone());
    }
    let steps = steps(workflow);
    if !steps.is_empty() {
        call.push(ToolItem::Steps(steps));
    }
    if state == ToolState::Failed
        && let Some(summary) = workflow.summary.as_deref().map(str::trim)
        && !summary.is_empty()
    {
        call.push(ToolItem::Text {
            text: sanitize_line(summary).into_owned(),
            tone: crate::cell::Tone::Error,
        });
    }
    call
}

/// The row before the workflow's first report: the name its call gives, if any. A call that
/// failed, such as a script that didn't parse, says why in its first line.
pub fn before_report(input: &Value, ok: Option<bool>, output: Option<&str>) -> ToolCall {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut call = ToolCall::call(ToolKind::Agent, TOOL, name_of(name))
        .with_tail(" · Claude Code's own".to_owned());
    if ok == Some(false) {
        let why = output
            .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
            .unwrap_or("It didn't start");
        call.push(ToolItem::Text {
            text: sanitize_line(why.trim()).into_owned(),
            tone: crate::cell::Tone::Error,
        });
        call.finish(false);
    }
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

/// Its declared phases, each in the state of its agents; a script that declares none shows its
/// agents instead.
fn steps(workflow: &VendorWorkflow) -> Vec<StepMark> {
    if workflow.phases.is_empty() {
        return workflow
            .agents
            .iter()
            .map(|agent| StepMark {
                label: agent.label.clone(),
                state: agent_state(agent.state),
            })
            .collect();
    }
    workflow
        .phases
        .iter()
        .map(|phase| {
            let states: Vec<State> = workflow
                .agents_in(phase.index)
                .map(|agent| agent_state(agent.state))
                .collect();
            StepMark {
                label: phase.title.clone(),
                state: phase_state(&states),
            }
        })
        .collect()
}

fn agent_state(state: VendorAgentState) -> State {
    match state {
        VendorAgentState::Running => State::Working,
        VendorAgentState::Done => State::Done,
        VendorAgentState::Failed => State::Failed,
        VendorAgentState::Queued | VendorAgentState::Other => State::Waiting,
    }
}

fn phase_state(agents: &[State]) -> State {
    if agents.iter().all(|state| *state == State::Waiting) {
        State::Waiting
    } else if agents.contains(&State::Failed) {
        State::Failed
    } else if agents.iter().all(|state| *state == State::Done) {
        State::Done
    } else {
        State::Working
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::{VendorAgent, VendorPhase};

    fn agent(index: u32, phase: u32, state: VendorAgentState) -> VendorAgent {
        VendorAgent {
            index,
            label: format!("agent:{index}"),
            phase: Some(phase),
            model: None,
            state,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            started_at_ms: None,
            attempt: 1,
            prompt: None,
            result: None,
        }
    }

    fn workflow(agents: Vec<VendorAgent>, status: SubagentStatus) -> VendorWorkflow {
        let phase = |index: u32, title: &str| VendorPhase {
            index,
            title: title.to_owned(),
            detail: String::new(),
        };
        VendorWorkflow {
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

    #[test]
    fn a_running_workflow_names_its_steps_in_their_states() {
        let running = workflow(
            vec![
                agent(1, 1, VendorAgentState::Done),
                agent(2, 2, VendorAgentState::Running),
                agent(3, 2, VendorAgentState::Done),
            ],
            SubagentStatus::Running,
        );
        let call = row(&running);
        assert_eq!(call.title.plain(), "Workflow(add-due-dates)");
        assert_eq!(call.state, ToolState::Running);
        assert_eq!(call.key.as_deref(), Some("toolu_1"));
        let marks: Vec<(&str, State)> = match call.body.as_slice() {
            [ToolItem::Steps(steps)] => steps
                .iter()
                .map(|step| (step.label.as_str(), step.state))
                .collect(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            marks,
            [
                ("build", State::Done),
                ("review", State::Working),
                ("judge", State::Waiting)
            ]
        );
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
        let call = before_report(
            &serde_json::json!({"script": "x"}),
            Some(false),
            Some("Workflow script has a syntax error and was not launched:\nUnexpected"),
        );
        assert_eq!(call.state, ToolState::Failed);
        match call.body.as_slice() {
            [ToolItem::Text { text, .. }] => assert!(text.starts_with("Workflow script has")),
            other => panic!("{other:?}"),
        }
    }
}
