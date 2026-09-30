//! A failing run can report one error up to three times. The parser keeps the most detailed and
//! reports it from [`StreamParser::finish`], once the exit code says whether agy recovered.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use argonauts_core::{AgentEvent, ErrorCategory, RetryReason, StreamParser};
use serde::Deserialize;
use serde_json::Value;

use crate::errors::{
    AgyError, agy_error_payload, is_soft_denial, parse_agy_error, strip_error_marker,
};

#[derive(Debug, Default)]
pub struct AgyStreamParser {
    /// The top-level conversation. Steps from other conversations belong to subagents.
    root: Option<String>,
    cwd: Option<String>,
    step_text: HashMap<StepKey, String>,
    /// The usage last reported for each step, so repeated reports only add the difference.
    step_usage: HashMap<StepKey, Usage>,
    open_tools: BTreeMap<StepKey, OpenTool>,
    closed_tools: HashSet<StepKey>,
    /// Subagent conversation id to the id of the tool call that started it.
    subagent_parent: HashMap<String, String>,
    /// The last complete top-level message, used when the run ends without a result.
    last_message: Option<String>,
    /// A non-success result, held until `finish` so its error can come first.
    held_result: Option<HeldResult>,
    terminal_error: Option<TerminalError>,
    saw_result: bool,
    reported_errors: HashSet<String>,
}

impl AgyStreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn on_init(&mut self, conversation_id: Option<String>, init: InitPayload) -> Vec<AgentEvent> {
        if conversation_id.is_some() {
            self.root.clone_from(&conversation_id);
        }
        self.cwd = init.cwd;
        let tools = init
            .tools
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        vec![AgentEvent::Init {
            session_id: conversation_id,
            model: init.model,
            tools,
        }]
    }

    fn on_step(&mut self, step: &StepUpdate, line: &str) -> Vec<AgentEvent> {
        let key = self.step_key(step);
        let is_tool = self.is_tool_step(&key, step);
        let mut events = Vec::new();
        match step.step_type.as_deref() {
            _ if is_tool => self.on_tool_step(&key, step, &mut events),
            Some("agent_response") => self.on_response_step(&key, step, &mut events),
            // An `error_message` step has no text. The error arrives in the result or on stderr.
            Some("user_input" | "checkpoint" | "error_message" | "finish") => {}
            _ => events.push(AgentEvent::unknown_line(line)),
        }
        if let Some(usage) = step.usage {
            events.extend(self.step_usage_increment(&key, usage));
        }
        events
    }

    /// Keys the step by conversation and index, adopting its conversation as root if none is set.
    fn step_key(&mut self, step: &StepUpdate) -> StepKey {
        let conversation = match (&step.conversation_id, &self.root) {
            (Some(id), _) => id.clone(),
            (None, Some(root)) => root.clone(),
            (None, None) => String::new(),
        };
        if self.root.is_none() && !conversation.is_empty() {
            self.root = Some(conversation.clone());
        }
        StepKey {
            conversation,
            index: step.step_index.unwrap_or_default(),
        }
    }

    /// A tool's closing update can carry its own step type, such as `finish`, and no tool info.
    fn is_tool_step(&self, key: &StepKey, step: &StepUpdate) -> bool {
        self.open_tools.contains_key(key)
            || step.step_type.as_deref() == Some("tool")
            || step.tool_info.is_some()
            || step.subagent_info.is_some()
    }

    fn on_response_step(&mut self, key: &StepKey, step: &StepUpdate, events: &mut Vec<AgentEvent>) {
        let parent = self.parent_of(&key.conversation);
        if let Some(delta) = step.text_delta.as_deref().filter(|t| !t.is_empty()) {
            self.step_text
                .entry(key.clone())
                .or_default()
                .push_str(delta);
            events.push(AgentEvent::TextDelta {
                text: delta.to_owned(),
                parent: parent.clone(),
            });
        }
        if step.is_finished() {
            let text = self.step_text.remove(key).unwrap_or_default();
            if !text.trim().is_empty() {
                if parent.is_none() {
                    self.last_message = Some(text.clone());
                }
                events.push(AgentEvent::Message { text, parent });
            }
        }
    }

    fn on_tool_step(&mut self, key: &StepKey, step: &StepUpdate, events: &mut Vec<AgentEvent>) {
        if self.closed_tools.contains(key) {
            return;
        }
        let info = step.tool_info.as_ref().unwrap_or(&NO_TOOL_INFO);
        let parent = self.parent_of(&key.conversation);
        if !self.open_tools.contains_key(key) {
            events.push(self.start_tool(key, step, info, parent.clone()));
        }
        if let (Some(subagents), Some(tool)) = (&step.subagent_info, self.open_tools.get(key)) {
            for child in subagent_conversations(subagents) {
                self.subagent_parent.insert(child, tool.id.clone());
            }
        }
        if step.is_finished() {
            self.finish_tool(key, step, info, parent, events);
        }
    }

    fn start_tool(
        &mut self,
        key: &StepKey,
        step: &StepUpdate,
        info: &ToolInfo,
        parent: Option<String>,
    ) -> AgentEvent {
        let tool = OpenTool {
            id: self.tool_id(key),
            name: step
                .tool_name
                .clone()
                .or_else(|| info.name.clone())
                .unwrap_or_else(|| "subagent".to_owned()),
            input: tool_input(info, step.subagent_info.as_ref()),
            parent,
        };
        let started = AgentEvent::ToolStarted {
            id: tool.id.clone(),
            name: tool.name.clone(),
            input: tool.input.clone(),
            parent: tool.parent.clone(),
        };
        self.open_tools.insert(key.clone(), tool);
        started
    }

    fn finish_tool(
        &mut self,
        key: &StepKey,
        step: &StepUpdate,
        info: &ToolInfo,
        parent: Option<String>,
        events: &mut Vec<AgentEvent>,
    ) {
        let Some(tool) = self.open_tools.remove(key) else {
            return;
        };
        self.closed_tools.insert(key.clone());
        let error = info.error_text();
        let ok = error.is_none() && step.state.as_deref() == Some("DONE");
        events.push(AgentEvent::ToolFinished {
            id: tool.id,
            ok,
            output: info.output_text().or(error),
            parent,
        });
        if ok {
            let input = if info.parameters.is_null() {
                &tool.input
            } else {
                &info.parameters
            };
            events.extend(file_change(&tool.name, input, self.cwd.as_deref()));
        }
    }

    fn on_result(&mut self, result: RunResult) -> Vec<AgentEvent> {
        self.saw_result = true;
        let mut events = Vec::new();
        // Step usage is per step, but the result's usage is the session total.
        if let Some(usage) = result.usage {
            events.push(usage.to_event(true));
        }
        if !result.denied_actions.is_empty() {
            let denied: Vec<String> = result.denied_actions.iter().map(describe_denied).collect();
            let message = format!("agy denied {}", denied.join(", "));
            events.extend(self.report_once(AgyError::permission(message)));
        }

        let error_text = result.error_text();
        let status = match (result.status.as_deref(), &error_text) {
            (Some(status), _) => status,
            (None, Some(_)) => "ERROR",
            (None, None) => "SUCCESS",
        };
        let outcome = HeldResult {
            text: result
                .response
                .filter(|r| !r.trim().is_empty())
                .or_else(|| self.last_message.clone()),
            structured: result.structured_output.filter(|v| !v.is_null()),
            recoverable: status == "ERROR",
        };
        if status == "SUCCESS" {
            events.push(outcome.into_event(false));
            return events;
        }

        let message = error_text.unwrap_or_else(|| format!("agy ended with status {status}"));
        self.note_terminal_error(ErrorSource::Result, AgyError::from_status(status, message));
        self.held_result = Some(outcome);
        events
    }

    fn note_terminal_error(&mut self, source: ErrorSource, error: AgyError) {
        let better = self
            .terminal_error
            .as_ref()
            .is_none_or(|current| source > current.source);
        if better {
            self.terminal_error = Some(TerminalError { source, error });
        }
    }

    fn report_once(&mut self, error: AgyError) -> Option<AgentEvent> {
        self.reported_errors
            .insert(error.message.trim().to_owned())
            .then_some(AgentEvent::Error {
                category: error.category,
                message: error.message,
                retryable: error.retryable,
            })
    }

    fn step_usage_increment(&mut self, key: &StepKey, usage: Usage) -> Option<AgentEvent> {
        let previous = self
            .step_usage
            .insert(key.clone(), usage)
            .unwrap_or_default();
        let increment = usage.minus(&previous);
        (!increment.is_zero()).then(|| increment.to_event(false))
    }

    fn abandon_open_tools(&mut self) -> Vec<AgentEvent> {
        std::mem::take(&mut self.open_tools)
            .into_values()
            .map(|tool| AgentEvent::ToolFinished {
                id: tool.id,
                ok: false,
                output: None,
                parent: tool.parent,
            })
            .collect()
    }

    fn final_result(&mut self, recovered: bool) -> Option<AgentEvent> {
        let result = match self.held_result.take() {
            Some(held) => Some(held.into_event(!recovered)),
            None if !self.saw_result => Some(AgentEvent::Result {
                text: self.last_message.clone(),
                structured: None,
                is_error: true,
            }),
            None => None,
        };
        self.saw_result = true;
        result
    }

    fn tool_id(&self, key: &StepKey) -> String {
        if self.is_root(&key.conversation) {
            format!("step-{}", key.index)
        } else {
            format!("{}/step-{}", key.conversation, key.index)
        }
    }

    fn parent_of(&self, conversation: &str) -> Option<String> {
        if self.is_root(conversation) {
            return None;
        }
        Some(
            self.subagent_parent
                .get(conversation)
                .cloned()
                .unwrap_or_else(|| conversation.to_owned()),
        )
    }

    fn is_root(&self, conversation: &str) -> bool {
        conversation.is_empty() || self.root.as_deref().is_none_or(|root| root == conversation)
    }
}

impl StreamParser for AgyStreamParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        match serde_json::from_str::<StreamEvent>(trimmed) {
            Ok(StreamEvent::Init {
                conversation_id,
                init,
            }) => self.on_init(conversation_id, init),
            Ok(StreamEvent::StepUpdate { step_update }) => self.on_step(&step_update, trimmed),
            Ok(StreamEvent::Result { result }) => self.on_result(result),
            Err(_) => vec![AgentEvent::unknown_line(trimmed)],
        }
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        if let Some(payload) = agy_error_payload(line) {
            self.note_terminal_error(ErrorSource::AgyErrorLine, parse_agy_error(payload));
            return Vec::new();
        }
        if let Some(message) = strip_error_marker(line) {
            let error = AgyError::from_message(message, ErrorCategory::Cli);
            self.note_terminal_error(ErrorSource::ErrorLine, error);
            return Vec::new();
        }
        if is_soft_denial(line) {
            let denial = AgyError::permission(line.to_owned());
            return self.report_once(denial).into_iter().collect();
        }
        vec![AgentEvent::unknown_line(line)]
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let mut events = self.abandon_open_tools();
        let clean_exit = exit_code == Some(0);
        let recovered = clean_exit && self.held_result.as_ref().is_some_and(|r| r.recoverable);
        match self.terminal_error.take() {
            Some(terminal) if recovered => events.push(AgentEvent::Retry {
                reason: retry_reason(&terminal.error),
                delay_ms: None,
            }),
            Some(terminal) => events.extend(self.report_once(terminal.error)),
            None if !clean_exit || !self.saw_result => {
                events.extend(self.report_once(exit_error(exit_code, self.saw_result)));
            }
            None => {}
        }
        events.extend(self.final_result(recovered));
        events
    }
}

fn exit_error(exit_code: Option<i32>, saw_result: bool) -> AgyError {
    let (category, message) = match exit_code {
        None => (
            ErrorCategory::Interrupted,
            "agy was stopped by a signal".to_owned(),
        ),
        Some(3) => (
            ErrorCategory::Unknown,
            "agy exited with code 3 (a model or agent API failure) without an AGY_ERROR line"
                .to_owned(),
        ),
        Some(0) => (ErrorCategory::Cli, "agy exited without a result".to_owned()),
        Some(code) if saw_result => (ErrorCategory::Cli, format!("agy exited with code {code}")),
        Some(code) => (
            ErrorCategory::Cli,
            format!("agy exited with code {code} before sending a result"),
        ),
    };
    AgyError {
        category,
        message,
        retryable: false,
    }
}

fn retry_reason(error: &AgyError) -> RetryReason {
    match error.category {
        ErrorCategory::RateLimit | ErrorCategory::UsageLimit => RetryReason::RateLimit,
        ErrorCategory::Overloaded => RetryReason::Overloaded,
        ErrorCategory::Server => RetryReason::ServerError,
        ErrorCategory::Network | ErrorCategory::Timeout => RetryReason::Network,
        _ => RetryReason::Other(error.message.clone()),
    }
}

fn file_change(tool: &str, input: &Value, cwd: Option<&str>) -> Option<AgentEvent> {
    let kind = match tool {
        "write_to_file" => {
            if input.get("Overwrite").and_then(Value::as_bool) == Some(true) {
                "update"
            } else {
                "add"
            }
        }
        "replace_file_content" | "multi_replace_file_content" | "sed_file" => "update",
        _ => return None,
    };
    let path = ["TargetFile", "File", "AbsolutePath"]
        .iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))?;
    Some(AgentEvent::FileChanged {
        path: relative_to(path, cwd),
        kind: kind.to_owned(),
    })
}

fn relative_to(path: &str, cwd: Option<&str>) -> String {
    let path = path.strip_prefix("file://").unwrap_or(path);
    cwd.and_then(|cwd| Path::new(path).strip_prefix(cwd).ok())
        .map(|rel| rel.to_string_lossy().into_owned())
        .filter(|rel| !rel.is_empty())
        .unwrap_or_else(|| path.to_owned())
}

fn tool_input(info: &ToolInfo, subagents: Option<&Value>) -> Value {
    if info.parameters.is_null() {
        subagents.cloned().unwrap_or(Value::Null)
    } else {
        info.parameters.clone()
    }
}

fn subagent_conversations(info: &Value) -> Vec<String> {
    info.get("subagents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.get("conversation_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn describe_denied(entry: &Value) -> String {
    let field = |key: &str| entry.get(key).and_then(Value::as_str);
    match (field("display_name"), field("action")) {
        (Some(name), Some(action)) => format!("{name} ({action})"),
        (Some(name), None) | (None, Some(name)) => name.to_owned(),
        (None, None) => value_text(entry),
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct StepKey {
    conversation: String,
    index: u64,
}

#[derive(Debug, Clone)]
struct OpenTool {
    id: String,
    name: String,
    input: Value,
    parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct HeldResult {
    text: Option<String>,
    structured: Option<Value>,
    /// True for status `ERROR`, which agy also reports after a retry saved the turn. Only the exit
    /// code tells them apart, since agy 1.2.10 exits with code 3 when an error ends the turn.
    recoverable: bool,
}

impl HeldResult {
    fn into_event(self, is_error: bool) -> AgentEvent {
        AgentEvent::Result {
            text: self.text,
            structured: self.structured,
            is_error,
        }
    }
}

/// Later variants carry more detail and win.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ErrorSource {
    Result,
    ErrorLine,
    AgyErrorLine,
}

#[derive(Debug, Clone)]
struct TerminalError {
    source: ErrorSource,
    error: AgyError,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum StreamEvent {
    Init {
        #[serde(default)]
        conversation_id: Option<String>,
        #[serde(default)]
        init: InitPayload,
    },
    StepUpdate {
        step_update: StepUpdate,
    },
    Result {
        result: RunResult,
    },
}

#[derive(Debug, Default, Deserialize)]
struct InitPayload {
    cwd: Option<String>,
    #[serde(default)]
    tools: Vec<Value>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StepUpdate {
    conversation_id: Option<String>,
    step_index: Option<u64>,
    state: Option<String>,
    step_type: Option<String>,
    tool_name: Option<String>,
    text_delta: Option<String>,
    usage: Option<Usage>,
    tool_info: Option<ToolInfo>,
    subagent_info: Option<Value>,
}

impl StepUpdate {
    /// Anything but `ACTIVE` counts, so `ERROR` closes a tool.
    fn is_finished(&self) -> bool {
        self.state.as_deref().is_some_and(|s| s != "ACTIVE")
    }
}

#[derive(Debug, Deserialize)]
struct ToolInfo {
    name: Option<String>,
    #[serde(default)]
    parameters: Value,
    #[serde(default)]
    output: Value,
    #[serde(default)]
    error: Value,
}

/// Stands in for a step without `tool_info`, so tool steps can borrow instead of clone.
static NO_TOOL_INFO: ToolInfo = ToolInfo {
    name: None,
    parameters: Value::Null,
    output: Value::Null,
    error: Value::Null,
};

impl ToolInfo {
    fn output_text(&self) -> Option<String> {
        (!self.output.is_null()).then(|| value_text(&self.output))
    }

    fn error_text(&self) -> Option<String> {
        match &self.error {
            Value::Null => None,
            Value::Object(fields) => Some(
                fields
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| self.error.to_string()),
            ),
            other => Some(value_text(other)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RunResult {
    status: Option<String>,
    response: Option<String>,
    error: Option<Value>,
    usage: Option<Usage>,
    structured_output: Option<Value>,
    #[serde(default)]
    denied_actions: Vec<Value>,
}

impl RunResult {
    fn error_text(&self) -> Option<String> {
        self.error
            .as_ref()
            .map(value_text)
            .filter(|e| !e.trim().is_empty())
    }
}

/// `input_tokens` leaves out cache reads, and `output_tokens` includes thinking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
}

impl Usage {
    fn minus(&self, earlier: &Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens.saturating_sub(earlier.input_tokens),
            output_tokens: self.output_tokens.saturating_sub(earlier.output_tokens),
            cache_read_tokens: self
                .cache_read_tokens
                .saturating_sub(earlier.cache_read_tokens),
        }
    }

    fn is_zero(&self) -> bool {
        *self == Usage::default()
    }

    fn to_event(self, cumulative: bool) -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: self.input_tokens.saturating_add(self.cache_read_tokens),
            output_tokens: self.output_tokens,
            cached_tokens: self.cache_read_tokens,
            cost_usd: None,
            cumulative,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_all(lines: &[&str]) -> Vec<AgentEvent> {
        let mut parser = AgyStreamParser::new();
        lines
            .iter()
            .flat_map(|line| parser.parse_stdout_line(line))
            .collect()
    }

    fn names(events: &[AgentEvent]) -> Vec<&'static str> {
        events.iter().map(AgentEvent::type_name).collect()
    }

    const INIT: &str = r#"{"event":"init","conversation_id":"c1","init":{"cwd":"/w","tools":["run_command","write_to_file"],"permission_mode":"request-review","model":"gemini-3.6-flash-low"}}"#;

    const RECOVERED_RESULT: &str = r#"{"event":"result","result":{"status":"ERROR","response":"done\n","error":"API error (attempt 1): UNAVAILABLE (code 503): No capacity available","structured_output":{"status":"done"}}}"#;

    #[test]
    fn init_reports_session_model_and_tools() {
        assert_eq!(
            parse_all(&[INIT]),
            [AgentEvent::Init {
                session_id: Some("c1".into()),
                model: Some("gemini-3.6-flash-low".into()),
                tools: vec!["run_command".into(), "write_to_file".into()],
            }]
        );
    }

    #[test]
    fn documented_short_run_parses_in_order() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":0,"state":"DONE","step_type":"user_input"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":3,"state":"DONE","step_type":"agent_response","text_delta":"Rebase.\n","duration_seconds":6.28,"usage":{"input_tokens":10302,"output_tokens":582,"thinking_tokens":551,"cache_read_tokens":8113,"total_tokens":10884}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":4,"state":"DONE","step_type":"checkpoint","usage":{"input_tokens":116,"output_tokens":7,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":123}}}"#,
            r#"{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"Rebase.\n","num_turns":1,"usage":{"input_tokens":10418,"output_tokens":589,"thinking_tokens":551,"cache_read_tokens":8113,"total_tokens":11007}}}"#,
        ]);
        assert_eq!(
            names(&events),
            [
                "init",
                "text_delta",
                "message",
                "usage",
                "usage",
                "usage",
                "result"
            ]
        );
        assert_eq!(
            events[3],
            AgentEvent::Usage {
                input_tokens: 18_415,
                output_tokens: 582,
                cached_tokens: 8113,
                cost_usd: None,
                cumulative: false,
            }
        );
        assert_eq!(
            events[5],
            AgentEvent::Usage {
                input_tokens: 18_531,
                output_tokens: 589,
                cached_tokens: 8113,
                cost_usd: None,
                cumulative: true,
            }
        );
        assert_eq!(
            events[6],
            AgentEvent::Result {
                text: Some("Rebase.\n".into()),
                structured: None,
                is_error: false,
            }
        );
    }

    #[test]
    fn active_deltas_stream_then_done_completes_the_message() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"app"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"le"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"\n"}}"#,
        ]);
        let texts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["app", "le", "\n"]);
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Message {
                text: "apple\n".into(),
                parent: None,
            })
        );
    }

    #[test]
    fn repeated_step_usage_only_adds_the_difference() {
        let events = parse_all(&[
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"ACTIVE","step_type":"agent_response","usage":{"input_tokens":100,"output_tokens":5}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":100,"output_tokens":9}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":100,"output_tokens":9}}}"#,
        ]);
        let outputs: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Usage { output_tokens, .. } => Some(*output_tokens),
                _ => None,
            })
            .collect();
        assert_eq!(outputs, [5, 4]);
    }

    #[test]
    fn a_done_only_tool_step_starts_and_finishes() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":4,"state":"DONE","step_type":"tool","tool_name":"run_command","duration_seconds":0.07,"tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"},"output":"hi\r\n"}}}"#,
        ]);
        assert_eq!(
            events[1..],
            [
                AgentEvent::ToolStarted {
                    id: "step-4".into(),
                    name: "run_command".into(),
                    input: json!({"CommandLine": "echo hi"}),
                    parent: None,
                },
                AgentEvent::ToolFinished {
                    id: "step-4".into(),
                    ok: true,
                    output: Some("hi\r\n".into()),
                    parent: None,
                },
            ]
        );
    }

    #[test]
    fn tool_errors_and_file_writes_are_reported() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":5,"state":"ACTIVE","step_type":"tool","tool_name":"write_to_file","tool_info":{"name":"write_to_file","parameters":{"TargetFile":"/w/src/a.txt"}}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":5,"state":"DONE","step_type":"tool","tool_name":"write_to_file","tool_info":{"name":"write_to_file","parameters":{"TargetFile":"/w/src/a.txt"}}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":6,"state":"ERROR","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"ls"},"error":{"type":"TOOL_ERROR","message":"permission check failed"}}}}"#,
        ]);
        assert_eq!(
            names(&events),
            [
                "init",
                "tool_started",
                "tool_finished",
                "file_changed",
                "tool_started",
                "tool_finished"
            ]
        );
        assert_eq!(
            events[3],
            AgentEvent::FileChanged {
                path: "src/a.txt".into(),
                kind: "add".into(),
            }
        );
        assert_eq!(
            events[5],
            AgentEvent::ToolFinished {
                id: "step-6".into(),
                ok: false,
                output: Some("permission check failed".into()),
                parent: None,
            }
        );
    }

    #[test]
    fn a_tool_closed_by_a_differently_typed_step_finishes() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":7,"state":"ACTIVE","step_type":"tool","tool_name":"finish","tool_info":{"name":"finish","parameters":{"status":"done"}}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":7,"state":"DONE","step_type":"finish","duration_seconds":0.02}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":5,"state":"DONE","step_type":"error_message","duration_seconds":0}}"#,
        ]);
        assert_eq!(names(&events), ["init", "tool_started", "tool_finished"]);
        assert_eq!(
            events[2],
            AgentEvent::ToolFinished {
                id: "step-7".into(),
                ok: true,
                output: None,
                parent: None,
            }
        );
    }

    #[test]
    fn subagent_steps_carry_their_parent_tool() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":7,"state":"ACTIVE","step_type":"tool","tool_name":"invoke_subagent","subagent_info":{"subagents":[{"type_name":"research","conversation_id":"c2"}]}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c2","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"found it"}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c2","step_index":2,"state":"DONE","step_type":"tool","tool_name":"view_file","tool_info":{"name":"view_file","parameters":{}}}}"#,
        ]);
        assert!(matches!(
            &events[1],
            AgentEvent::ToolStarted { id, name, parent: None, .. } if id == "step-7" && name == "invoke_subagent"
        ));
        assert_eq!(
            events[2],
            AgentEvent::TextDelta {
                text: "found it".into(),
                parent: Some("step-7".into()),
            }
        );
        assert!(matches!(
            &events[4],
            AgentEvent::ToolStarted { id, parent: Some(p), .. } if id == "c2/step-2" && p == "step-7"
        ));
    }

    #[test]
    fn structured_output_and_denials_come_from_the_result() {
        let events = parse_all(&[
            r#"{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"{\"status\":\"done\"}\n","structured_output":{"status":"done"},"denied_actions":[{"action":"command","display_name":"RunCommand"},"read_url"],"usage":{"input_tokens":1,"output_tokens":2}}}"#,
        ]);
        assert_eq!(names(&events), ["usage", "error", "result"]);
        assert_eq!(
            events[1],
            AgentEvent::Error {
                category: ErrorCategory::Permission,
                message: "agy denied RunCommand (command), read_url".into(),
                retryable: false,
            }
        );
        assert!(matches!(
            &events[2],
            AgentEvent::Result { structured: Some(s), is_error: false, .. } if s == &json!({"status": "done"})
        ));
    }

    #[test]
    fn error_results_wait_for_the_exit_code() {
        let mut parser = AgyStreamParser::new();
        let events = parser.parse_stdout_line(
            r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"invalid model selection (--model \"x\" --effort \"\"): model x is not recognized as a known model or custom model in settings","usage":{"input_tokens":0,"output_tokens":0}}}"#,
        );
        assert_eq!(names(&events), ["usage"]);
        let finish = parser.finish(Some(1));
        assert!(matches!(
            &finish[0],
            AgentEvent::Error {
                category: ErrorCategory::ModelUnavailable,
                retryable: false,
                ..
            }
        ));
        assert_eq!(
            finish[1],
            AgentEvent::Result {
                text: None,
                structured: None,
                is_error: true,
            }
        );
    }

    #[test]
    fn an_error_result_with_a_clean_exit_was_recovered() {
        let mut parser = AgyStreamParser::new();
        assert!(parser.parse_stdout_line(RECOVERED_RESULT).is_empty());
        assert_eq!(
            parser.finish(Some(0)),
            [
                AgentEvent::Retry {
                    reason: RetryReason::Overloaded,
                    delay_ms: None,
                },
                AgentEvent::Result {
                    text: Some("done\n".into()),
                    structured: Some(json!({"status": "done"})),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn other_failed_statuses_stay_errors_after_a_clean_exit() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(
            r#"{"event":"result","result":{"status":"RUNNING","response":"part"}}"#,
        );
        let finish = parser.finish(Some(0));
        assert!(matches!(
            &finish[0],
            AgentEvent::Error {
                category: ErrorCategory::Timeout,
                ..
            }
        ));
        assert!(matches!(
            &finish[1],
            AgentEvent::Result { is_error: true, .. }
        ));

        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(r#"{"event":"result","result":{"status":"INTERRUPTED"}}"#);
        let finish = parser.finish(Some(130));
        assert!(matches!(
            &finish[0],
            AgentEvent::Error {
                category: ErrorCategory::Interrupted,
                ..
            }
        ));
    }

    #[test]
    fn the_most_detailed_terminal_error_is_reported_once() {
        let mut parser = AgyStreamParser::new();
        let message = "API error (attempt 2): UNAVAILABLE (code 503): No capacity available";
        let result = format!(
            r#"{{"event":"result","result":{{"status":"ERROR","response":"","error":"{message}"}}}}"#
        );
        assert!(parser.parse_stdout_line(&result).is_empty());
        assert!(
            parser
                .parse_stderr_line(&format!("error: {message}"))
                .is_empty()
        );
        let agy_error = format!(
            r#"AGY_ERROR: {{"short_error":"{message}","retryable":false,"error_id":"e-2"}}"#
        );
        assert!(parser.parse_stderr_line(&agy_error).is_empty());
        assert_eq!(
            parser.finish(Some(3)),
            [
                AgentEvent::Error {
                    category: ErrorCategory::Overloaded,
                    message: format!("{message} (error id e-2)"),
                    retryable: false,
                },
                AgentEvent::Result {
                    text: None,
                    structured: None,
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn a_less_detailed_error_never_replaces_a_better_one() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stderr_line(r#"AGY_ERROR: {"short_error":"quota","error_code":429}"#);
        parser.parse_stderr_line("error: something vaguer");
        let finish = parser.finish(Some(3));
        assert!(matches!(
            &finish[0],
            AgentEvent::Error { message, category: ErrorCategory::RateLimit, .. } if message == "quota"
        ));
    }

    #[test]
    fn malformed_and_unknown_lines_are_kept() {
        let events = parse_all(&[
            "{not json",
            r#"{"event":"future_thing","x":1}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":9,"state":"DONE","step_type":"planner_note"}}"#,
            r#"{"event":"step_update","step_update":"oops"}"#,
            "",
        ]);
        assert_eq!(names(&events), ["unknown"; 4]);
        assert_eq!(
            events[0],
            AgentEvent::Unknown {
                raw: json!("{not json"),
            }
        );
        assert_eq!(
            events[1],
            AgentEvent::Unknown {
                raw: json!({"event": "future_thing", "x": 1}),
            }
        );
    }

    #[test]
    fn exit_3_without_agy_error_is_still_reported() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(INIT);
        parser.parse_stdout_line(
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"partial"}}"#,
        );
        let finish = parser.finish(Some(3));
        assert!(
            matches!(&finish[0], AgentEvent::Error { message, .. } if message.contains("code 3"))
        );
        assert_eq!(
            finish[1],
            AgentEvent::Result {
                text: Some("partial".into()),
                structured: None,
                is_error: true,
            }
        );
    }

    #[test]
    fn stderr_errors_denials_and_warnings() {
        let mut parser = AgyStreamParser::new();
        assert!(
            parser
                .parse_stderr_line("Error: authentication required. Run 'agy' to log in.")
                .is_empty()
        );
        let denied = parser.parse_stderr_line(
            "the run_command tool(s) required approval that headless mode cannot prompt for, so they were auto-denied.",
        );
        assert!(matches!(
            &denied[..],
            [AgentEvent::Error {
                category: ErrorCategory::Permission,
                ..
            }]
        ));
        assert!(parser.parse_stderr_line("  ").is_empty());
        assert_eq!(
            parser.parse_stderr_line(
                "warning: --mode plan has no effect while slash command expansion is disabled."
            ),
            [AgentEvent::Unknown {
                raw: json!(
                    "warning: --mode plan has no effect while slash command expansion is disabled."
                ),
            }]
        );
        let finish = parser.finish(Some(1));
        assert!(matches!(
            &finish[..],
            [
                AgentEvent::Error {
                    category: ErrorCategory::Auth,
                    retryable: false,
                    ..
                },
                AgentEvent::Result { is_error: true, .. },
            ]
        ));
    }

    #[test]
    fn repeated_denial_notices_are_reported_once() {
        let mut parser = AgyStreamParser::new();
        let notice = "the x tool(s) required approval that headless mode cannot prompt for, so they were auto-denied.";
        assert_eq!(parser.parse_stderr_line(notice).len(), 1);
        assert!(parser.parse_stderr_line(notice).is_empty());
    }

    #[test]
    fn finish_closes_open_tools_and_reports_a_signal() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(INIT);
        parser.parse_stdout_line(
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":3,"state":"ACTIVE","step_type":"tool","tool_name":"run_command","tool_info":{"parameters":{"CommandLine":"sleep 100"}}}}"#,
        );
        assert_eq!(
            parser.finish(None),
            [
                AgentEvent::ToolFinished {
                    id: "step-3".into(),
                    ok: false,
                    output: None,
                    parent: None,
                },
                AgentEvent::Error {
                    category: ErrorCategory::Interrupted,
                    message: "agy was stopped by a signal".into(),
                    retryable: false,
                },
                AgentEvent::Result {
                    text: None,
                    structured: None,
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn clean_finish_after_a_result_adds_nothing() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(
            r#"{"event":"result","result":{"status":"SUCCESS","response":"ok"}}"#,
        );
        assert!(parser.finish(Some(0)).is_empty());
    }

    #[test]
    fn paths_outside_cwd_stay_absolute() {
        assert_eq!(relative_to("/w/a/b.rs", Some("/w")), "a/b.rs");
        assert_eq!(relative_to("file:///w/a.rs", Some("/w")), "a.rs");
        assert_eq!(relative_to("/elsewhere/x", Some("/w")), "/elsewhere/x");
        assert_eq!(relative_to("rel.txt", None), "rel.txt");
    }

    #[test]
    fn overwrites_and_edits_are_updates() {
        let update = |tool: &str, input: Value| match file_change(tool, &input, Some("/w")) {
            Some(AgentEvent::FileChanged { path, kind }) => Some((path, kind)),
            _ => None,
        };
        assert_eq!(
            update(
                "write_to_file",
                json!({"TargetFile": "/w/a", "Overwrite": true})
            ),
            Some(("a".into(), "update".into()))
        );
        assert_eq!(
            update("replace_file_content", json!({"TargetFile": "/w/b"})),
            Some(("b".into(), "update".into()))
        );
        assert_eq!(update("view_file", json!({"TargetFile": "/w/c"})), None);
        assert_eq!(update("write_to_file", json!({})), None);
    }
}
