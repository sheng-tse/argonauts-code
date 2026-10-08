//! Codex subagents, seen only through the main thread's `collab_tool_call` items
//! (`spawn_agent`, `send_input`, `wait`, `close_agent`). Their own steps and tokens aren't
//! streamed, so each child shows as a thread with its status and last message.

use std::collections::BTreeMap;

use argonauts_core::AgentEvent;
use argonauts_core::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use serde::Deserialize;

/// Characters of a child's prompt kept for the agent panel.
const PROMPT_CHARS: usize = 160;

#[derive(Debug, Default)]
pub struct Threads {
    main: Option<String>,
    /// The latest update sent for each child, so only changes go out.
    sent: BTreeMap<String, SubagentUpdate>,
}

impl Threads {
    pub fn set_main(&mut self, thread_id: &str) {
        self.main = Some(thread_id.to_owned());
    }

    /// One update for each child whose state changed.
    pub fn observe(&mut self, call: &WireCollabCall) -> Vec<AgentEvent> {
        let parent = call
            .sender_thread_id
            .clone()
            .filter(|sender| Some(sender) != self.main.as_ref());
        let spawned: &[String] = if call.tool == "spawn_agent" {
            &call.receiver_thread_ids
        } else {
            &[]
        };
        let mut children: Vec<&String> = spawned.iter().collect();
        children.extend(call.agents_states.keys().filter(|c| !spawned.contains(c)));

        let mut events = Vec::new();
        for child in children {
            let previous = self.sent.get(child);
            let state = call.agents_states.get(child);
            let update = SubagentUpdate {
                id: child.clone(),
                parent: previous.map_or_else(|| parent.clone(), |p| p.parent.clone()),
                kind: SubagentKind::Thread,
                // codex-cli 0.158.0 names neither the agent type nor the nickname in exec output.
                label: None,
                prompt: previous.and_then(|p| p.prompt.clone()).or_else(|| {
                    spawned
                        .contains(child)
                        .then(|| shorten(call.prompt.as_deref()))
                        .flatten()
                }),
                status: match state {
                    Some(state) => status(&state.status, previous.map(|p| p.status)),
                    None => previous.map_or(SubagentStatus::Started, |p| p.status),
                },
                last_message: state
                    .and_then(|s| s.message.clone())
                    .filter(|m| !m.trim().is_empty())
                    .or_else(|| previous.and_then(|p| p.last_message.clone())),
                log_uri: None,
                steps_visible: false,
                tokens: None,
            };
            if previous != Some(&update) {
                self.sent.insert(child.clone(), update.clone());
                events.push(AgentEvent::Subagent { update });
            }
        }
        events
    }
}

/// codex-cli 0.158.0 child states: `pending_init`, `running`, `completed`, `errored`,
/// `shutdown` (closed), and `not_found`.
fn status(wire: &str, previous: Option<SubagentStatus>) -> SubagentStatus {
    match wire {
        "pending_init" => SubagentStatus::Started,
        "running" => SubagentStatus::Running,
        "completed" => SubagentStatus::Done,
        "errored" | "not_found" | "interrupted" => SubagentStatus::Failed,
        "shutdown" if previous == Some(SubagentStatus::Failed) => SubagentStatus::Failed,
        "shutdown" => SubagentStatus::Done,
        _ => SubagentStatus::Unknown,
    }
}

fn shorten(prompt: Option<&str>) -> Option<String> {
    let prompt = prompt?.trim();
    if prompt.is_empty() {
        return None;
    }
    let mut short: String = prompt.chars().take(PROMPT_CHARS).collect();
    if short.len() < prompt.len() {
        short.push('…');
    }
    Some(short)
}

#[derive(Debug, Deserialize)]
pub struct WireCollabCall {
    pub tool: String,
    pub sender_thread_id: Option<String>,
    #[serde(default)]
    pub receiver_thread_ids: Vec<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub agents_states: BTreeMap<String, WireAgentState>,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct WireAgentState {
    pub status: String,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(value: serde_json::Value) -> WireCollabCall {
        serde_json::from_value(value).unwrap()
    }

    fn updates(events: Vec<AgentEvent>) -> Vec<SubagentUpdate> {
        events
            .into_iter()
            .map(|event| match event {
                AgentEvent::Subagent { update } => update,
                other => panic!("not a subagent update: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_child_is_tracked_from_spawn_to_close() {
        let mut threads = Threads::default();
        threads.set_main("main");
        let spawn = updates(threads.observe(&call(json!({
            "tool": "spawn_agent", "sender_thread_id": "main", "receiver_thread_ids": ["c1"],
            "prompt": "count the files", "agents_states": {}, "status": "in_progress"
        }))));
        assert_eq!(spawn.len(), 1);
        assert_eq!(spawn[0].id, "c1");
        assert_eq!(spawn[0].parent, None);
        assert_eq!(spawn[0].kind, SubagentKind::Thread);
        assert_eq!(spawn[0].status, SubagentStatus::Started);
        assert_eq!(spawn[0].prompt.as_deref(), Some("count the files"));
        assert!(!spawn[0].steps_visible);

        // The same state again sends nothing.
        let again = threads.observe(&call(json!({
            "tool": "spawn_agent", "sender_thread_id": "main", "receiver_thread_ids": ["c1"],
            "prompt": "count the files", "status": "in_progress"
        })));
        assert!(again.is_empty());

        let done = updates(threads.observe(&call(json!({
            "tool": "wait", "sender_thread_id": "main", "receiver_thread_ids": ["c1"],
            "agents_states": {"c1": {"status": "completed", "message": "3"}}, "status": "completed"
        }))));
        assert_eq!(done[0].status, SubagentStatus::Done);
        assert_eq!(done[0].last_message.as_deref(), Some("3"));
        assert_eq!(done[0].prompt.as_deref(), Some("count the files"));
    }

    #[test]
    fn statuses_map_and_failures_stick_through_close() {
        assert_eq!(status("pending_init", None), SubagentStatus::Started);
        assert_eq!(status("running", None), SubagentStatus::Running);
        assert_eq!(status("errored", None), SubagentStatus::Failed);
        assert_eq!(
            status("shutdown", Some(SubagentStatus::Failed)),
            SubagentStatus::Failed
        );
        assert_eq!(
            status("shutdown", Some(SubagentStatus::Done)),
            SubagentStatus::Done
        );
        assert_eq!(status("sleeping", None), SubagentStatus::Unknown);
    }

    #[test]
    fn grandchildren_name_their_parent_and_prompts_are_shortened() {
        let mut threads = Threads::default();
        threads.set_main("main");
        let long = "x".repeat(400);
        let update = updates(threads.observe(&call(json!({
            "tool": "spawn_agent", "sender_thread_id": "c1", "receiver_thread_ids": ["c2"],
            "prompt": long, "status": "completed"
        }))));
        assert_eq!(update[0].parent.as_deref(), Some("c1"));
        let prompt = update[0].prompt.as_deref().unwrap();
        assert_eq!(prompt.chars().count(), PROMPT_CHARS + 1);
        assert!(prompt.ends_with('…'));
    }
}
