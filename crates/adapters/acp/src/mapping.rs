//! ACP session updates to `AgentEvent`s. Updates the driver checks, the command list, and
//! chunks that aren't text give no event, and the raw log keeps them. Anything else without a
//! row here, `_meta` extensions included, is kept raw as `Unknown`, which counts toward the
//! sign that the agent's format changed.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Plan, PlanEntryStatus, SessionUpdate, ToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, UsageUpdate,
};
use argonauts_core::{AgentEvent, TodoItem, TodoStatus};
use serde_json::Value;

pub(crate) struct Mapper {
    cwd: PathBuf,
    /// Cost comes from `usage_update` only when the profile lists it as a usage source.
    cost_from_usage_update: bool,
    text: String,
    message_id: Option<String>,
    last_message: Option<String>,
    started: HashSet<String>,
    running: BTreeSet<String>,
    rejected: HashSet<String>,
    /// Each running tool's proposed file changes, emitted only if it completes.
    changes: HashMap<String, Vec<(String, &'static str)>>,
    /// The session's cost so far in dollars. ACP reports it cumulatively, and `Usage` events
    /// without the cumulative flag add up, so each event carries the difference.
    cost: f64,
}

impl Mapper {
    pub fn new(cwd: &Path, cost_from_usage_update: bool) -> Self {
        Self {
            cwd: cwd.to_owned(),
            cost_from_usage_update,
            text: String::new(),
            message_id: None,
            last_message: None,
            started: HashSet::new(),
            running: BTreeSet::new(),
            rejected: HashSet::new(),
            changes: HashMap::new(),
            cost: 0.0,
        }
    }

    /// The events for one `session/update`'s `update` object.
    pub fn update(&mut self, update: &Value) -> Vec<AgentEvent> {
        let unknown = || AgentEvent::Unknown {
            raw: update.clone(),
        };
        let Ok(parsed) = serde_json::from_value::<SessionUpdate>(update.clone()) else {
            return vec![unknown()];
        };
        let mut events = Vec::new();
        match parsed {
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let Some(text) = text_of(&chunk.content) {
                    self.message_chunk(&chunk, text, &mut events);
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let Some(text) = text_of(&chunk.content) {
                    events.push(AgentEvent::Reasoning {
                        text: text.to_owned(),
                    });
                }
            }
            SessionUpdate::ToolCall(call) => self.tool_call(call, &mut events),
            SessionUpdate::ToolCallUpdate(update) => self.tool_update(update, &mut events),
            SessionUpdate::Plan(plan) => events.push(todo(&plan)),
            SessionUpdate::UsageUpdate(usage) => self.usage(&usage, &mut events),
            SessionUpdate::CurrentModeUpdate(_)
            | SessionUpdate::ConfigOptionUpdate(_)
            | SessionUpdate::SessionInfoUpdate(_)
            | SessionUpdate::AvailableCommandsUpdate(_) => {}
            _ => events.push(unknown()),
        }
        events
    }

    /// At the end of a turn or of the session: every tool still open fails, since no update
    /// will finish it now, and the pending message goes out.
    pub fn end_turn(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for id in std::mem::take(&mut self.running) {
            self.changes.remove(&id);
            events.push(AgentEvent::ToolFinished {
                id,
                ok: false,
                output: None,
                parent: None,
            });
        }
        self.rejected.clear();
        events.extend(self.flush());
        events
    }

    /// A tool whose permission request was rejected never runs, so nothing waits for it. The
    /// agent may never say so, and then the turn's end closes it.
    pub fn reject(&mut self, id: &str) {
        self.rejected.insert(id.to_owned());
    }

    fn flush(&mut self) -> Vec<AgentEvent> {
        if self.text.is_empty() {
            return Vec::new();
        }
        let text = std::mem::take(&mut self.text);
        self.last_message = Some(text.clone());
        vec![AgentEvent::Message { text, parent: None }]
    }

    pub fn last_message(&self) -> Option<&str> {
        self.last_message.as_deref()
    }

    pub fn tools_running(&self) -> bool {
        self.running.iter().any(|id| !self.rejected.contains(id))
    }

    fn message_chunk(&mut self, chunk: &ContentChunk, text: &str, events: &mut Vec<AgentEvent>) {
        let id = chunk.message_id.as_ref().map(|id| id.0.to_string());
        if id != self.message_id {
            events.extend(self.flush());
            self.message_id = id;
        }
        self.text.push_str(text);
        events.push(AgentEvent::TextDelta {
            text: text.to_owned(),
            parent: None,
        });
    }

    fn tool_call(&mut self, call: ToolCall, events: &mut Vec<AgentEvent>) {
        let id = call.tool_call_id.0.to_string();
        let name = call.name.clone().unwrap_or_else(|| call.title.clone());
        self.start_tool(&id, name, call.raw_input.clone(), events);
        self.note_changes(&id, &call.content);
        if let Some(ok) = finished(call.status) {
            let output = tool_output(call.raw_output.as_ref(), &call.content);
            self.finish_tool(&id, ok, output, events);
        }
    }

    fn tool_update(&mut self, update: ToolCallUpdate, events: &mut Vec<AgentEvent>) {
        let id = update.tool_call_id.0.to_string();
        let fields = update.fields;
        if !self.started.contains(&id) {
            let name = fields
                .name
                .clone()
                .or_else(|| fields.title.clone())
                .unwrap_or_else(|| id.clone());
            self.start_tool(&id, name, fields.raw_input.clone(), events);
        }
        let content = fields.content.unwrap_or_default();
        self.note_changes(&id, &content);
        if let Some(ok) = fields.status.and_then(finished) {
            let output = tool_output(fields.raw_output.as_ref(), &content);
            self.finish_tool(&id, ok, output, events);
        }
    }

    fn start_tool(
        &mut self,
        id: &str,
        name: String,
        input: Option<Value>,
        events: &mut Vec<AgentEvent>,
    ) {
        if !self.started.insert(id.to_owned()) {
            return;
        }
        self.running.insert(id.to_owned());
        events.push(AgentEvent::ToolStarted {
            id: id.to_owned(),
            name,
            input: input.unwrap_or(Value::Null),
            parent: None,
        });
    }

    fn finish_tool(
        &mut self,
        id: &str,
        ok: bool,
        output: Option<String>,
        events: &mut Vec<AgentEvent>,
    ) {
        let changes = self.changes.remove(id).unwrap_or_default();
        if self.running.remove(id) {
            if ok {
                events.extend(
                    changes
                        .into_iter()
                        .map(|(path, kind)| AgentEvent::FileChanged {
                            path,
                            kind: kind.to_owned(),
                        }),
                );
            }
            events.push(AgentEvent::ToolFinished {
                id: id.to_owned(),
                ok,
                output,
                parent: None,
            });
        }
    }

    /// A hint only: the diff you review still comes from the worktree. An edit the agent
    /// proposes in a permission request may be rejected, so it counts once the tool completes.
    fn note_changes(&mut self, id: &str, content: &[ToolCallContent]) {
        if !self.running.contains(id) {
            return;
        }
        for item in content {
            if let ToolCallContent::Diff(diff) = item {
                let path = diff.path.strip_prefix(&self.cwd).unwrap_or(&diff.path);
                let path = path.display().to_string();
                let kind = if diff.old_text.is_none() {
                    "add"
                } else {
                    "update"
                };
                let changes = self.changes.entry(id.to_owned()).or_default();
                if !changes.iter().any(|(seen, _)| *seen == path) {
                    changes.push((path, kind));
                }
            }
        }
    }

    fn usage(&mut self, usage: &UsageUpdate, events: &mut Vec<AgentEvent>) {
        events.push(AgentEvent::ContextFill {
            used: usage.used,
            size: (usage.size > 0).then_some(usage.size),
        });
        let Some(cost) = usage.cost.as_ref().filter(|_| self.cost_from_usage_update) else {
            return;
        };
        if !cost.currency.eq_ignore_ascii_case("USD") || cost.amount <= self.cost {
            return;
        }
        let spent = cost.amount - self.cost;
        self.cost = cost.amount;
        events.push(AgentEvent::Usage {
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cost_usd: Some(spent),
            cumulative: false,
        });
    }
}

/// The `usage` draft some agents put in the `session/prompt` answer, for that turn.
pub(crate) fn end_turn_usage(result: &Value) -> Option<AgentEvent> {
    let usage = result.get("usage").filter(|usage| usage.is_object())?;
    let count = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| usage.get(*name).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    Some(AgentEvent::Usage {
        input_tokens: count(&["inputTokens", "input_tokens"]),
        output_tokens: count(&["outputTokens", "output_tokens"]),
        cached_tokens: count(&["cachedReadTokens", "cached_read_tokens"]),
        cost_usd: None,
        cumulative: false,
    })
}

fn text_of(content: &ContentBlock) -> Option<&str> {
    match content {
        ContentBlock::Text(text) => Some(&text.text),
        _ => None,
    }
}

fn finished(status: ToolCallStatus) -> Option<bool> {
    match status {
        ToolCallStatus::Completed => Some(true),
        ToolCallStatus::Failed => Some(false),
        _ => None,
    }
}

fn tool_output(raw: Option<&Value>, content: &[ToolCallContent]) -> Option<String> {
    match raw {
        Some(Value::String(text)) => return Some(text.clone()),
        Some(value) if !value.is_null() => return Some(value.to_string()),
        _ => {}
    }
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|item| match item {
            ToolCallContent::Content(content) => text_of(&content.content),
            _ => None,
        })
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

fn todo(plan: &Plan) -> AgentEvent {
    let items = plan
        .entries
        .iter()
        .map(|entry| TodoItem {
            text: entry.content.clone(),
            status: match entry.status {
                PlanEntryStatus::InProgress => TodoStatus::InProgress,
                PlanEntryStatus::Completed => TodoStatus::Completed,
                _ => TodoStatus::Pending,
            },
        })
        .collect();
    AgentEvent::Todo { items }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn acp_updates_map_to_agent_events() {
        let mut mapper = Mapper::new(Path::new("/w"), true);
        let mut events = Vec::new();
        let updates = [
            json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "Plan first." } }),
            json!({ "sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": { "type": "text", "text": "Hello" } }),
            json!({ "sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": { "type": "text", "text": " there" } }),
            json!({ "sessionUpdate": "agent_message_chunk", "messageId": "m2", "content": { "type": "text", "text": "Next." } }),
            json!({
                "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit src/a.rs", "name": "patch",
                "kind": "edit", "status": "pending", "rawInput": { "path": "src/a.rs" },
                "content": [{ "type": "diff", "path": "/w/src/a.rs", "oldText": "a", "newText": "b" }],
            }),
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "in_progress" }),
            json!({
                "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "patched" } }],
            }),
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed" }),
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t2", "title": "Run tests", "status": "failed", "rawOutput": { "exit": 1 } }),
            json!({ "sessionUpdate": "tool_call", "toolCallId": "t3", "title": "Write new.md", "kind": "edit", "status": "in_progress",
                    "content": [{ "type": "diff", "path": "/elsewhere/new.md", "newText": "x" }] }),
            json!({ "sessionUpdate": "plan", "entries": [
                { "content": "Read", "priority": "high", "status": "completed" },
                { "content": "Fix", "priority": "medium", "status": "in_progress" },
                { "content": "Test", "priority": "low", "status": "pending" },
            ] }),
            json!({ "sessionUpdate": "usage_update", "used": 1200, "size": 200000, "cost": { "amount": 0.25, "currency": "USD" } }),
            json!({ "sessionUpdate": "usage_update", "used": 1500, "size": 200000, "cost": { "amount": 0.75, "currency": "USD" } }),
            json!({ "sessionUpdate": "usage_update", "used": 1600, "size": 0, "cost": { "amount": 3.0, "currency": "EUR" } }),
            json!({ "sessionUpdate": "available_commands_update", "availableCommands": [] }),
            json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "replayed" } }),
            json!({ "sessionUpdate": "something_new", "_meta": { "vendor": 1 } }),
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "image", "data": "", "mimeType": "image/png" } }),
            json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "image", "data": "", "mimeType": "image/png" } }),
            json!({ "sessionUpdate": "current_mode_update", "currentModeId": "default" }),
            json!({ "sessionUpdate": "config_option_update", "configOptions": [] }),
            json!({ "sessionUpdate": "session_info_update", "title": "Fix the bug" }),
        ];
        for update in &updates {
            events.extend(mapper.update(update));
        }
        assert!(mapper.tools_running(), "t3 is still running");
        events.extend(mapper.end_turn());
        assert!(!mapper.tools_running());
        // An update for a tool the turn's end closed is ignored.
        assert_eq!(mapper.update(&updates[9]), []);

        let unknown = |index: usize| AgentEvent::Unknown {
            raw: updates[index].clone(),
        };
        let expected = vec![
            AgentEvent::Reasoning {
                text: "Plan first.".into(),
            },
            AgentEvent::TextDelta {
                text: "Hello".into(),
                parent: None,
            },
            AgentEvent::TextDelta {
                text: " there".into(),
                parent: None,
            },
            AgentEvent::Message {
                text: "Hello there".into(),
                parent: None,
            },
            AgentEvent::TextDelta {
                text: "Next.".into(),
                parent: None,
            },
            AgentEvent::ToolStarted {
                id: "t1".into(),
                name: "patch".into(),
                input: json!({ "path": "src/a.rs" }),
                parent: None,
            },
            AgentEvent::FileChanged {
                path: "src/a.rs".into(),
                kind: "update".into(),
            },
            AgentEvent::ToolFinished {
                id: "t1".into(),
                ok: true,
                output: Some("patched".into()),
                parent: None,
            },
            AgentEvent::ToolStarted {
                id: "t2".into(),
                name: "Run tests".into(),
                input: Value::Null,
                parent: None,
            },
            AgentEvent::ToolFinished {
                id: "t2".into(),
                ok: false,
                output: Some(r#"{"exit":1}"#.into()),
                parent: None,
            },
            AgentEvent::ToolStarted {
                id: "t3".into(),
                name: "Write new.md".into(),
                input: Value::Null,
                parent: None,
            },
            AgentEvent::Todo {
                items: vec![
                    TodoItem {
                        text: "Read".into(),
                        status: TodoStatus::Completed,
                    },
                    TodoItem {
                        text: "Fix".into(),
                        status: TodoStatus::InProgress,
                    },
                    TodoItem {
                        text: "Test".into(),
                        status: TodoStatus::Pending,
                    },
                ],
            },
            AgentEvent::ContextFill {
                used: 1200,
                size: Some(200_000),
            },
            AgentEvent::Usage {
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                cost_usd: Some(0.25),
                cumulative: false,
            },
            AgentEvent::ContextFill {
                used: 1500,
                size: Some(200_000),
            },
            AgentEvent::Usage {
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                cost_usd: Some(0.5),
                cumulative: false,
            },
            AgentEvent::ContextFill {
                used: 1600,
                size: None,
            },
            unknown(15),
            unknown(16),
            AgentEvent::ToolFinished {
                id: "t3".into(),
                ok: false,
                output: None,
                parent: None,
            },
            AgentEvent::Message {
                text: "Next.".into(),
                parent: None,
            },
        ];
        assert_eq!(events, expected);
        assert_eq!(mapper.last_message(), Some("Next."));

        let result = json!({ "stopReason": "end_turn", "usage": {
            "inputTokens": 900, "outputTokens": 40, "cachedReadTokens": 600, "totalTokens": 940 } });
        assert_eq!(
            end_turn_usage(&result),
            Some(AgentEvent::Usage {
                input_tokens: 900,
                output_tokens: 40,
                cached_tokens: 600,
                cost_usd: None,
                cumulative: false,
            })
        );
        assert_eq!(end_turn_usage(&json!({ "stopReason": "end_turn" })), None);

        let mut without_cost = Mapper::new(Path::new("/w"), false);
        assert_eq!(
            without_cost.update(&updates[11]),
            vec![AgentEvent::ContextFill {
                used: 1200,
                size: Some(200_000)
            }]
        );
    }
}
