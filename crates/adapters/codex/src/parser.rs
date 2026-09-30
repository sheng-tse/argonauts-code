//! Codex prints one JSON object per line, such as `thread.started`, `turn.started`, `item.*`,
//! `turn.completed` (with session token totals), `turn.failed`, and `error`. Agent text arrives
//! only as whole messages. Subagents show up as `collab_tool_call` items; their own tool calls
//! are not streamed.

use std::collections::{BTreeMap, HashMap, VecDeque};

use argonauts_core::{AgentEvent, ErrorCategory, StreamParser, TodoItem, TodoStatus};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::classify::{classify_error, is_retry_notice, retry_reason};

const STDERR_TAIL_LINES: usize = 10;

#[derive(Debug, Default)]
pub struct CodexParser {
    /// Tool calls started but not finished, as (Codex item id, id used in `ToolStarted`).
    open_tools: Vec<(String, String)>,
    /// The latest complete agent message, which becomes the result text.
    last_message: Option<String>,
    /// The latest fatal error message, so a following `turn.failed` isn't reported twice.
    last_error: Option<String>,
    child_messages: HashMap<String, String>,
    stderr_tail: VecDeque<String>,
    turn_ended: bool,
}

impl CodexParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn on_turn_completed(&mut self, value: &Value) -> Vec<AgentEvent> {
        let mut events = Vec::with_capacity(2);
        if let Some(usage) = value
            .get("usage")
            .and_then(|u| WireUsage::deserialize(u).ok())
        {
            events.push(usage.into_event());
        }
        self.turn_ended = true;
        events.push(self.result(false));
        events
    }

    fn on_turn_failed(&mut self, value: &Value) -> Vec<AgentEvent> {
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("turn failed");
        let mut events = Vec::with_capacity(2);
        if self.last_error.as_deref() != Some(message) {
            events.push(error_event(message));
            self.last_error = Some(message.to_owned());
        }
        self.turn_ended = true;
        events.push(self.result(true));
        events
    }

    fn on_error(&mut self, value: &Value) -> Option<Vec<AgentEvent>> {
        let message = value.get("message")?.as_str()?;
        if is_retry_notice(message) {
            return Some(vec![AgentEvent::Retry {
                reason: retry_reason(message),
                delay_ms: None,
            }]);
        }
        self.last_error = Some(message.to_owned());
        Some(vec![error_event(message)])
    }

    fn on_item(&mut self, value: &Value, phase: Phase) -> Option<Vec<AgentEvent>> {
        let raw = value.get("item")?;
        let item_id = raw.get("id")?.as_str()?;
        let item = WireItem::deserialize(raw).ok()?;
        let done = phase == Phase::Completed;
        let mut events = Vec::new();

        match item {
            WireItem::AgentMessage { text } => {
                if done {
                    self.last_message = Some(text.clone());
                    events.push(AgentEvent::Message { text, parent: None });
                }
            }
            WireItem::Reasoning { text } => {
                if done {
                    events.push(AgentEvent::Reasoning { text });
                }
            }
            WireItem::CommandExecution(command) => {
                self.track(&mut events, command.into_call(item_id), phase);
            }
            WireItem::FileChange { changes, status } => {
                let ok = status == "completed";
                let input = json!({ "changes": raw.get("changes").cloned().unwrap_or_default() });
                let call = ToolCall::new(item_id, "file_change", input, ok, None);
                self.track(&mut events, call, phase);
                if done && ok {
                    events.extend(changes.into_iter().map(WireChange::into_event));
                }
            }
            WireItem::McpToolCall(mcp) => self.track(&mut events, mcp.into_call(item_id), phase),
            WireItem::CollabToolCall(collab) => {
                self.on_collab_call(&mut events, item_id, collab, phase);
            }
            WireItem::WebSearch { query, action } => {
                let input = json!({ "query": query, "action": action });
                let call = ToolCall::new(item_id, "web_search", input, true, None);
                self.track(&mut events, call, phase);
            }
            WireItem::TodoList { items } => {
                events.push(AgentEvent::Todo {
                    items: items.into_iter().map(WireTodo::into_item).collect(),
                });
            }
            WireItem::Error { message } => {
                // Error items are warnings, such as a model reroute. The turn goes on.
                events.push(error_event(&message));
            }
        }
        Some(events)
    }

    fn on_collab_call(
        &mut self,
        events: &mut Vec<AgentEvent>,
        item_id: &str,
        collab: WireCollabCall,
        phase: Phase,
    ) {
        // A spawned subagent is keyed by its thread id, which its messages use as parent.
        let tool_id = match collab.receiver_thread_ids.first() {
            Some(child) if collab.tool == "spawn_agent" => child.clone(),
            _ => item_id.to_owned(),
        };
        let call = ToolCall {
            item_id: item_id.to_owned(),
            tool_id,
            name: collab.tool,
            input: json!({
                "prompt": collab.prompt,
                "receiver_thread_ids": collab.receiver_thread_ids,
            }),
            ok: collab.status == "completed",
            output: summarize_agents(&collab.agents_states),
        };
        self.track(events, call, phase);
        events.extend(self.child_updates(collab.agents_states));
    }

    /// Emits `ToolStarted` the first time a call is seen and `ToolFinished` when it completes.
    fn track(&mut self, events: &mut Vec<AgentEvent>, call: ToolCall, phase: Phase) {
        let open = self
            .open_tools
            .iter()
            .position(|(item, _)| *item == call.item_id);
        let tool_id = match open {
            Some(index) if phase == Phase::Completed => self.open_tools.remove(index).1,
            Some(_) => return,
            None => {
                events.push(AgentEvent::ToolStarted {
                    id: call.tool_id.clone(),
                    name: call.name,
                    input: call.input,
                    parent: None,
                });
                if phase != Phase::Completed {
                    self.open_tools.push((call.item_id, call.tool_id));
                    return;
                }
                call.tool_id
            }
        };
        events.push(AgentEvent::ToolFinished {
            id: tool_id,
            ok: call.ok,
            output: call.output,
            parent: None,
        });
    }

    fn child_updates(&mut self, states: BTreeMap<String, WireAgentState>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for (thread, state) in states {
            let Some(text) = state.message.filter(|m| !m.trim().is_empty()) else {
                continue;
            };
            if self.child_messages.get(&thread) == Some(&text) {
                continue;
            }
            self.child_messages.insert(thread.clone(), text.clone());
            events.push(AgentEvent::Message {
                text,
                parent: Some(thread),
            });
        }
        events
    }

    fn result(&self, is_error: bool) -> AgentEvent {
        let text = if is_error {
            None
        } else {
            self.last_message.clone()
        };
        let structured = text.as_deref().and_then(structured_output);
        AgentEvent::Result {
            text,
            structured,
            is_error,
        }
    }

    fn exit_message(&self, exit_code: Option<i32>) -> String {
        if !self.stderr_tail.is_empty() {
            let lines: Vec<&str> = self.stderr_tail.iter().map(String::as_str).collect();
            return lines.join("\n");
        }
        match exit_code {
            Some(code) => format!("codex exited with code {code} before the turn finished"),
            None => "codex was stopped by a signal before the turn finished".to_owned(),
        }
    }
}

impl StreamParser for CodexParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![AgentEvent::unknown_line(line)];
        };
        let events = match value.get("type").and_then(Value::as_str) {
            Some("thread.started") => thread_started(&value),
            Some("turn.started") => Some(Vec::new()),
            Some("turn.completed") => Some(self.on_turn_completed(&value)),
            Some("turn.failed") => Some(self.on_turn_failed(&value)),
            Some("item.started") => self.on_item(&value, Phase::Started),
            Some("item.updated") => self.on_item(&value, Phase::Updated),
            Some("item.completed") => self.on_item(&value, Phase::Completed),
            Some("error") => self.on_error(&value),
            _ => None,
        };
        events.unwrap_or_else(|| vec![AgentEvent::Unknown { raw: value }])
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim_end();
        if line.trim().is_empty() {
            return Vec::new();
        }
        if self.stderr_tail.len() == STDERR_TAIL_LINES {
            self.stderr_tail.pop_front();
        }
        self.stderr_tail.push_back(line.to_owned());
        vec![AgentEvent::Unknown {
            raw: json!({ "stderr": line }),
        }]
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let mut events: Vec<AgentEvent> = self
            .open_tools
            .drain(..)
            .map(|(_, id)| AgentEvent::ToolFinished {
                id,
                ok: false,
                output: None,
                parent: None,
            })
            .collect();
        if self.turn_ended {
            return events;
        }
        // Without a closing event, Codex was interrupted, killed, or failed before the turn began.
        self.turn_ended = true;
        let failed = exit_code != Some(0);
        if failed && self.last_error.is_none() {
            events.push(exit_error(self.exit_message(exit_code), exit_code));
        }
        events.push(self.result(failed));
        events
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Started,
    Updated,
    Completed,
}

/// `ok` and `output` only count once the call completes.
struct ToolCall {
    item_id: String,
    tool_id: String,
    name: String,
    input: Value,
    ok: bool,
    output: Option<String>,
}

impl ToolCall {
    /// A call reported under its own Codex item id.
    fn new(
        item_id: &str,
        name: impl Into<String>,
        input: Value,
        ok: bool,
        output: Option<String>,
    ) -> Self {
        Self {
            item_id: item_id.to_owned(),
            tool_id: item_id.to_owned(),
            name: name.into(),
            input,
            ok,
            output,
        }
    }
}

/// Codex never names the model it ran, so `Init` carries only the session id.
fn thread_started(value: &Value) -> Option<Vec<AgentEvent>> {
    let session_id = value.get("thread_id")?.as_str()?.to_owned();
    Some(vec![AgentEvent::Init {
        session_id: Some(session_id),
        model: None,
        tools: Vec::new(),
    }])
}

fn error_event(message: &str) -> AgentEvent {
    let (category, retryable) = classify_error(message);
    AgentEvent::Error {
        category,
        message: message.to_owned(),
        retryable,
    }
}

fn exit_error(message: String, exit_code: Option<i32>) -> AgentEvent {
    let (category, retryable) = match (exit_code, classify_error(&message)) {
        (None, _) => (ErrorCategory::Interrupted, false),
        (Some(_), (ErrorCategory::Unknown, retryable)) => (ErrorCategory::Cli, retryable),
        (Some(_), classified) => classified,
    };
    AgentEvent::Error {
        category,
        message,
        retryable,
    }
}

fn structured_output(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .filter(Value::is_object)
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn summarize_agents(states: &BTreeMap<String, WireAgentState>) -> Option<String> {
    let lines: Vec<String> = states
        .iter()
        .map(|(thread, state)| format!("{thread}: {}", state.status))
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

impl WireUsage {
    /// Codex reports session totals. Input includes cached tokens and output includes reasoning,
    /// matching the core semantics.
    fn into_event(self) -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_tokens: self.cached_input_tokens,
            cost_usd: None,
            cumulative: true,
        }
    }
}

/// Statuses stay strings so new ones don't break parsing.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireItem {
    AgentMessage {
        text: String,
    },
    Reasoning {
        text: String,
    },
    CommandExecution(WireCommand),
    FileChange {
        changes: Vec<WireChange>,
        status: String,
    },
    McpToolCall(WireMcpCall),
    CollabToolCall(WireCollabCall),
    WebSearch {
        #[serde(default)]
        query: String,
        #[serde(default)]
        action: Value,
    },
    TodoList {
        items: Vec<WireTodo>,
    },
    Error {
        message: String,
    },
}

#[derive(Deserialize)]
struct WireCommand {
    command: String,
    #[serde(default)]
    aggregated_output: String,
    exit_code: Option<i32>,
    status: String,
}

impl WireCommand {
    fn into_call(self, item_id: &str) -> ToolCall {
        let ok = self.status == "completed" && self.exit_code.unwrap_or(0) == 0;
        let input = json!({ "command": self.command });
        let output = non_empty(self.aggregated_output);
        ToolCall::new(item_id, "command_execution", input, ok, output)
    }
}

#[derive(Deserialize)]
struct WireChange {
    path: String,
    kind: String,
}

impl WireChange {
    fn into_event(self) -> AgentEvent {
        AgentEvent::FileChanged {
            path: self.path,
            kind: self.kind,
        }
    }
}

#[derive(Deserialize)]
struct WireMcpCall {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: Value,
    result: Option<WireMcpResult>,
    error: Option<WireMessage>,
    status: String,
}

impl WireMcpCall {
    fn into_call(self, item_id: &str) -> ToolCall {
        let name = format!("mcp:{}/{}", self.server, self.tool);
        let ok = self.status == "completed" && self.error.is_none();
        let output = match self.error {
            Some(error) => Some(error.message),
            None => self.result.and_then(WireMcpResult::into_text),
        };
        ToolCall::new(item_id, name, self.arguments, ok, output)
    }
}

#[derive(Deserialize)]
struct WireCollabCall {
    tool: String,
    #[serde(default)]
    receiver_thread_ids: Vec<String>,
    prompt: Option<String>,
    #[serde(default)]
    agents_states: BTreeMap<String, WireAgentState>,
    status: String,
}

#[derive(Deserialize)]
struct WireMessage {
    message: String,
}

#[derive(Deserialize)]
struct WireMcpResult {
    #[serde(default)]
    content: Vec<Value>,
    structured_content: Option<Value>,
}

impl WireMcpResult {
    fn into_text(self) -> Option<String> {
        let texts: Vec<&str> = self
            .content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect();
        if !texts.is_empty() {
            return Some(texts.join("\n"));
        }
        self.structured_content.map(|value| value.to_string())
    }
}

#[derive(Deserialize)]
struct WireAgentState {
    status: String,
    message: Option<String>,
}

#[derive(Deserialize)]
struct WireTodo {
    text: String,
    #[serde(default)]
    completed: bool,
}

impl WireTodo {
    /// Codex only says whether an item is done, so open items are all `Pending`.
    fn into_item(self) -> TodoItem {
        TodoItem {
            text: self.text,
            status: if self.completed {
                TodoStatus::Completed
            } else {
                TodoStatus::Pending
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::{Report, ReportStatus, RetryReason};

    fn parse(lines: &[&str]) -> (CodexParser, Vec<AgentEvent>) {
        let mut parser = CodexParser::new();
        let events = lines
            .iter()
            .flat_map(|line| parser.parse_stdout_line(line))
            .collect();
        (parser, events)
    }

    fn types(events: &[AgentEvent]) -> Vec<&'static str> {
        events.iter().map(AgentEvent::type_name).collect()
    }

    const THREAD: &str =
        r#"{"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}"#;
    const TURN: &str = r#"{"type":"turn.started"}"#;
    const DONE: &str = r#"{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"cache_write_input_tokens":0,"output_tokens":122,"reasoning_output_tokens":64}}"#;

    #[test]
    fn documented_sample_stream_maps_to_events() {
        let (_, events) = parse(&[
            THREAD,
            TURN,
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"docs\nsdk\n","exit_code":0,"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Repo contains docs and sdk."}}"#,
            DONE,
        ]);
        assert_eq!(
            types(&events),
            [
                "init",
                "tool_started",
                "tool_finished",
                "message",
                "usage",
                "result"
            ]
        );
        assert_eq!(
            events[0],
            AgentEvent::Init {
                session_id: Some("0199a213-81c0-7800-8aa1-bbab2a035a53".into()),
                model: None,
                tools: vec![],
            }
        );
        assert_eq!(
            events[1],
            AgentEvent::ToolStarted {
                id: "item_1".into(),
                name: "command_execution".into(),
                input: json!({"command": "bash -lc ls"}),
                parent: None,
            }
        );
        assert_eq!(
            events[2],
            AgentEvent::ToolFinished {
                id: "item_1".into(),
                ok: true,
                output: Some("docs\nsdk\n".into()),
                parent: None,
            }
        );
        assert_eq!(
            events[4],
            AgentEvent::Usage {
                input_tokens: 24763,
                output_tokens: 122,
                cached_tokens: 24448,
                cost_usd: None,
                cumulative: true,
            }
        );
        assert_eq!(
            events[5],
            AgentEvent::Result {
                text: Some("Repo contains docs and sdk.".into()),
                structured: None,
                is_error: false,
            }
        );
    }

    #[test]
    fn failed_or_declined_commands_are_not_ok() {
        let (_, events) = parse(&[
            r#"{"type":"item.completed","item":{"id":"item_2","type":"command_execution","command":"false","aggregated_output":"","exit_code":1,"status":"failed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_4","type":"command_execution","command":"rm -rf /","aggregated_output":"","exit_code":null,"status":"declined"}}"#,
        ]);
        // A completion without a start still gets a ToolStarted, so the UI can pair them.
        assert_eq!(
            types(&events),
            [
                "tool_started",
                "tool_finished",
                "tool_started",
                "tool_finished"
            ]
        );
        for event in [&events[1], &events[3]] {
            assert!(matches!(
                event,
                AgentEvent::ToolFinished {
                    ok: false,
                    output: None,
                    ..
                }
            ));
        }
    }

    #[test]
    fn file_changes_become_tool_calls_and_file_events() {
        let (_, events) = parse(&[
            r#"{"type":"item.started","item":{"id":"item_5","type":"file_change","changes":[{"path":"/w/hello.txt","kind":"add"}],"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_5","type":"file_change","changes":[{"path":"/w/hello.txt","kind":"add"},{"path":"/w/old.txt","kind":"delete"}],"status":"completed"}}"#,
        ]);
        assert_eq!(
            types(&events),
            [
                "tool_started",
                "tool_finished",
                "file_changed",
                "file_changed"
            ]
        );
        assert_eq!(
            events[2],
            AgentEvent::FileChanged {
                path: "/w/hello.txt".into(),
                kind: "add".into()
            }
        );
        assert_eq!(
            events[3],
            AgentEvent::FileChanged {
                path: "/w/old.txt".into(),
                kind: "delete".into()
            }
        );
    }

    #[test]
    fn failed_patches_report_no_file_changes() {
        let (_, events) = parse(&[
            r#"{"type":"item.completed","item":{"id":"item_6","type":"file_change","changes":[{"path":"/w/a","kind":"update"}],"status":"failed"}}"#,
        ]);
        assert_eq!(types(&events), ["tool_started", "tool_finished"]);
        assert!(matches!(
            events[1],
            AgentEvent::ToolFinished { ok: false, .. }
        ));
    }

    #[test]
    fn structured_final_message_becomes_the_report() {
        let report = r#"{\"status\":\"done\",\"summary\":\"Created hello.txt\",\"files_changed\":[\"hello.txt\"],\"tests_run\":[],\"concerns\":[]}"#;
        let message = format!(
            r#"{{"type":"item.completed","item":{{"id":"item_7","type":"agent_message","text":"{report}"}}}}"#
        );
        let (_, events) = parse(&[THREAD, &message, DONE]);
        let AgentEvent::Result {
            structured: Some(structured),
            is_error: false,
            ..
        } = events.last().unwrap()
        else {
            panic!("no structured result in {events:?}");
        };
        let report = Report::from_value(structured).unwrap();
        assert_eq!(report.status, ReportStatus::Done);
        assert_eq!(report.files_changed, ["hello.txt"]);
    }

    #[test]
    fn reasoning_todo_and_warnings() {
        let (_, events) = parse(&[
            r#"{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"**Planning** the change"}}"#,
            r#"{"type":"item.started","item":{"id":"item_1","type":"todo_list","items":[{"text":"write","completed":false},{"text":"test","completed":false}]}}"#,
            r#"{"type":"item.updated","item":{"id":"item_1","type":"todo_list","items":[{"text":"write","completed":true},{"text":"test","completed":false}]}}"#,
            r#"{"type":"item.completed","item":{"id":"item_9","type":"error","message":"model rerouted: gpt-5.6-luna -> gpt-6-luna (Capacity)"}}"#,
        ]);
        assert_eq!(types(&events), ["reasoning", "todo", "todo", "error"]);
        assert_eq!(
            events[2],
            AgentEvent::Todo {
                items: vec![
                    TodoItem {
                        text: "write".into(),
                        status: TodoStatus::Completed
                    },
                    TodoItem {
                        text: "test".into(),
                        status: TodoStatus::Pending
                    },
                ]
            }
        );
        assert!(matches!(
            events[3],
            AgentEvent::Error {
                category: ErrorCategory::ModelUnavailable,
                ..
            }
        ));
    }

    #[test]
    fn mcp_and_web_search_calls() {
        let (_, events) = parse(&[
            r#"{"type":"item.started","item":{"id":"item_1","type":"mcp_tool_call","server":"docs","tool":"search","arguments":{"q":"x"},"result":null,"error":null,"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"mcp_tool_call","server":"docs","tool":"search","arguments":{"q":"x"},"result":{"content":[{"type":"text","text":"found"}],"structured_content":null},"error":null,"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call","server":"docs","tool":"fetch","arguments":{},"result":null,"error":{"message":"boom"},"status":"failed"}}"#,
            r#"{"type":"item.started","item":{"id":"ws_1","type":"web_search","query":"rust","action":{"type":"search","query":"rust"}}}"#,
            r#"{"type":"item.completed","item":{"id":"ws_1","type":"web_search","query":"rust","action":{"type":"search","query":"rust"}}}"#,
        ]);
        assert_eq!(
            types(&events),
            [
                "tool_started",
                "tool_finished",
                "tool_started",
                "tool_finished",
                "tool_started",
                "tool_finished"
            ]
        );
        assert!(
            matches!(&events[0], AgentEvent::ToolStarted { name, input, .. } if name == "mcp:docs/search" && input == &json!({"q": "x"}))
        );
        assert!(
            matches!(&events[1], AgentEvent::ToolFinished { ok: true, output: Some(o), .. } if o == "found")
        );
        assert!(
            matches!(&events[3], AgentEvent::ToolFinished { ok: false, output: Some(o), .. } if o == "boom")
        );
        assert!(
            matches!(&events[4], AgentEvent::ToolStarted { name, id, .. } if name == "web_search" && id == "ws_1")
        );
    }

    #[test]
    fn subagents_are_keyed_by_child_thread() {
        let (_, events) = parse(&[
            r#"{"type":"item.started","item":{"id":"item_1","type":"collab_tool_call","tool":"spawn_agent","sender_thread_id":"main","receiver_thread_ids":["child-1"],"prompt":"look around","agents_states":{},"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"collab_tool_call","tool":"spawn_agent","sender_thread_id":"main","receiver_thread_ids":["child-1"],"prompt":"look around","agents_states":{"child-1":{"status":"running","message":null}},"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_2","type":"collab_tool_call","tool":"wait","sender_thread_id":"main","receiver_thread_ids":["child-1"],"prompt":null,"agents_states":{"child-1":{"status":"completed","message":"found 3 files"}},"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_3","type":"collab_tool_call","tool":"close_agent","sender_thread_id":"main","receiver_thread_ids":["child-1"],"prompt":null,"agents_states":{"child-1":{"status":"shutdown","message":"found 3 files"}},"status":"completed"}}"#,
        ]);
        assert_eq!(
            types(&events),
            [
                "tool_started",
                "tool_finished",
                "tool_started",
                "tool_finished",
                "message",
                "tool_started",
                "tool_finished"
            ]
        );
        assert!(
            matches!(&events[0], AgentEvent::ToolStarted { id, name, .. } if id == "child-1" && name == "spawn_agent")
        );
        assert!(
            matches!(&events[1], AgentEvent::ToolFinished { id, ok: true, .. } if id == "child-1")
        );
        assert!(
            matches!(&events[2], AgentEvent::ToolStarted { id, name, .. } if id == "item_2" && name == "wait")
        );
        assert_eq!(
            events[4],
            AgentEvent::Message {
                text: "found 3 files".into(),
                parent: Some("child-1".into())
            }
        );
    }

    #[test]
    fn retry_notices_become_retry_events() {
        let (parser, events) = parse(&[
            r#"{"type":"error","message":"Reconnecting... 1/5 (stream disconnected before completion: eof)"}"#,
        ]);
        assert_eq!(
            events,
            [AgentEvent::Retry {
                reason: RetryReason::Network,
                delay_ms: None
            }]
        );
        assert!(parser.last_error.is_none());
    }

    #[test]
    fn fatal_errors_are_reported_once_then_fail_the_turn() {
        let limit = "You’ve hit your usage limit. Try again at 3:00 PM.";
        let error = format!(r#"{{"type":"error","message":"{limit}"}}"#);
        let failed = format!(r#"{{"type":"turn.failed","error":{{"message":"{limit}"}}}}"#);
        let (mut parser, mut events) = parse(&[THREAD, TURN, &error, &failed]);
        events.extend(parser.finish(Some(1)));
        assert_eq!(types(&events), ["init", "error", "result"]);
        assert_eq!(
            events[1],
            AgentEvent::Error {
                category: ErrorCategory::UsageLimit,
                message: limit.into(),
                retryable: false,
            }
        );
        assert_eq!(
            events[2],
            AgentEvent::Result {
                text: None,
                structured: None,
                is_error: true
            }
        );
    }

    #[test]
    fn turn_failed_without_prior_error_reports_it() {
        let (_, events) =
            parse(&[r#"{"type":"turn.failed","error":{"message":"request timed out"}}"#]);
        assert_eq!(types(&events), ["error", "result"]);
        assert!(matches!(
            events[0],
            AgentEvent::Error {
                category: ErrorCategory::Timeout,
                retryable: true,
                ..
            }
        ));
    }

    #[test]
    fn malformed_and_unknown_lines_are_kept() {
        let (_, events) = parse(&[
            "",
            "   ",
            "not json {",
            r#"{"type":"turn.something_new","x":1}"#,
            r#"{"no_type":true}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"hologram","x":1}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"no id"}}"#,
            r#"{"type":"thread.started"}"#,
            r#"[1,2]"#,
        ]);
        assert_eq!(types(&events), ["unknown"; 7]);
        assert_eq!(
            events[0],
            AgentEvent::Unknown {
                raw: json!("not json {")
            }
        );
        assert_eq!(
            events[1],
            AgentEvent::Unknown {
                raw: json!({"type": "turn.something_new", "x": 1})
            }
        );
    }

    #[test]
    fn stderr_lines_are_kept_and_explain_a_failed_exit() {
        let mut parser = CodexParser::new();
        assert!(parser.parse_stderr_line("").is_empty());
        let events = parser.parse_stderr_line(
            "Not inside a trusted directory and --skip-git-repo-check was not specified.",
        );
        assert!(matches!(&events[..], [AgentEvent::Unknown { raw }] if raw["stderr"].is_string()));
        let events = parser.finish(Some(1));
        assert_eq!(types(&events), ["error", "result"]);
        assert!(matches!(
            &events[0],
            AgentEvent::Error { category: ErrorCategory::Cli, message, .. } if message.contains("trusted directory")
        ));
        assert!(matches!(
            events[1],
            AgentEvent::Result { is_error: true, .. }
        ));
    }

    #[test]
    fn stderr_tail_is_bounded() {
        let mut parser = CodexParser::new();
        for n in 0..25 {
            parser.parse_stderr_line(&format!("line {n}"));
        }
        assert_eq!(parser.stderr_tail.len(), STDERR_TAIL_LINES);
        assert_eq!(
            parser.stderr_tail.front().map(String::as_str),
            Some("line 15")
        );
    }

    #[test]
    fn interrupted_session_closes_open_tools_and_fails() {
        let (mut parser, _) = parse(&[
            THREAD,
            TURN,
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"sleep 100","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
        ]);
        let events = parser.finish(Some(1));
        assert_eq!(types(&events), ["tool_finished", "error", "result"]);
        assert!(
            matches!(&events[0], AgentEvent::ToolFinished { id, ok: false, .. } if id == "item_1")
        );
        assert!(matches!(
            &events[1],
            AgentEvent::Error { category: ErrorCategory::Cli, message, .. } if message.contains("code 1")
        ));
        assert!(matches!(
            events[2],
            AgentEvent::Result { is_error: true, .. }
        ));
    }

    #[test]
    fn killed_session_is_interrupted() {
        let mut parser = CodexParser::new();
        let events = parser.finish(None);
        assert!(matches!(
            events[0],
            AgentEvent::Error {
                category: ErrorCategory::Interrupted,
                ..
            }
        ));
        assert!(parser.finish(None).is_empty());
    }

    #[test]
    fn finish_after_a_completed_turn_adds_nothing() {
        let (mut parser, _) = parse(&[THREAD, TURN, DONE]);
        assert!(parser.finish(Some(0)).is_empty());
    }

    #[test]
    fn clean_exit_without_turn_end_still_gives_a_result() {
        let (mut parser, _) = parse(&[
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"ok"}}"#,
        ]);
        assert_eq!(
            parser.finish(Some(0)),
            [AgentEvent::Result {
                text: Some("ok".into()),
                structured: None,
                is_error: false
            }]
        );
    }

    #[test]
    fn usage_without_fields_defaults_to_zero() {
        let (_, events) = parse(&[r#"{"type":"turn.completed","usage":{}}"#]);
        assert!(matches!(
            events[0],
            AgentEvent::Usage {
                input_tokens: 0,
                cumulative: true,
                ..
            }
        ));
        let (_, events) = parse(&[r#"{"type":"turn.completed"}"#]);
        assert_eq!(types(&events), ["result"]);
    }
}
