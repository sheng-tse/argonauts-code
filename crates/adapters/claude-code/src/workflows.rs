//! Workflows Claude Code runs itself (2.1.289 and later): the `Workflow` tool starts a script in
//! the background, and `system/task_*` events with `task_type: local_workflow` report it.
//! `task_started` carries the script, whose `meta` names the workflow and its phases;
//! `task_progress` carries `workflow_progress`, every phase and agent so far; `task_updated` and
//! `task_notification` end it. Each becomes a whole `AgentEvent::Workflow`, so the views draw it
//! like one of Argonauts Code's own, with whatever names it uses.

use std::collections::HashMap;

use argonauts_core::AgentEvent;
use argonauts_core::events::session::{
    CliAgent, CliAgentState, CliPhase, CliTool, CliWorkflowRun, SubagentStatus,
};
use serde_json::Value;

use crate::json::{array_at, str_at, string_at, u64_at};
use crate::script_meta;

const LOCAL_WORKFLOW: &str = "local_workflow";
const PROMPT_CHARS: usize = 200;
const RESULT_CHARS: usize = 400;
/// What a tool was called with, which the opened agent shows on one row.
const TOOL_CHARS: usize = 120;

#[derive(Debug, Default)]
pub struct WorkflowTracker {
    /// By task id.
    running: HashMap<String, CliWorkflowRun>,
}

impl WorkflowTracker {
    /// A `system/task_*` line about a workflow, as the workflow's whole state now. `None` for a
    /// line about anything else, or one that changed nothing.
    pub fn task_event(&mut self, subtype: &str, line: &Value) -> Option<AgentEvent> {
        let task = str_at(line, "task_id")?;
        let is_workflow = str_at(line, "task_type") == Some(LOCAL_WORKFLOW)
            || line.get("workflow_progress").is_some();
        if subtype == "task_started" {
            if !is_workflow {
                return None;
            }
            let declared = started(task, line);
            match self.running.get_mut(task) {
                // A progress report came first: what the start declares joins what it said.
                Some(known) => {
                    known.name = declared.name;
                    known.description = declared.description;
                    known.call = known.call.take().or(declared.call);
                    for phase in declared.phases {
                        if !known.phases.iter().any(|seen| seen.index == phase.index) {
                            known.phases.push(phase);
                        }
                    }
                    known.phases.sort_by_key(|phase| phase.index);
                }
                None => {
                    self.running.insert(task.to_owned(), declared);
                }
            }
            return self.event(task);
        }
        // A stream that lost or reordered the start still shows the workflow from its first
        // report that says it's one.
        if !self.running.contains_key(task) && is_workflow {
            self.running.insert(task.to_owned(), started(task, line));
        }
        let before = self.running.get(task)?.clone();
        let workflow = self.running.get_mut(task)?;
        match subtype {
            "task_progress" => progress(workflow, line),
            "task_updated" => {
                if let Some(status) = line
                    .get("patch")
                    .and_then(|patch| str_at(patch, "status"))
                    .and_then(task_status)
                {
                    workflow.status = status;
                }
            }
            "task_notification" => {
                if let Some(status) = str_at(line, "status").and_then(task_status) {
                    workflow.status = status;
                }
                if let Some(summary) = str_at(line, "summary") {
                    workflow.summary = Some(summary.to_owned());
                }
                totals(workflow, line);
            }
            _ => return None,
        }
        if *workflow == before {
            return None;
        }
        self.event(task)
    }

    fn event(&self, task: &str) -> Option<AgentEvent> {
        let workflow = self.running.get(task)?.clone();
        Some(AgentEvent::Workflow {
            workflow: Box::new(workflow),
        })
    }

    /// The stream ended with workflows still going, as when the session was stopped, paused,
    /// or killed before they reported their end: each ends with it.
    pub fn close(&mut self, why: &str) -> Vec<AgentEvent> {
        let mut open: Vec<&mut CliWorkflowRun> = self
            .running
            .values_mut()
            .filter(|workflow| !workflow.ended())
            .collect();
        open.sort_by(|a, b| a.id.cmp(&b.id));
        open.into_iter()
            .map(|workflow| {
                workflow.end_with_session(why);
                AgentEvent::Workflow {
                    workflow: Box::new(workflow.clone()),
                }
            })
            .collect()
    }
}

/// The workflow as its script declares it: the `meta`'s name, description, and phases, else
/// what the event itself says.
fn started(task: &str, line: &Value) -> CliWorkflowRun {
    let meta = str_at(line, "prompt").and_then(script_meta::meta);
    let meta_str = |key: &str| {
        meta.as_ref()
            .and_then(|meta| str_at(meta, key))
            .map(str::to_owned)
    };
    let phases = meta
        .as_ref()
        .map(|meta| array_at(meta, "phases"))
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter_map(|(index, phase)| {
            Some(CliPhase {
                index: u32::try_from(index + 1).ok()?,
                title: str_at(phase, "title")?.to_owned(),
                detail: string_at(phase, "detail").unwrap_or_default(),
            })
        })
        .collect();
    CliWorkflowRun {
        id: task.to_owned(),
        call: string_at(line, "tool_use_id"),
        name: meta_str("name")
            .or_else(|| string_at(line, "workflow_name"))
            .unwrap_or_else(|| "workflow".to_owned()),
        description: meta_str("description")
            .or_else(|| string_at(line, "description"))
            .unwrap_or_default(),
        phases,
        agents: Vec::new(),
        status: SubagentStatus::Running,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        summary: None,
        started_at_ms: None,
    }
}

/// A progress report: the totals, what it's doing now, and every phase and agent it lists.
/// Some reports carry only the totals.
fn progress(workflow: &mut CliWorkflowRun, line: &Value) {
    totals(workflow, line);
    if let Some(now) = str_at(line, "description") {
        workflow.summary = Some(now.to_owned());
    }
    for entry in array_at(line, "workflow_progress") {
        match str_at(entry, "type") {
            Some("workflow_phase") => phase(workflow, entry),
            Some("workflow_agent") => agent(workflow, entry),
            _ => {}
        }
    }
}

fn totals(workflow: &mut CliWorkflowRun, line: &Value) {
    let Some(usage) = line.get("usage") else {
        return;
    };
    workflow.tokens = u64_at(usage, "total_tokens").max(workflow.tokens);
    workflow.tool_calls = u32::try_from(u64_at(usage, "tool_uses"))
        .unwrap_or(u32::MAX)
        .max(workflow.tool_calls);
    workflow.duration_ms = u64_at(usage, "duration_ms").max(workflow.duration_ms);
}

/// A phase the script didn't declare in its `meta` still appears, in its place.
fn phase(workflow: &mut CliWorkflowRun, entry: &Value) {
    let (Some(index), Some(title)) = (index_at(entry, "index"), str_at(entry, "title")) else {
        return;
    };
    match workflow
        .phases
        .iter_mut()
        .find(|known| known.index == index)
    {
        Some(known) => title.clone_into(&mut known.title),
        None => {
            workflow.phases.push(CliPhase {
                index,
                title: title.to_owned(),
                detail: String::new(),
            });
            workflow.phases.sort_by_key(|known| known.index);
        }
    }
}

fn agent(workflow: &mut CliWorkflowRun, entry: &Value) {
    let Some(index) = index_at(entry, "index") else {
        return;
    };
    // An agent whose phase the report names only by title takes that phase's index.
    let phase = index_at(entry, "phaseIndex").or_else(|| {
        let title = str_at(entry, "phaseTitle")?;
        workflow
            .phases
            .iter()
            .find(|phase| phase.title == title)
            .map(|phase| phase.index)
    });
    // An agent waiting for a slot is reported as started, with when it was queued and no
    // start time yet. Claude Code's own view reads that as queued, and so does this.
    let known = |key: &str| entry.get(key).is_some_and(|value| !value.is_null());
    let waits_for_a_slot = known("queuedAt") && !known("startedAt");
    let flagged = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
    // An error says with a flag when nothing went wrong in the agent itself: you skipped it,
    // or it was refused before it started. Claude Code's own view reads them in this order.
    let state = match agent_state(str_at(entry, "state")) {
        CliAgentState::Running | CliAgentState::Other if waits_for_a_slot => CliAgentState::Queued,
        CliAgentState::Failed if flagged("skipped") => CliAgentState::Skipped,
        CliAgentState::Failed if flagged("blocked") => CliAgentState::Blocked,
        state => state,
    };
    let agent = CliAgent {
        index,
        label: string_at(entry, "label").unwrap_or_else(|| format!("agent:{index}")),
        phase,
        model: string_at(entry, "model"),
        state,
        tokens: u64_at(entry, "tokens"),
        tool_calls: u32::try_from(u64_at(entry, "toolCalls")).unwrap_or(u32::MAX),
        duration_ms: u64_at(entry, "durationMs"),
        started_at_ms: entry.get("startedAt").and_then(Value::as_u64),
        try_number: u32::try_from(u64_at(entry, "attempt").max(1)).unwrap_or(1),
        prompt: str_at(entry, "promptPreview").map(|text| shorten(text, PROMPT_CHARS)),
        result: str_at(entry, "resultPreview").map(|text| shorten(text, RESULT_CHARS)),
        agent_type: string_at(entry, "agentType"),
        isolation: string_at(entry, "isolation"),
        fallback_model: string_at(entry, "fallbackModel"),
        last_tool: string_at(entry, "lastToolName").map(|name| CliTool {
            name,
            summary: str_at(entry, "lastToolSummary").map(|text| shorten(text, TOOL_CHARS)),
        }),
        error: str_at(entry, "error").map(|text| shorten(text, RESULT_CHARS)),
    };
    match workflow
        .agents
        .iter_mut()
        .find(|known| known.index == index)
    {
        Some(known) => *known = agent,
        None => workflow.agents.push(agent),
    }
}

fn index_at(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn agent_state(state: Option<&str>) -> CliAgentState {
    match state {
        Some("queued" | "pending" | "waiting") => CliAgentState::Queued,
        Some("start" | "started" | "running") => CliAgentState::Running,
        Some("done" | "completed" | "success") => CliAgentState::Done,
        Some("failed" | "error" | "killed" | "cancelled" | "stopped") => CliAgentState::Failed,
        _ => CliAgentState::Other,
    }
}

fn task_status(status: &str) -> Option<SubagentStatus> {
    match status {
        "running" | "pending" => Some(SubagentStatus::Running),
        "completed" => Some(SubagentStatus::Done),
        "failed" | "killed" | "stopped" | "cancelled" | "error" => Some(SubagentStatus::Failed),
        _ => None,
    }
}

fn shorten(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_owned(),
    }
}

/// The tool that starts a workflow script.
pub const TOOL: &str = "Workflow";

/// How 2.1.289 to 2.1.295 begin the tool's answer once the script runs, here or remotely.
const LAUNCHED: &str = "Workflow launched";

/// Whether a `Workflow` call's result only says the script started. Its task id, run id, paths
/// and resume hints are for the model; the workflow's own reports draw it, and the raw log
/// keeps the text.
pub fn launched(structured: Option<&Value>, output: Option<&str>) -> bool {
    let status = structured.and_then(|result| str_at(result, "status"));
    matches!(status, Some("async_launched" | "remote_launched"))
        || output.is_some_and(|text| text.trim_start().starts_with(LAUNCHED))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workflow(event: Option<AgentEvent>) -> CliWorkflowRun {
        match event {
            Some(AgentEvent::Workflow { workflow }) => *workflow,
            other => panic!("expected a workflow, got {other:?}"),
        }
    }

    #[test]
    fn other_tasks_are_left_to_the_subagent_tracker() {
        let mut tracker = WorkflowTracker::default();
        let agent = json!({"task_id": "a1", "task_type": "local_agent", "tool_use_id": "t1"});
        assert_eq!(tracker.task_event("task_started", &agent), None);
        let progress = json!({"task_id": "a1", "description": "reading"});
        assert_eq!(tracker.task_event("task_progress", &progress), None);
    }

    #[test]
    fn names_come_from_the_event_when_the_script_has_no_meta() {
        let mut tracker = WorkflowTracker::default();
        let started = json!({"task_id": "w1", "task_type": "local_workflow",
            "workflow_name": "sort-issues", "description": "Sort the issues",
            "prompt": "await agent('go', { label: 'sort:all', phase: 'Sort' })"});
        let started = workflow(tracker.task_event("task_started", &started));
        assert_eq!(
            (started.name.as_str(), started.description.as_str()),
            ("sort-issues", "Sort the issues")
        );
        assert!(started.phases.is_empty());

        // Phases and agents arrive with the progress, by title when the index is missing.
        let progress = json!({"task_id": "w1", "description": "Sort: sort:all",
            "usage": {"total_tokens": 5, "tool_uses": 1, "duration_ms": 9},
            "workflow_progress": [
                {"type": "workflow_phase", "index": 1, "title": "Sort"},
                {"type": "workflow_agent", "index": 1, "label": "sort:all", "phaseTitle": "Sort",
                 "state": "queued"}]});
        let now = workflow(tracker.task_event("task_progress", &progress));
        assert_eq!(now.phases[0].title, "Sort");
        assert_eq!(now.agents[0].phase, Some(1));
        assert_eq!(now.agents[0].state, CliAgentState::Queued);
        assert_eq!((now.tokens, now.tool_calls, now.duration_ms), (5, 1, 9));
        // The same report again changes nothing.
        assert_eq!(tracker.task_event("task_progress", &progress), None);

        let failed = json!({"task_id": "w1", "patch": {"status": "failed"}});
        let ended = workflow(tracker.task_event("task_updated", &failed));
        assert_eq!(ended.status, SubagentStatus::Failed);
    }

    /// Claude Code reports an agent as started from the moment it is queued, and gives it a
    /// start time only once a slot is free. Until then it reads as queued, as in Claude Code's
    /// own view of the workflow.
    #[test]
    fn an_agent_with_no_start_time_yet_waits_for_a_slot() {
        let mut tracker = WorkflowTracker::default();
        let started = json!({"task_id": "w1", "task_type": "local_workflow",
            "workflow_name": "review-all", "prompt": ""});
        tracker.task_event("task_started", &started);
        let report = |agent: Value| {
            json!({"task_id": "w1", "workflow_progress": [
                {"type": "workflow_phase", "index": 1, "title": "Review"}, agent]})
        };
        let queued = report(
            json!({"type": "workflow_agent", "index": 1, "label": "review:one",
            "phaseIndex": 1, "state": "start", "queuedAt": 1_791_076_604_609_u64}),
        );
        let now = workflow(tracker.task_event("task_progress", &queued));
        assert_eq!(now.agents[0].state, CliAgentState::Queued);

        let running = report(
            json!({"type": "workflow_agent", "index": 1, "label": "review:one",
            "phaseIndex": 1, "state": "progress", "queuedAt": 1_791_076_604_609_u64,
            "startedAt": 1_791_076_604_610_u64}),
        );
        let now = workflow(tracker.task_event("task_progress", &running));
        assert_ne!(now.agents[0].state, CliAgentState::Queued);

        // One that failed before it got a slot failed, whatever its times say.
        let failed = report(
            json!({"type": "workflow_agent", "index": 1, "label": "review:one",
            "phaseIndex": 1, "state": "error", "queuedAt": 1_791_076_604_609_u64}),
        );
        let now = workflow(tracker.task_event("task_progress", &failed));
        assert_eq!(now.agents[0].state, CliAgentState::Failed);
    }

    /// One report of each agent as Claude Code 2.1.296 writes them, with `more` beside what
    /// every agent has.
    fn agent_reported(more: Value) -> CliAgent {
        let mut tracker = WorkflowTracker::default();
        let started = json!({"task_id": "w1", "task_type": "local_workflow",
            "tool_use_id": "toolu_1", "workflow_name": "review-twice"});
        tracker.task_event("task_started", &started);
        let mut agent = json!({"type": "workflow_agent", "index": 1, "label": "review:one",
            "phaseIndex": 1, "model": "claude-opus-5-5", "startedAt": 1_791_076_604_610_u64,
            "tokens": 21_400, "toolCalls": 14});
        agent
            .as_object_mut()
            .unwrap()
            .extend(more.as_object().unwrap().clone());
        let report = json!({"task_id": "w1", "workflow_progress": [
            {"type": "workflow_phase", "index": 1, "title": "Review"}, agent]});
        workflow(tracker.task_event("task_progress", &report))
            .agents
            .remove(0)
    }

    #[test]
    fn an_agent_at_work_says_its_last_tool_and_where_it_works() {
        let agent = agent_reported(json!({"state": "progress", "agentType": "Explore",
            "isolation": "worktree", "fallbackModel": "claude-sonnet-5-5",
            "lastToolName": "Read", "lastToolSummary": "src/cli.js"}));
        assert_eq!(agent.state, CliAgentState::Other);
        assert_eq!(agent.agent_type.as_deref(), Some("Explore"));
        assert_eq!(agent.isolation.as_deref(), Some("worktree"));
        assert_eq!(agent.fallback_model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            agent.last_tool,
            Some(CliTool {
                name: "Read".into(),
                summary: Some("src/cli.js".into())
            })
        );
        assert_eq!(agent.error, None);

        // A tool called with nothing to say of it has a name alone, and an agent that has
        // called none yet has no last tool.
        let bare = agent_reported(json!({"state": "progress", "lastToolName": "TodoWrite"}));
        assert_eq!(
            bare.last_tool,
            Some(CliTool {
                name: "TodoWrite".into(),
                summary: None
            })
        );
        let none = agent_reported(json!({"state": "start"}));
        assert_eq!(
            (none.last_tool, none.agent_type, none.isolation),
            (None, None, None)
        );
    }

    /// Claude Code ends an agent that didn't finish with one state, `error`, and says with a
    /// flag when it was skipped or blocked. Each keeps the words it ended with.
    #[test]
    fn an_agent_skipped_or_blocked_is_not_one_that_failed() {
        let failed = agent_reported(json!({"state": "error",
            "error": "stalled — no progress for 180000ms"}));
        assert_eq!(failed.state, CliAgentState::Failed);
        assert_eq!(
            failed.error.as_deref(),
            Some("stalled — no progress for 180000ms")
        );

        let skipped = agent_reported(json!({"state": "error", "skipped": true,
            "error": "skipped by user"}));
        assert_eq!(skipped.state, CliAgentState::Skipped);

        let blocked = agent_reported(json!({"state": "error", "blocked": true,
            "error": "[review:one] blocked by safety classifier: asks for a secret"}));
        assert_eq!(blocked.state, CliAgentState::Blocked);
        assert_eq!(
            blocked.error.as_deref(),
            Some("[review:one] blocked by safety classifier: asks for a secret")
        );

        // Skipped is read before blocked, as Claude Code's own view reads them, and a flag
        // that isn't set changes nothing.
        let both = agent_reported(json!({"state": "error", "skipped": true, "blocked": true}));
        assert_eq!(both.state, CliAgentState::Skipped);
        let neither = agent_reported(json!({"state": "error", "skipped": false}));
        assert_eq!(neither.state, CliAgentState::Failed);
    }
}
