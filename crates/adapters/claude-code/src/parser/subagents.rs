//! Rebuilds the subagent tree. A subagent is the `Agent` call that started it, and its own
//! messages carry that call's id as `parent_tool_use_id` at every depth (2.1.219 and later).
//! Under `-p` Claude usually runs them in the background: the call returns at once, and the
//! `system/task_*` events report progress and the end, with the tokens it has used so far.

use std::collections::HashMap;

use argonauts_core::AgentEvent;
use argonauts_core::runs::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use serde_json::Value;

use crate::parser::json::{bool_at, str_at, string_at, u64_at};

/// `Task` is the tool's older name, which `init` still lists in 2.1.285.
const SPAWN_TOOLS: &[&str] = &["Agent", "Task"];

/// A shell command running in the background, as `task_type` names it.
const SHELL_TASK: &str = "local_bash";

/// The statuses a task ends with when you, or the main agent, stopped it.
const STOPPED: &[&str] = &["killed", "stopped", "cancelled"];

const PROMPT_CHARS: usize = 200;
const MESSAGE_CHARS: usize = 400;

#[derive(Debug, Default)]
pub struct SubagentTracker {
    children: HashMap<String, Child>,
    /// `task_updated` names only the task, not the tool call.
    task_to_call: HashMap<String, String>,
}

#[derive(Debug)]
struct Child {
    parent: Option<String>,
    /// A workflow the `Workflow` tool started, whose agents' own messages don't stream.
    kind: SubagentKind,
    label: Option<String>,
    prompt: Option<String>,
    status: SubagentStatus,
    last_message: Option<String>,
    /// What its task's `usage` last counted, which is the number Claude Code's own list shows.
    tokens: Option<u64>,
}

impl Child {
    fn new(parent: Option<String>) -> Self {
        Self {
            parent,
            kind: SubagentKind::Subagent,
            label: None,
            prompt: None,
            status: SubagentStatus::Started,
            last_message: None,
            tokens: None,
        }
    }

    fn ended(&self) -> bool {
        matches!(self.status, SubagentStatus::Done | SubagentStatus::Failed)
    }
}

impl SubagentTracker {
    /// A tool call that starts a subagent. `parent` is the subagent making the call, if any.
    pub fn tool_started(
        &mut self,
        id: &str,
        name: &str,
        input: &Value,
        parent: Option<&str>,
    ) -> Option<AgentEvent> {
        if !SPAWN_TOOLS.contains(&name) || id.is_empty() {
            return None;
        }
        let child = Child {
            label: label_in(input),
            prompt: str_at(input, "prompt").map(|p| shorten(p, PROMPT_CHARS)),
            ..Child::new(parent.map(str::to_owned))
        };
        self.children.insert(id.to_owned(), child);
        self.update(id)
    }

    /// A message from inside subagent `id`, with its text when it has some.
    pub fn activity(&mut self, id: &str, text: Option<&str>) -> Option<AgentEvent> {
        // Seen only through its messages, as when the stream starts mid-session.
        let child = self
            .children
            .entry(id.to_owned())
            .or_insert_with(|| Child::new(None));
        let text = text.map(str::trim).filter(|t| !t.is_empty());
        if child.ended() || (child.status == SubagentStatus::Running && text.is_none()) {
            return None;
        }
        child.status = SubagentStatus::Running;
        if let Some(text) = text {
            child.last_message = Some(shorten(text, MESSAGE_CHARS));
        }
        self.update(id)
    }

    /// The task id Claude Code stops subagent `id` by, once it runs as a background task.
    pub fn task_of(&self, id: &str) -> Option<&str> {
        self.task_to_call
            .iter()
            .find(|(_, call)| call.as_str() == id)
            .map(|(task, _)| task.as_str())
    }

    /// The result of the call that started subagent `id`. A background launch returns at once
    /// with `async_launched`, and the subagent keeps running.
    pub fn tool_finished(
        &mut self,
        id: &str,
        ok: bool,
        output: Option<&str>,
        structured: Option<&Value>,
    ) -> Option<AgentEvent> {
        self.children.get(id)?;
        let launched = structured.is_some_and(|s| {
            bool_at(s, "isAsync") || str_at(s, "status") == Some("async_launched")
        });
        if launched && ok {
            return self.set(id, Some(SubagentStatus::Running), None, None);
        }
        let status = if ok {
            SubagentStatus::Done
        } else {
            SubagentStatus::Failed
        };
        self.set(id, Some(status), output, None)
    }

    /// `system/task_started`, `task_progress`, `task_updated`, and `task_notification`.
    /// `None` for a subtype this tracker doesn't read.
    pub fn task_event(&mut self, subtype: &str, line: &Value) -> Option<Option<AgentEvent>> {
        const TASK_EVENTS: &[&str] = &[
            "task_started",
            "task_progress",
            "task_updated",
            "task_notification",
        ];
        if !TASK_EVENTS.contains(&subtype) {
            return None;
        }
        // The task's first call names its subagent, whichever call an event carries.
        let known = str_at(line, "task_id").and_then(|task| self.task_to_call.get(task).cloned());
        let call = known.or_else(|| string_at(line, "tool_use_id"));
        let Some(call) = call else {
            return Some(None);
        };
        let event = match subtype {
            // A command a subagent left running in the background is a task too, but not one
            // of the agents.
            "task_started" if str_at(line, "task_type") == Some(SHELL_TASK) => None,
            "task_started" => {
                // A message resumes a finished subagent as the same task under the call that
                // sent it, so the task keeps the call that first started it.
                let call = match str_at(line, "task_id") {
                    Some(task) => self
                        .task_to_call
                        .entry(task.to_owned())
                        .or_insert(call)
                        .clone(),
                    None => call,
                };
                let child = self
                    .children
                    .entry(call.clone())
                    .or_insert_with(|| Child::new(None));
                if child.ended() {
                    child.status = SubagentStatus::Started;
                }
                if str_at(line, "task_type") == Some("local_workflow") {
                    child.kind = SubagentKind::Workflow;
                    if let Some(name) = str_at(line, "workflow_name") {
                        child.label = Some(name.to_owned());
                    }
                }
                if child.label.is_none() {
                    child.label = label_in(line);
                }
                if child.prompt.is_none() {
                    child.prompt = str_at(line, "prompt").map(|p| shorten(p, PROMPT_CHARS));
                }
                self.set(&call, Some(SubagentStatus::Running), None, None)
            }
            // Its description is a short note on what the subagent is doing now.
            "task_progress" => self.set(&call, None, str_at(line, "description"), tokens_in(line)),
            "task_updated" => {
                let status = line.get("patch").and_then(|patch| str_at(patch, "status"));
                // A stop says nothing more, so the row says it.
                let note = status
                    .filter(|status| STOPPED.contains(status))
                    .map(|_| "Stopped");
                self.set(&call, status.and_then(task_status), note, None)
            }
            _ => {
                let status = str_at(line, "status").and_then(task_status);
                self.set(&call, status, str_at(line, "summary"), tokens_in(line))
            }
        };
        Some(event)
    }

    /// Applies a change and reports it, or `None` when nothing changed. An ended subagent
    /// never runs again, and its count of tokens never goes back: one a message resumed counts
    /// on from where it stood, and the total a task ends with arrives after its end.
    fn set(
        &mut self,
        id: &str,
        status: Option<SubagentStatus>,
        message: Option<&str>,
        tokens: Option<u64>,
    ) -> Option<AgentEvent> {
        let child = self.children.get_mut(id)?;
        let mut changed = false;
        // What comes after its end is about what it left behind, such as a background command
        // of its own being stopped, and doesn't change how it ended.
        let stale = child.ended() && status.is_some_and(|status| status != child.status);
        if let Some(status) = status
            && status != child.status
            && !child.ended()
        {
            child.status = status;
            changed = true;
        }
        let message = message.filter(|_| !stale);
        if let Some(message) = message.map(str::trim).filter(|m| !m.is_empty()) {
            let message = shorten(message, MESSAGE_CHARS);
            if child.last_message.as_deref() != Some(message.as_str()) {
                child.last_message = Some(message);
                changed = true;
            }
        }
        // A workflow counts its own on its row.
        let counts = child.kind == SubagentKind::Subagent;
        if let Some(tokens) = tokens.filter(|tokens| counts && *tokens > child.tokens.unwrap_or(0))
        {
            child.tokens = Some(tokens);
            changed = true;
        }
        if changed { self.update(id) } else { None }
    }

    fn update(&self, id: &str) -> Option<AgentEvent> {
        let child = self.children.get(id)?;
        Some(AgentEvent::Subagent {
            update: SubagentUpdate {
                id: id.to_owned(),
                parent: child.parent.clone(),
                kind: child.kind,
                label: child.label.clone(),
                prompt: child.prompt.clone(),
                status: child.status,
                last_message: child.last_message.clone(),
                log_uri: None,
                // A subagent's tool calls and results stream with its id as their parent; a
                // workflow's agents report only through its progress.
                steps_visible: child.kind == SubagentKind::Subagent,
                tokens: child.tokens,
            },
        })
    }
}

fn label_in(value: &Value) -> Option<String> {
    str_at(value, "subagent_type")
        .or_else(|| str_at(value, "description"))
        .map(str::to_owned)
}

/// What a task's line says the subagent has used so far.
fn tokens_in(line: &Value) -> Option<u64> {
    line.get("usage").map(|usage| u64_at(usage, "total_tokens"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update(event: Option<AgentEvent>) -> SubagentUpdate {
        match event {
            Some(AgentEvent::Subagent { update }) => update,
            other => panic!("expected a subagent update, got {other:?}"),
        }
    }

    fn status(event: Option<AgentEvent>) -> (SubagentStatus, Option<String>) {
        let update = update(event);
        (update.status, update.last_message)
    }

    #[test]
    fn a_foreground_subagent_starts_runs_and_finishes() {
        let mut tracker = SubagentTracker::default();
        let input = json!({"description": "Read the readme", "subagent_type": "Explore",
                           "prompt": "x".repeat(300)});
        let started = update(tracker.tool_started("toolu_1", "Agent", &input, None));
        assert_eq!(started.label.as_deref(), Some("Explore"));
        assert_eq!(started.prompt.unwrap().chars().count(), PROMPT_CHARS + 1);
        assert_eq!(started.status, SubagentStatus::Started);
        assert!(started.parent.is_none() && started.steps_visible);

        assert_eq!(
            status(tracker.activity("toolu_1", None)),
            (SubagentStatus::Running, None)
        );
        // Messages without text after the first add nothing.
        assert!(tracker.activity("toolu_1", None).is_none());
        assert_eq!(
            status(tracker.activity("toolu_1", Some(" found it "))),
            (SubagentStatus::Running, Some("found it".into()))
        );
        assert_eq!(
            status(tracker.tool_finished("toolu_1", false, Some("boom"), None)),
            (SubagentStatus::Failed, Some("boom".into()))
        );
        assert!(tracker.activity("toolu_1", Some("late")).is_none());
    }

    #[test]
    fn a_background_subagent_ends_with_its_task_events() {
        let mut tracker = SubagentTracker::default();
        tracker.tool_started("toolu_a", "Agent", &json!({"description": "Read"}), None);
        let started = json!({"task_id": "t1", "tool_use_id": "toolu_a",
                             "subagent_type": "general-purpose", "is_backgrounded": true});
        assert_eq!(
            status(tracker.task_event("task_started", &started).unwrap()),
            (SubagentStatus::Running, None)
        );
        let launched = json!({"isAsync": true, "status": "async_launched", "agentId": "t1"});
        assert!(
            tracker
                .tool_finished(
                    "toolu_a",
                    true,
                    Some("Async agent launched"),
                    Some(&launched)
                )
                .is_none()
        );
        let progress = json!({"task_id": "t1", "tool_use_id": "toolu_a",
                              "description": "Reading README.md"});
        assert_eq!(
            status(tracker.task_event("task_progress", &progress).unwrap()),
            (SubagentStatus::Running, Some("Reading README.md".into()))
        );
        assert!(
            tracker
                .task_event("task_progress", &progress)
                .unwrap()
                .is_none()
        );
        let updated = json!({"task_id": "t1", "patch": {"status": "completed"}});
        assert_eq!(
            status(tracker.task_event("task_updated", &updated).unwrap()).0,
            SubagentStatus::Done
        );
        let note = json!({"task_id": "t1", "tool_use_id": "toolu_a", "status": "completed",
                          "summary": "The first line is # Throwaway"});
        assert_eq!(
            status(tracker.task_event("task_notification", &note).unwrap()),
            (
                SubagentStatus::Done,
                Some("The first line is # Throwaway".into())
            )
        );
        assert!(
            tracker
                .task_event("background_tasks_changed", &note)
                .is_none()
        );
        assert_eq!(tracker.task_of("toolu_a"), Some("t1"));
        assert_eq!(tracker.task_of("toolu_z"), None);
        let orphan = json!({"task_id": "t9", "patch": {"status": "completed"}});
        assert_eq!(tracker.task_event("task_updated", &orphan), Some(None));
    }

    /// The counts are ones 2.1.292 sent for a subagent that ran two commands and ended.
    #[test]
    fn a_subagents_tokens_follow_its_tasks_count() {
        let mut tracker = SubagentTracker::default();
        tracker.tool_started("toolu_a", "Agent", &json!({"description": "Read"}), None);
        let started = json!({"task_id": "t1", "tool_use_id": "toolu_a", "is_backgrounded": true});
        let begun = update(tracker.task_event("task_started", &started).unwrap());
        assert_eq!(begun.tokens, None);
        let progress = |tokens: u64| {
            json!({"task_id": "t1", "tool_use_id": "toolu_a", "description": "Reading README.md",
                   "usage": {"total_tokens": tokens, "tool_uses": 1, "duration_ms": 1632}})
        };
        let first = update(
            tracker
                .task_event("task_progress", &progress(11_823))
                .unwrap(),
        );
        assert_eq!(first.tokens, Some(11_823));
        // The same note with a higher count is news, and a count never goes back.
        let more = update(
            tracker
                .task_event("task_progress", &progress(12_812))
                .unwrap(),
        );
        assert_eq!(more.tokens, Some(12_812));
        assert_eq!(more.last_message.as_deref(), Some("Reading README.md"));
        let lower = tracker.task_event("task_progress", &progress(12_000));
        assert_eq!(lower, Some(None));
        // The total it ended with arrives after its end, and it stays ended.
        let done = json!({"task_id": "t1", "patch": {"status": "completed"}});
        tracker.task_event("task_updated", &done);
        let note = json!({"task_id": "t1", "tool_use_id": "toolu_a", "status": "completed",
            "usage": {"total_tokens": 17_051, "tool_uses": 6, "duration_ms": 17_259}});
        let ended = update(tracker.task_event("task_notification", &note).unwrap());
        assert_eq!(
            (ended.status, ended.tokens),
            (SubagentStatus::Done, Some(17_051))
        );
    }

    #[test]
    fn a_workflows_task_and_a_shell_commands_count_nothing_here() {
        let mut tracker = SubagentTracker::default();
        let usage = json!({"total_tokens": 900, "tool_uses": 1, "duration_ms": 9});
        let workflow = json!({"task_id": "w1", "tool_use_id": "toolu_w",
            "task_type": "local_workflow", "workflow_name": "review"});
        tracker.task_event("task_started", &workflow);
        let progress = json!({"task_id": "w1", "tool_use_id": "toolu_w", "usage": usage});
        assert_eq!(tracker.task_event("task_progress", &progress), Some(None));
        let shell = json!({"task_id": "b1", "tool_use_id": "toolu_b", "task_type": "local_bash"});
        tracker.task_event("task_started", &shell);
        let ran = json!({"task_id": "b1", "tool_use_id": "toolu_b", "usage": usage});
        assert_eq!(tracker.task_event("task_progress", &ran), Some(None));
    }

    #[test]
    fn nested_and_unknown_children() {
        let mut tracker = SubagentTracker::default();
        let nested = update(tracker.tool_started("toolu_2", "Task", &json!({}), Some("toolu_1")));
        assert_eq!(nested.parent.as_deref(), Some("toolu_1"));
        assert!(nested.label.is_none());
        assert!(
            tracker
                .tool_started("toolu_3", "Bash", &json!({}), None)
                .is_none()
        );
        assert!(tracker.tool_finished("toolu_3", true, None, None).is_none());
        assert_eq!(
            status(tracker.activity("toolu_9", None)),
            (SubagentStatus::Running, None)
        );
    }

    #[test]
    fn shorten_cuts_on_characters() {
        assert_eq!(shorten("héllo wörld", 5), "héllo…");
        assert_eq!(shorten("  short  ", 10), "short");
    }
}
