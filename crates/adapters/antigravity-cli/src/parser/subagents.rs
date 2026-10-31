//! Subagents from `subagent_info`, keyed by the child's conversation id.
//!
//! agy 1.2.13 reports a child twice on the `invoke_subagent` step: while launching, without an
//! id, then launched, with its `conversation_id` and `log_uri`. The child runs on its own and
//! its steps don't stream, so its log is the only view into it, and the turn's `result` is the
//! first sign that it ended.

use argonauts_core::AgentEvent;
use argonauts_core::runs::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use serde_json::Value;

/// Characters of the child's first prompt kept in its update.
const PROMPT_CHARS: usize = 200;

#[derive(Debug, Default)]
pub(crate) struct Subagents {
    /// In launch order.
    children: Vec<SubagentUpdate>,
}

impl Subagents {
    /// Updates from one step's `subagent_info`. `parent` is the invoking conversation when it
    /// isn't the main one, and `failed` is set when the invoking step ended in an error.
    pub fn on_info(&mut self, info: &Value, parent: Option<&str>, failed: bool) -> Vec<AgentEvent> {
        let entries = info
            .get("subagents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        let mut events = Vec::new();
        for entry in entries {
            let Some(id) = text(entry, "conversation_id") else {
                continue;
            };
            let status = if failed {
                SubagentStatus::Failed
            } else {
                SubagentStatus::Started
            };
            let seen = SubagentUpdate {
                id: id.clone(),
                parent: parent.map(str::to_owned),
                kind: SubagentKind::Subagent,
                label: text(entry, "role").or_else(|| text(entry, "type_name")),
                prompt: text(entry, "initial_prompt").map(|p| shorten(&p)),
                status,
                last_message: None,
                log_uri: text(entry, "log_uri"),
                steps_visible: false,
                tokens: None,
            };
            match self.children.iter_mut().find(|child| child.id == id) {
                None => {
                    self.children.push(seen.clone());
                    events.push(subagent_event(seen));
                }
                Some(known) => {
                    let changed = merge(known, seen);
                    if changed {
                        events.push(subagent_event(known.clone()));
                    }
                }
            }
        }
        events
    }

    /// A finished message from a child whose steps do stream.
    pub fn on_child_message(&mut self, conversation: &str, text: &str) -> Option<AgentEvent> {
        let child = self
            .children
            .iter_mut()
            .find(|child| child.id == conversation)?;
        child.status = SubagentStatus::Running;
        child.last_message = Some(shorten(text));
        child.steps_visible = true;
        Some(subagent_event(child.clone()))
    }

    /// Closes every child still running, when the turn ends or the process exits.
    pub fn close_running(&mut self, status: SubagentStatus) -> Vec<AgentEvent> {
        self.children
            .iter_mut()
            .filter(|child| is_running(child.status))
            .map(|child| {
                child.status = status;
                subagent_event(child.clone())
            })
            .collect()
    }
}

/// Whether a child counts against the fan-out cap.
fn is_running(status: SubagentStatus) -> bool {
    matches!(status, SubagentStatus::Started | SubagentStatus::Running)
}

/// Keeps what's already known and fills in what's new. A failure sticks.
fn merge(known: &mut SubagentUpdate, seen: SubagentUpdate) -> bool {
    let before = known.clone();
    if known.label.is_none() {
        known.label = seen.label;
    }
    if known.prompt.is_none() {
        known.prompt = seen.prompt;
    }
    if known.log_uri.is_none() {
        known.log_uri = seen.log_uri;
    }
    if seen.status == SubagentStatus::Failed {
        known.status = SubagentStatus::Failed;
    }
    *known != before
}

fn subagent_event(update: SubagentUpdate) -> AgentEvent {
    AgentEvent::Subagent { update }
}

fn text(entry: &Value, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn shorten(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(PROMPT_CHARS) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn launched(id: &str) -> Value {
        json!({"subagents": [{
            "type_name": "research",
            "role": "Notes Reader",
            "initial_prompt": "Read notes.txt",
            "conversation_id": id,
            "log_uri": format!("file://~/.gemini/antigravity-cli/brain/{id}/.system_generated/logs/transcript.jsonl"),
            "workspace_uris": ["file://<workdir>"],
        }]})
    }

    fn update(event: &AgentEvent) -> &SubagentUpdate {
        match event {
            AgentEvent::Subagent { update } => update,
            other => panic!("not a subagent event: {other:?}"),
        }
    }

    #[test]
    fn launching_without_an_id_is_skipped() {
        let mut subagents = Subagents::default();
        let launching = json!({"subagents": [{"type_name": "research", "initial_prompt": "x"}]});
        assert!(subagents.on_info(&launching, None, false).is_empty());
    }

    #[test]
    fn a_launched_child_starts_once_with_its_log() {
        let mut subagents = Subagents::default();
        let events = subagents.on_info(&launched("c2"), None, false);
        assert_eq!(events.len(), 1);
        let child = update(&events[0]);
        assert_eq!(child.id, "c2");
        assert_eq!(child.parent, None);
        assert_eq!(child.kind, SubagentKind::Subagent);
        assert_eq!(child.label.as_deref(), Some("Notes Reader"));
        assert_eq!(child.prompt.as_deref(), Some("Read notes.txt"));
        assert_eq!(child.status, SubagentStatus::Started);
        assert!(child.log_uri.as_deref().unwrap().contains("brain/c2/"));
        assert!(!child.steps_visible);

        assert!(subagents.on_info(&launched("c2"), None, false).is_empty());
    }

    #[test]
    fn nested_children_name_their_parent_and_failures_stick() {
        let mut subagents = Subagents::default();
        let events = subagents.on_info(&launched("c3"), Some("c2"), true);
        let child = update(&events[0]);
        assert_eq!(child.parent.as_deref(), Some("c2"));
        assert_eq!(child.status, SubagentStatus::Failed);
        assert!(subagents.close_running(SubagentStatus::Done).is_empty());
    }

    #[test]
    fn the_turn_end_closes_running_children_once() {
        let mut subagents = Subagents::default();
        subagents.on_info(&launched("c2"), None, false);
        let message = subagents.on_child_message("c2", "found it").unwrap();
        assert_eq!(update(&message).status, SubagentStatus::Running);
        assert_eq!(update(&message).last_message.as_deref(), Some("found it"));
        assert!(update(&message).steps_visible);
        assert!(subagents.on_child_message("other", "x").is_none());

        let closed = subagents.close_running(SubagentStatus::Done);
        assert_eq!(closed.len(), 1);
        assert_eq!(update(&closed[0]).status, SubagentStatus::Done);
        assert!(subagents.close_running(SubagentStatus::Failed).is_empty());
    }

    #[test]
    fn long_prompts_are_shortened_on_a_char_boundary() {
        let long = "é".repeat(PROMPT_CHARS + 50);
        let short = shorten(&long);
        assert_eq!(short.chars().count(), PROMPT_CHARS + 1);
        assert!(short.ends_with('…'));
        assert_eq!(shorten("  short  "), "short");
    }
}
