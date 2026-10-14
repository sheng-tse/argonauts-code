//! Each turn closes at its `result`, because a session with open input stays alive until every
//! turn has one. A failing turn can report its error three times, in the result and twice on
//! stderr. The parser reports the most detailed it has by then and drops the later echoes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use argonauts_core::events::session::SubagentStatus;
use argonauts_core::limits::{PlanUsageReading, PlanUsageSource, PlanUsageStatus};
use argonauts_core::names::AgentId;
use argonauts_core::{AgentEvent, ErrorCategory, RetryReason, StreamParser};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::ADAPTER_ID;
use crate::errors::{
    AgyError, agy_error_payload, is_soft_denial, parse_agy_error, strip_error_marker,
    without_error_id,
};
use crate::subagents::Subagents;

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
    /// The turn's last complete top-level message, used when its result has no response.
    last_message: Option<String>,
    /// Set by a result and cleared by the next turn's first line. agy 1.2.13 starts each turn
    /// with a `user_input` step and sends nothing after a result until the next turn.
    turn_closed: bool,
    /// The last closed turn ended in an error, which explains a failed exit.
    last_turn_failed: bool,
    /// The most detailed stderr error not yet reported.
    stderr_error: Option<StderrError>,
    saw_init: bool,
    /// Errors reported or retried since the turn began, keyed without agy's error id, so a
    /// stderr echo of the result's error is dropped.
    turn_errors: HashSet<String>,
    subagents: Subagents,
    /// The last context fill reported, so an unchanged one isn't repeated.
    last_fill: Option<u64>,
}

impl AgyStreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn on_init(&mut self, conversation_id: Option<String>, init: InitPayload) -> Vec<AgentEvent> {
        if conversation_id.is_some() {
            self.root.clone_from(&conversation_id);
        }
        self.saw_init = true;
        let tools = init.tool_names();
        self.cwd = init.cwd;
        vec![AgentEvent::Init {
            session_id: conversation_id,
            model: init.model,
            tools,
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        }]
    }

    fn on_step(&mut self, step: &StepUpdate, line: &str) -> Vec<AgentEvent> {
        let key = self.step_key(step);
        let is_tool = self.is_tool_step(&key, step);
        let mut events = Vec::new();
        match step.step_type.as_deref() {
            _ if is_tool => self.on_tool_step(&key, step, &mut events),
            Some("agent_response") => self.on_response_step(&key, step, &mut events),
            // These carry no text. An error arrives in the result or on stderr, and a
            // `system_message` (such as a subagent's reply reaching its parent) stays internal.
            Some("user_input" | "checkpoint" | "error_message" | "finish" | "system_message") => {}
            _ => events.push(AgentEvent::unknown_line(line)),
        }
        if let Some(info) = &step.subagent_info {
            let parent = (!self.is_root(&key.conversation)).then_some(key.conversation.as_str());
            let failed = step.state.as_deref() == Some("ERROR");
            events.extend(self.subagents.on_info(info, parent, failed));
        }
        if let Some(usage) = step.usage {
            events.extend(self.step_usage_increment(&key, usage));
            // A `checkpoint` step's usage is a separate small call, not the conversation.
            let main_call = step.step_type.as_deref() == Some("agent_response");
            if main_call && self.is_root(&key.conversation) {
                events.extend(self.context_fill(usage));
            }
        }
        events
    }

    /// The step's prompt plus the visible part of its answer, which the next request carries.
    fn context_fill(&mut self, usage: Usage) -> Option<AgentEvent> {
        let used = usage
            .input_tokens
            .saturating_add(usage.cache_read_tokens)
            .saturating_add(usage.output_tokens.saturating_sub(usage.thinking_tokens));
        if used == 0 || self.last_fill == Some(used) {
            return None;
        }
        self.last_fill = Some(used);
        Some(AgentEvent::ContextFill {
            used,
            size: None,
            sent: None,
        })
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
            || matches!(step.step_type.as_deref(), Some("tool" | "subagent"))
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
                let child = self.subagents.on_child_message(&key.conversation, &text);
                events.push(AgentEvent::Message { text, parent });
                events.extend(child);
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

    /// The first line after a result begins the next turn. A stderr error that came in between
    /// belongs to the turn before.
    fn open_turn(&mut self) -> Vec<AgentEvent> {
        if !std::mem::take(&mut self.turn_closed) {
            return Vec::new();
        }
        let late = self.take_stderr_error().map(|error| self.report(error));
        self.turn_errors.clear();
        self.last_turn_failed = false;
        late.unwrap_or_default()
    }

    fn on_result(&mut self, result: RunResult) -> Vec<AgentEvent> {
        self.turn_closed = true;
        let error_text = result.error_text();
        let status = match (result.status.as_deref(), &error_text) {
            (Some(status), _) => status,
            (None, Some(_)) => "ERROR",
            (None, None) => "SUCCESS",
        };
        // The turn's end is the first sign that its subagents ended.
        let mut events = self.subagents.close_running(if status == "SUCCESS" {
            SubagentStatus::Done
        } else {
            SubagentStatus::Failed
        });
        // Step usage is per step, but the result's usage is the session total.
        if let Some(usage) = result.usage {
            events.push(usage.to_event(true));
        }
        let denied = result.denied_actions();
        if !denied.is_empty() {
            let message = format!("agy denied {}", denied.join(", "));
            events.extend(self.report(AgyError::permission(message)));
        }

        let response = result.response.filter(|r| !r.trim().is_empty());
        let structured = result.structured_output.filter(|v| !v.is_null());
        // agy 1.2.13 also reports ERROR for a turn that a retry saved, and the finished work is
        // what tells them apart.
        let recovered = status == "ERROR" && (response.is_some() || structured.is_some());
        let failed = status != "SUCCESS" && !recovered;
        let last_message = self.last_message.take();
        let text = response.or(last_message);
        let stderr_error = self.take_stderr_error();
        if status == "SUCCESS" {
            events.extend(
                stderr_error
                    .map(|error| self.report(error))
                    .unwrap_or_default(),
            );
        } else {
            let message = error_text.unwrap_or_else(|| format!("agy ended with status {status}"));
            let from_result = AgyError::from_status(status, message);
            let result_key = error_key(&from_result);
            let error = stderr_error.unwrap_or(from_result);
            if recovered {
                self.turn_errors.insert(error_key(&error));
                events.push(AgentEvent::Retry {
                    reason: retry_reason(&error),
                    delay_ms: None,
                });
            } else {
                // agy retries a failed request itself, so an error that still ends the turn is
                // final. agy 1.2.13's `AGY_ERROR` line says the same.
                events.extend(self.report(AgyError {
                    retryable: false,
                    ..error
                }));
            }
            // A stderr line that repeats the result's own error is the same failure.
            self.turn_errors.insert(result_key);
        }
        self.last_turn_failed = failed;
        events.push(AgentEvent::Result {
            text,
            structured,
            is_error: failed,
        });
        events
    }

    /// Keeps the most detailed stderr error, unless it repeats one this turn already reported.
    fn note_stderr_error(&mut self, source: StderrSource, error: AgyError) {
        if self.turn_errors.contains(&error_key(&error)) {
            return;
        }
        let better = self
            .stderr_error
            .as_ref()
            .is_none_or(|current| source > current.source);
        if better {
            self.stderr_error = Some(StderrError { source, error });
        }
    }

    fn take_stderr_error(&mut self) -> Option<AgyError> {
        self.stderr_error.take().map(|held| held.error)
    }

    /// Reports an error once per turn, with a plan usage reading when the plan's usage is spent.
    fn report(&mut self, error: AgyError) -> Vec<AgentEvent> {
        if !self.turn_errors.insert(error_key(&error)) {
            return Vec::new();
        }
        let mut events: Vec<AgentEvent> = plan_usage_spent(&error).into_iter().collect();
        events.push(AgentEvent::Error {
            category: error.category,
            message: error.message,
            retryable: error.retryable,
        });
        events
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
        let Ok(event) = serde_json::from_str::<StreamEvent>(trimmed) else {
            return vec![AgentEvent::unknown_line(trimmed)];
        };
        match event {
            StreamEvent::Init {
                conversation_id,
                init,
            } => {
                let mut events = self.open_turn();
                events.extend(self.on_init(conversation_id, init));
                events
            }
            StreamEvent::StepUpdate { step_update } => {
                let mut events = self.open_turn();
                events.extend(self.on_step(&step_update, trimmed));
                events
            }
            StreamEvent::Result { result } => self.on_result(result),
        }
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        if let Some(payload) = agy_error_payload(line) {
            self.note_stderr_error(StderrSource::AgyErrorLine, parse_agy_error(payload));
            return Vec::new();
        }
        if let Some(message) = strip_error_marker(line) {
            let error = AgyError::from_message(message, ErrorCategory::Unknown);
            self.note_stderr_error(StderrSource::ErrorLine, error);
            return Vec::new();
        }
        if is_soft_denial(line) {
            return self.report(AgyError::permission(line.to_owned()));
        }
        vec![AgentEvent::unknown_line(line)]
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let mut events = self.abandon_open_tools();
        events.extend(self.subagents.close_running(SubagentStatus::Failed));
        let stderr_error = self
            .take_stderr_error()
            .map(|error| rejected_model(error, exit_code, self.saw_init));
        if self.turn_closed {
            // Every turn has its result, so what's left is an error agy printed after it or an
            // exit that the last turn's own error doesn't explain.
            let unexplained = exit_code != Some(0) && !self.last_turn_failed;
            let error = stderr_error.or_else(|| unexplained.then(|| exit_error(exit_code, true)));
            events.extend(error.map(|error| self.report(error)).unwrap_or_default());
            return events;
        }
        let error = stderr_error.unwrap_or_else(|| exit_error(exit_code, false));
        events.extend(self.report(error));
        events.push(AgentEvent::Result {
            text: self.last_message.take(),
            structured: None,
            is_error: true,
        });
        self.turn_closed = true;
        self.last_turn_failed = true;
        events
    }
}

fn error_key(error: &AgyError) -> String {
    without_error_id(&error.message).to_owned()
}

fn exit_error(exit_code: Option<i32>, turn_closed: bool) -> AgyError {
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
        Some(0) => (
            ErrorCategory::Unknown,
            "agy exited without a result".to_owned(),
        ),
        Some(code) if turn_closed => (
            ErrorCategory::Unknown,
            format!("agy exited with code {code}"),
        ),
        Some(code) => (
            ErrorCategory::Unknown,
            format!("agy exited with code {code} before sending a result"),
        ),
    };
    AgyError {
        category,
        message,
        retryable: false,
    }
}

/// agy exits 1 on an unknown `--model` before the session starts, instead of falling back.
fn rejected_model(error: AgyError, exit_code: Option<i32>, saw_init: bool) -> AgyError {
    let about_model = error.message.to_ascii_lowercase().contains("model");
    if exit_code == Some(1) && !saw_init && about_model {
        AgyError {
            category: ErrorCategory::ModelUnavailable,
            retryable: false,
            ..error
        }
    } else {
        error
    }
}

/// A daily or plan limit that ran out is also a plan usage reading, so the board stops sending
/// tasks here until a probe says otherwise.
fn plan_usage_spent(error: &AgyError) -> Option<AgentEvent> {
    if error.category != ErrorCategory::UsageLimit {
        return None;
    }
    let lower = error.message.to_ascii_lowercase();
    let window = (lower.contains("daily") || lower.contains("per day")).then(|| "daily".to_owned());
    Some(AgentEvent::PlanUsage {
        reading: PlanUsageReading {
            agent_id: AgentId(ADAPTER_ID.to_owned()),
            status: PlanUsageStatus::Rejected,
            used: Some(1.0),
            resets_at: None,
            window,
            source: PlanUsageSource::ErrorMessage,
            observed_at: OffsetDateTime::now_utc(),
            detail: Some(json!({"message": error.message})),
            wrap_up: false,
        },
    })
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

/// Later variants carry more detail and win. Both beat the result's own error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum StderrSource {
    ErrorLine,
    AgyErrorLine,
}

#[derive(Debug, Clone)]
struct StderrError {
    source: StderrSource,
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
        step_update: Box<StepUpdate>,
    },
    Result {
        result: RunResult,
    },
}

// The fields that say what a line means are strict, so a line whose meaning changed becomes
// Unknown. The minor ones are lenient, so a changed type drops only that field.

#[derive(Debug, Default, Deserialize)]
struct InitPayload {
    #[serde(default, deserialize_with = "lenient")]
    cwd: Option<String>,
    #[serde(default)]
    tools: Value,
    #[serde(default, deserialize_with = "lenient")]
    model: Option<String>,
}

impl InitPayload {
    fn tool_names(&self) -> Vec<String> {
        self.tools
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct StepUpdate {
    conversation_id: Option<String>,
    #[serde(default, deserialize_with = "lenient_count")]
    step_index: Option<u64>,
    state: Option<String>,
    step_type: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    tool_name: Option<String>,
    text_delta: Option<String>,
    #[serde(default, deserialize_with = "lenient_usage")]
    usage: Option<Usage>,
    #[serde(default, deserialize_with = "lenient")]
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
    #[serde(default, deserialize_with = "lenient")]
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
    #[serde(default, deserialize_with = "lenient_usage")]
    usage: Option<Usage>,
    structured_output: Option<Value>,
    #[serde(default)]
    denied_actions: Value,
}

impl RunResult {
    fn error_text(&self) -> Option<String> {
        self.error
            .as_ref()
            .map(value_text)
            .filter(|e| !e.trim().is_empty())
    }

    fn denied_actions(&self) -> Vec<String> {
        self.denied_actions
            .as_array()
            .into_iter()
            .flatten()
            .map(describe_denied)
            .collect()
    }
}

fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

fn lenient_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Value::deserialize(deserializer).map(|value| count(&value))
}

fn lenient_usage<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Usage>, D::Error> {
    Value::deserialize(deserializer).map(|value| Usage::from_value(&value))
}

/// A whole number, also when it arrives as a float such as `12.0`.
fn count(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|n| n.round() as u64)
    })
}

/// `input_tokens` leaves out cache reads, and `output_tokens` includes thinking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    thinking_tokens: u64,
}

impl Usage {
    /// Each count is read on its own, so one that isn't a number counts as zero.
    fn from_value(value: &Value) -> Option<Usage> {
        let fields = value.as_object()?;
        let field = |key: &str| fields.get(key).and_then(count).unwrap_or_default();
        Some(Usage {
            input_tokens: field("input_tokens"),
            output_tokens: field("output_tokens"),
            cache_read_tokens: field("cache_read_tokens"),
            thinking_tokens: field("thinking_tokens"),
        })
    }

    fn minus(&self, earlier: &Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens.saturating_sub(earlier.input_tokens),
            output_tokens: self.output_tokens.saturating_sub(earlier.output_tokens),
            cache_read_tokens: self
                .cache_read_tokens
                .saturating_sub(earlier.cache_read_tokens),
            thinking_tokens: self.thinking_tokens.saturating_sub(earlier.thinking_tokens),
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
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
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
                "context_fill",
                "usage",
                "usage",
                "result"
            ]
        );
        assert_eq!(
            events[4],
            AgentEvent::ContextFill {
                used: 10_302 + 8113 + 582 - 551,
                size: None,
                sent: None,
            }
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
            events[6],
            AgentEvent::Usage {
                input_tokens: 18_531,
                output_tokens: 589,
                cached_tokens: 8113,
                cost_usd: None,
                cumulative: true,
            }
        );
        assert_eq!(
            events[7],
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
        assert_eq!(
            names(&events),
            [
                "init",
                "tool_started",
                "subagent",
                "text_delta",
                "message",
                "subagent",
                "tool_started",
                "tool_finished"
            ]
        );
        assert!(matches!(
            &events[1],
            AgentEvent::ToolStarted { id, name, parent: None, .. } if id == "step-7" && name == "invoke_subagent"
        ));
        assert!(matches!(
            &events[2],
            AgentEvent::Subagent { update } if update.id == "c2" && update.parent.is_none()
        ));
        assert_eq!(
            events[3],
            AgentEvent::TextDelta {
                text: "found it".into(),
                parent: Some("step-7".into()),
            }
        );
        assert!(matches!(
            &events[5],
            AgentEvent::Subagent { update }
                if update.last_message.as_deref() == Some("found it") && update.steps_visible
        ));
        assert!(matches!(
            &events[6],
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
    fn an_error_result_reports_its_error_at_once() {
        let mut parser = AgyStreamParser::new();
        let events = parser.parse_stdout_line(
            r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"invalid model selection (--model \"x\" --effort \"\"): model x is not recognized as a known model or custom model in settings","usage":{"input_tokens":0,"output_tokens":0}}}"#,
        );
        assert_eq!(names(&events), ["usage", "error", "result"]);
        assert!(matches!(
            &events[1],
            AgentEvent::Error {
                category: ErrorCategory::ModelUnavailable,
                retryable: false,
                ..
            }
        ));
        assert_eq!(
            events[2],
            AgentEvent::Result {
                text: None,
                structured: None,
                is_error: true,
            }
        );
        // The turn's own error explains the failed exit.
        assert!(parser.finish(Some(1)).is_empty());
    }

    #[test]
    fn an_error_result_with_finished_work_was_recovered() {
        let mut parser = AgyStreamParser::new();
        assert_eq!(
            parser.parse_stdout_line(RECOVERED_RESULT),
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
        assert!(
            parser
                .parse_stderr_line(
                    "error: API error (attempt 1): UNAVAILABLE (code 503): No capacity available"
                )
                .is_empty()
        );
        assert!(parser.finish(Some(0)).is_empty());
    }

    #[test]
    fn other_failed_statuses_are_errors_even_with_a_response() {
        let mut parser = AgyStreamParser::new();
        let events = parser.parse_stdout_line(
            r#"{"event":"result","result":{"status":"RUNNING","response":"part"}}"#,
        );
        assert!(matches!(
            &events[..],
            [
                AgentEvent::Error {
                    category: ErrorCategory::Timeout,
                    retryable: false,
                    ..
                },
                AgentEvent::Result { is_error: true, .. },
            ]
        ));

        let mut parser = AgyStreamParser::new();
        let events =
            parser.parse_stdout_line(r#"{"event":"result","result":{"status":"INTERRUPTED"}}"#);
        assert!(matches!(
            &events[0],
            AgentEvent::Error {
                category: ErrorCategory::Interrupted,
                ..
            }
        ));
        assert!(parser.finish(Some(130)).is_empty());
    }

    const CAPACITY: &str = "API error (attempt 2): UNAVAILABLE (code 503): No capacity available";

    fn capacity_result() -> String {
        format!(
            r#"{{"event":"result","result":{{"status":"ERROR","response":"","error":"{CAPACITY}"}}}}"#
        )
    }

    fn capacity_agy_error() -> String {
        format!(r#"AGY_ERROR: {{"short_error":"{CAPACITY}","retryable":false,"error_id":"e-2"}}"#)
    }

    #[test]
    fn stderr_echoes_of_a_reported_error_are_dropped() {
        let mut parser = AgyStreamParser::new();
        assert_eq!(
            parser.parse_stdout_line(&capacity_result()),
            [
                AgentEvent::Error {
                    category: ErrorCategory::Overloaded,
                    message: CAPACITY.into(),
                    retryable: false,
                },
                AgentEvent::Result {
                    text: None,
                    structured: None,
                    is_error: true,
                },
            ]
        );
        assert!(
            parser
                .parse_stderr_line(&format!("error: {CAPACITY}"))
                .is_empty()
        );
        assert!(parser.parse_stderr_line(&capacity_agy_error()).is_empty());
        assert!(parser.finish(Some(3)).is_empty());
    }

    #[test]
    fn stderr_details_that_came_first_win_at_the_result() {
        let mut parser = AgyStreamParser::new();
        assert!(
            parser
                .parse_stderr_line(&format!("error: {CAPACITY}"))
                .is_empty()
        );
        assert!(parser.parse_stderr_line(&capacity_agy_error()).is_empty());
        let events = parser.parse_stdout_line(&capacity_result());
        assert_eq!(
            events[0],
            AgentEvent::Error {
                category: ErrorCategory::Overloaded,
                message: format!("{CAPACITY} (error id e-2)"),
                retryable: false,
            }
        );
        assert_eq!(names(&events), ["error", "result"]);
        assert!(parser.finish(Some(3)).is_empty());
    }

    #[test]
    fn a_spent_plan_usage_is_reported_with_its_turn() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(INIT);
        let events = parser.parse_stdout_line(
            r#"{"event":"result","result":{"status":"ERROR","response":"","error":"API error (attempt 1): RESOURCE_EXHAUSTED (code 429): Daily quota exhausted for gemini-3.6-flash-low"}}"#,
        );
        assert_eq!(names(&events), ["plan_usage", "error", "result"]);
        assert!(matches!(
            &events[1],
            AgentEvent::Error {
                category: ErrorCategory::UsageLimit,
                retryable: false,
                ..
            }
        ));
    }

    #[test]
    fn each_turn_reports_its_own_errors_and_result() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(INIT);
        let second_turn = r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":8,"state":"DONE","step_type":"user_input"}}"#;
        let mut errors = 0;
        for _ in 0..2 {
            parser.parse_stdout_line(second_turn);
            let events = parser.parse_stdout_line(&capacity_result());
            assert_eq!(names(&events), ["error", "result"]);
            errors += 1;
        }
        assert_eq!(errors, 2);
        parser.parse_stdout_line(second_turn);
        let done = parser.parse_stdout_line(
            r#"{"event":"result","result":{"status":"SUCCESS","response":"ok"}}"#,
        );
        assert_eq!(
            done,
            [AgentEvent::Result {
                text: Some("ok".into()),
                structured: None,
                is_error: false,
            }]
        );
        assert!(parser.finish(Some(0)).is_empty());
    }

    #[test]
    fn a_late_stderr_error_is_reported_before_the_next_turn() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(INIT);
        parser.parse_stdout_line(r#"{"event":"result","result":{"status":"SUCCESS"}}"#);
        assert!(parser.parse_stderr_line("error: disk full").is_empty());
        let events = parser.parse_stdout_line(
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":8,"state":"DONE","step_type":"user_input"}}"#,
        );
        assert!(matches!(
            &events[..],
            [AgentEvent::Error { message, .. }] if message == "disk full"
        ));
    }

    #[test]
    fn a_turn_cut_short_after_an_earlier_result_still_gets_one() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(INIT);
        parser.parse_stdout_line(r#"{"event":"result","result":{"status":"SUCCESS"}}"#);
        parser.parse_stdout_line(
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":9,"state":"DONE","step_type":"agent_response","text_delta":"half"}}"#,
        );
        let finish = parser.finish(Some(1));
        assert!(matches!(
            &finish[..],
            [
                AgentEvent::Error { message, .. },
                AgentEvent::Result { text: Some(text), is_error: true, .. },
            ] if message.contains("before sending a result") && text == "half"
        ));
    }

    #[test]
    fn minor_fields_of_another_type_drop_only_themselves() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":"three","state":"DONE","step_type":"agent_response","text_delta":"hi","usage":{"input_tokens":12.0,"output_tokens":"many"}}}"#,
            r#"{"event":"result","result":{"status":"SUCCESS","response":"hi","usage":[1],"denied_actions":{"x":1}}}"#,
        ]);
        assert_eq!(
            names(&events),
            [
                "init",
                "text_delta",
                "message",
                "usage",
                "context_fill",
                "result"
            ]
        );
        assert_eq!(
            events[3],
            AgentEvent::Usage {
                input_tokens: 12,
                output_tokens: 0,
                cached_tokens: 0,
                cost_usd: None,
                cumulative: false,
            }
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

    /// Replays stdout, then stderr, then the exit, the order the worker delivers them.
    fn run(stdout: &[&str], stderr: &[&str], exit_code: Option<i32>) -> Vec<AgentEvent> {
        let mut parser = AgyStreamParser::new();
        let mut events: Vec<AgentEvent> = stdout
            .iter()
            .flat_map(|line| parser.parse_stdout_line(line))
            .collect();
        for line in stderr {
            events.extend(parser.parse_stderr_line(line));
        }
        events.extend(parser.finish(exit_code));
        events
    }

    fn only_error(events: &[AgentEvent]) -> (ErrorCategory, bool) {
        let errors: Vec<(ErrorCategory, bool)> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Error {
                    category,
                    retryable,
                    ..
                } => Some((*category, *retryable)),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 1, "{events:?}");
        errors[0]
    }

    #[test]
    fn agy_error_classification() {
        let daily = run(
            &[INIT],
            &[
                r#"AGY_ERROR: {"short_error":"Daily quota exhausted for gemini-3.6-flash-low","canonical_status":"RESOURCE_EXHAUSTED","http_status":429,"retryable":false,"error_id":"e1"}"#,
            ],
            Some(3),
        );
        assert_eq!(only_error(&daily), (ErrorCategory::UsageLimit, false));
        let plan_usage = daily
            .iter()
            .find_map(|e| match e {
                AgentEvent::PlanUsage { reading } => Some(reading),
                _ => None,
            })
            .expect("a plan usage reading");
        assert_eq!(plan_usage.agent_id, AgentId("antigravity-cli".into()));
        assert_eq!(plan_usage.status, PlanUsageStatus::Rejected);
        assert_eq!(plan_usage.window.as_deref(), Some("daily"));
        assert_eq!(plan_usage.source, PlanUsageSource::ErrorMessage);
        assert!(matches!(
            daily.last(),
            Some(AgentEvent::Result { is_error: true, .. })
        ));

        let billing = run(
            &[INIT],
            &[
                r#"AGY_ERROR: {"short_error":"Prepaid credits are depleted","canonical_status":"RESOURCE_EXHAUSTED","http_status":429}"#,
            ],
            Some(3),
        );
        assert_eq!(only_error(&billing), (ErrorCategory::Billing, false));
        assert!(
            !billing
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanUsage { .. }))
        );

        let cases = [
            (
                r#"{"short_error":"Too many requests per minute","canonical_status":"RESOURCE_EXHAUSTED","http_status":429}"#,
                (ErrorCategory::RateLimit, true),
            ),
            (
                r#"{"short_error":"No capacity","canonical_status":"UNAVAILABLE","http_status":503}"#,
                (ErrorCategory::Overloaded, true),
            ),
            (
                r#"{"short_error":"Request had invalid authentication credentials","canonical_status":"UNAUTHENTICATED","http_status":401}"#,
                (ErrorCategory::Auth, false),
            ),
            (
                r#"{"short_error":"boom","status_code":500}"#,
                (ErrorCategory::Server, true),
            ),
        ];
        for (payload, want) in cases {
            let line = format!("AGY_ERROR: {payload}");
            let events = run(&[INIT], &[line.as_str()], Some(3));
            assert_eq!(only_error(&events), want, "{payload}");
        }

        // agy retried on its own and the turn still succeeded.
        let retried = run(
            &[
                INIT,
                r#"{"event":"result","result":{"status":"ERROR","response":"done","error":"API error (attempt 1): UNAVAILABLE (code 503): No capacity available"}}"#,
            ],
            &[],
            Some(0),
        );
        assert!(retried.contains(&AgentEvent::Retry {
            reason: RetryReason::Overloaded,
            delay_ms: None,
        }));
        assert!(
            !retried
                .iter()
                .any(|e| matches!(e, AgentEvent::Error { .. }))
        );

        let unknown_model = run(
            &[],
            &[r#"error: invalid value "gemini-no-such-model" for --model"#],
            Some(1),
        );
        assert_eq!(
            only_error(&unknown_model),
            (ErrorCategory::ModelUnavailable, false)
        );

        let bad_flag = run(&[], &["error: unknown flag: --frobnicate"], Some(1));
        assert_ne!(only_error(&bad_flag).0, ErrorCategory::ModelUnavailable);
        // Once the session started, exit 1 isn't a model rejection.
        let later = run(&[INIT], &["error: the model stream broke"], Some(1));
        assert_ne!(only_error(&later).0, ErrorCategory::ModelUnavailable);
    }

    #[test]
    fn context_fill_skips_repeats_side_calls_and_subagents() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":1,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":100,"output_tokens":20,"thinking_tokens":15,"cache_read_tokens":50}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":1,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":100,"output_tokens":20,"thinking_tokens":15,"cache_read_tokens":50}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"checkpoint","usage":{"input_tokens":9,"output_tokens":1}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c9","step_index":1,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":5000,"output_tokens":1}}}"#,
        ]);
        let fills: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ContextFill { .. }))
            .collect();
        assert_eq!(
            fills,
            [&AgentEvent::ContextFill {
                used: 100 + 50 + 5,
                size: None,
                sent: None,
            }]
        );
    }

    #[test]
    fn subagent_steps_and_system_messages_are_understood() {
        let events = parse_all(&[
            INIT,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"ACTIVE","step_type":"subagent","tool_name":"invoke_subagent","subagent_info":{"subagents":[{"type_name":"research","initial_prompt":"look"}]}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"subagent","tool_name":"invoke_subagent","subagent_info":{"subagents":[{"type_name":"research","initial_prompt":"look","conversation_id":"c2","log_uri":"file:///log"}]}}}"#,
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":4,"state":"DONE","step_type":"system_message"}}"#,
            r#"{"event":"result","result":{"status":"SUCCESS","response":"ok"}}"#,
        ]);
        assert_eq!(
            names(&events),
            [
                "init",
                "tool_started",
                "tool_finished",
                "subagent",
                "subagent",
                "result"
            ]
        );
        let statuses: Vec<SubagentStatus> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Subagent { update } => Some(update.status),
                _ => None,
            })
            .collect();
        assert_eq!(statuses, [SubagentStatus::Started, SubagentStatus::Done]);
    }

    #[test]
    fn a_killed_run_fails_its_running_subagents() {
        let mut parser = AgyStreamParser::new();
        parser.parse_stdout_line(INIT);
        parser.parse_stdout_line(
            r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"subagent","tool_name":"invoke_subagent","subagent_info":{"subagents":[{"conversation_id":"c2"}]}}}"#,
        );
        let finish = parser.finish(None);
        assert!(finish.iter().any(|e| matches!(
            e,
            AgentEvent::Subagent { update } if update.id == "c2" && update.status == SubagentStatus::Failed
        )));
    }

    #[test]
    fn exits_without_a_result_are_crashes() {
        for code in [0, 1] {
            let finish = AgyStreamParser::new().finish(Some(code));
            assert!(
                matches!(
                    &finish[..],
                    [
                        AgentEvent::Error {
                            category: ErrorCategory::Unknown,
                            retryable: false,
                            ..
                        },
                        AgentEvent::Result { is_error: true, .. },
                    ]
                ),
                "{code}: {finish:?}"
            );
        }
        let events = run(&[INIT], &["Error: something went wrong"], Some(1));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { category: ErrorCategory::Unknown, message, .. }
                if message.contains("something went wrong")
        )));
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
