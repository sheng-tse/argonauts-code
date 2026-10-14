use std::collections::{HashMap, HashSet};

use argonauts_core::screen::ScreenChange;
use argonauts_core::{AgentEvent, ErrorCategory, StreamParser};
use serde_json::Value;
use time::OffsetDateTime;

use crate::background::BackgroundTracker;
use crate::context::ContextTracker;
use crate::errors;
use crate::isolation;
use crate::json::{array_at, bool_at, str_at, string_at, strings_at, u64_at};
use crate::patch::file_patch;
use crate::quota::{self, LimitTracker};
use crate::settings::STATUS_SERVER;
use crate::subagents::SubagentTracker;
use crate::todo::TodoTracker;
use crate::usage::UsageTracker;
use crate::workflows::{self, WorkflowTracker};
use crate::{control, mods};

const FILE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// Text streams as [`AgentEvent::TextDelta`] and then arrives whole in [`AgentEvent::Message`],
/// while usage comes as increments until the result reports cumulative totals with cost.
/// A session with stream-json input has one result per turn.
#[derive(Debug, Default)]
pub struct ClaudeStreamParser {
    cwd: Option<String>,
    usage: UsageTracker,
    context: ContextTracker,
    todos: TodoTracker,
    subagents: SubagentTracker,
    workflows: WorkflowTracker,
    background: BackgroundTracker,
    open_tools: HashMap<String, OpenTool>,
    streaming_message_id: Option<String>,
    last_text: Option<String>,
    results_seen: usize,
    /// Output arrived after the last result, so a new turn is waiting for its own.
    turn_open: bool,
    /// Your messages Claude Code queued and hasn't started, by uuid (`command_lifecycle`).
    queued: HashSet<String>,
    /// A message started since the last result.
    message_started: bool,
    /// Whether this turn's failure was already reported, so the result doesn't repeat it.
    error_reported: bool,
    /// The category of the last result's error, for telling a crash from a clean failure.
    result_error: Option<ErrorCategory>,
    /// Retry categories already reported as errors. Claude Code sends one event per attempt.
    retry_errors: HashSet<ErrorCategory>,
    /// The servers init said Claude Code rejected. It runs on without them, so no turn is clean.
    rejected_mcp: Option<String>,
    /// The last stderr error nothing classified, to explain an exit without a result.
    stderr_error: Option<String>,
    /// A usage limit read this turn, which waits for the turn's end: Claude Code may finish on
    /// its wrap-up allowance.
    limits: LimitTracker,
    /// A mod opened a pane, which only Claude Code's own screen shows. Said once.
    pane_noted: bool,
    /// An isolated session's init is checked for what isolation keeps out, once.
    isolated: bool,
    leak_checked: bool,
    /// Why Claude Code turned down a tool call, by call id, for the call's own row.
    denials: HashMap<String, String>,
    /// Claude Code took this session's `initialize`, which says the screen stops background
    /// tasks one by one. Only then does stopping a turn spare them.
    spares_background: bool,
    finished: bool,
}

#[derive(Debug)]
struct OpenTool {
    name: String,
    input: Value,
}

impl ClaudeStreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// For an isolated session, whose init must list none of your plugins or synced skills.
    pub fn isolated() -> Self {
        Self {
            isolated: true,
            ..Self::default()
        }
    }

    /// `None` when the line isn't one this parser knows.
    fn on_line(&mut self, line: &Value) -> Option<Vec<AgentEvent>> {
        let kind = str_at(line, "type")?;
        let mut events = match kind {
            "system" => self.on_system(line),
            "stream_event" => self.on_stream_event(line),
            "assistant" => self.on_assistant(line),
            "user" => Some(self.on_user(line)),
            "result" => Some(self.on_result(line)),
            "rate_limit_event" => Some(self.on_rate_limit(line)),
            // The heartbeat during a long tool call. Every line already counts as output for
            // the watchdog, so it needs no event, and as an unknown one it would pile up.
            "tool_progress" => Some(Vec::new()),
            // Replies to control requests, by the request each answers.
            "control_response" => {
                self.spares_background |= control::opened_by(line);
                Some(control::from_response(line))
            }
            // A message's way through the queue: queued, started, completed.
            "command_lifecycle" => {
                if let Some(uuid) = str_at(line, "command_uuid") {
                    match str_at(line, "state") {
                        Some("queued") => {
                            self.queued.insert(uuid.to_owned());
                        }
                        Some("started") => {
                            self.queued.remove(uuid);
                            self.message_started = true;
                        }
                        _ => {
                            self.queued.remove(uuid);
                        }
                    }
                }
                Some(control::lifecycle(line).into_iter().collect())
            }
            _ => None,
        }?;
        // Output after a limit means Claude Code is finishing on its allowance. Its own error
        // message on a hard stop isn't the turn going on.
        let went_on =
            matches!(kind, "assistant" | "user" | "stream_event") && line.get("error").is_none();
        if went_on && let Some(reading) = self.limits.went_on(OffsetDateTime::now_utc()) {
            events.insert(0, AgentEvent::Quota { reading });
        }
        Some(events)
    }

    fn on_system(&mut self, line: &Value) -> Option<Vec<AgentEvent>> {
        let events = match str_at(line, "subtype")? {
            "init" => self.on_init(line),
            "api_retry" => vec![self.on_api_retry(line)],
            "permission_denied" => self.on_denied(line),
            // Transient hints about a request in flight or a running estimate of thinking tokens,
            // and the --name title.
            "status" | "thinking_tokens" | "session_title_changed" => Vec::new(),
            // What runs in the background, which keeps the session's input open.
            "background_tasks_changed" => self
                .background
                .observe("background_tasks_changed", line)
                .into_iter()
                .collect(),
            "commands_changed" => vec![AgentEvent::CommandsChanged {
                commands: control::commands(line),
            }],
            // Something drawn above the input changed, so a joined screen asks again.
            "ui_invalidate" if str_at(line, "event") == Some("ui.render") => {
                vec![AgentEvent::ScreenChanged {
                    change: ScreenChange::Stale,
                }]
            }
            "ui_panes" => {
                let first = !std::mem::replace(&mut self.pane_noted, true);
                first.then(mods::pane_notice).into_iter().collect()
            }
            subtype if mods::QUIET.contains(&subtype) => Vec::new(),
            subtype if let Some(output) = mods::output(subtype, line) => {
                let wrap_up = mods::says_wrapping_up(subtype, line)
                    .then(|| self.limits.notice(line, OffsetDateTime::now_utc()))
                    .flatten();
                wrap_up
                    .map(|reading| AgentEvent::Quota { reading })
                    .into_iter()
                    .chain([output])
                    .collect()
            }
            // A workflow is a subagent too, so both trackers read its task events.
            subtype => {
                let child = self.subagents.task_event(subtype, line)?;
                child
                    .into_iter()
                    .chain(self.workflows.task_event(subtype, line))
                    .chain(self.background.observe(subtype, line))
                    .collect()
            }
        };
        Some(events)
    }

    fn on_init(&mut self, line: &Value) -> Vec<AgentEvent> {
        self.turn_open = true;
        self.cwd = string_at(line, "cwd");
        let model = string_at(line, "model");
        self.context.set_model(model.clone());
        let mut events = vec![AgentEvent::Init {
            session_id: string_at(line, "session_id"),
            model,
            tools: strings_at(line, "tools"),
            plugins: plugin_names(line),
            capabilities: strings_at(line, "capabilities"),
            version: string_at(line, "claude_code_version"),
        }];
        // Under -p Claude Code skips a bad --mcp-config entry and runs on. An entry naming
        // Argonauts' own server means the adapter produced something this version rejects; your
        // own servers' troubles are yours. Each turn's init repeats it.
        if let Some(servers) = line.get("mcp_server_errors").and_then(mcp_error_summary)
            && self.rejected_mcp.is_none()
        {
            events.push(AgentEvent::Error {
                category: ErrorCategory::Cli,
                message: format!(
                    "adapter broken: Claude Code rejected Argonauts' MCP config ({servers}), \
                     so the status tool is missing"
                ),
                retryable: false,
            });
            self.rejected_mcp = Some(servers);
        }
        if self.isolated && !self.leak_checked {
            self.leak_checked = true;
            events.extend(isolation::leak(line));
        }
        events
    }

    /// Claude Code turned a call down without asking anyone, such as a flagged rm under
    /// `--permission-prompts none`. Its message says what to do instead, so it goes whole on the
    /// call's own row rather than as an error line.
    fn on_denied(&mut self, line: &Value) -> Vec<AgentEvent> {
        let reason = str_at(line, "message")
            .or_else(|| str_at(line, "decision_reason"))
            .unwrap_or("permission denied");
        match string_at(line, "tool_use_id") {
            Some(id) if self.open_tools.contains_key(&id) => {
                let words = format!("Claude Code turned this down: {reason}");
                self.denials.insert(id, words);
                Vec::new()
            }
            _ => {
                let tool = str_at(line, "tool_name").unwrap_or("A tool");
                vec![AgentEvent::Error {
                    category: ErrorCategory::Permission,
                    message: format!("{tool} was denied: {reason}"),
                    retryable: false,
                }]
            }
        }
    }

    /// Sent before each retry of an API request. Waiting fixes a rate limit or an overload, so
    /// those are retries; a login, billing, or request problem is reported once as an error.
    fn on_api_retry(&mut self, line: &Value) -> AgentEvent {
        let code = str_at(line, "error").unwrap_or("unknown");
        let category = errors::from_api_error(code, "");
        let lasting = category != ErrorCategory::Unknown && !errors::retryable(category);
        if lasting && self.retry_errors.insert(category) {
            let status = line
                .get("error_status")
                .and_then(Value::as_u64)
                .map(|s| format!(" (HTTP {s})"))
                .unwrap_or_default();
            return self.error(
                category,
                format!("Claude Code's API requests fail with {code}{status}"),
            );
        }
        AgentEvent::Retry {
            reason: errors::retry_reason(line),
            delay_ms: line.get("retry_delay_ms").and_then(Value::as_u64),
        }
    }

    fn on_rate_limit(&mut self, line: &Value) -> Vec<AgentEvent> {
        let info = line.get("rate_limit_info").unwrap_or(line);
        let Some(reading) = quota::reading(info, OffsetDateTime::now_utc()) else {
            return vec![AgentEvent::QuotaSignal {
                detail: info.clone(),
            }];
        };
        let others = quota::other_windows(info, &reading);
        let mut events = vec![AgentEvent::Quota {
            reading: self.limits.observe(reading),
        }];
        events.extend(
            others
                .into_iter()
                .map(|reading| AgentEvent::Quota { reading }),
        );
        events
    }

    fn on_stream_event(&mut self, line: &Value) -> Option<Vec<AgentEvent>> {
        let event = line.get("event")?;
        let main = string_at(line, "parent_tool_use_id").is_none();
        match str_at(event, "type")? {
            "message_start" => {
                self.turn_open = true;
                let message = event.get("message")?;
                self.streaming_message_id = string_at(message, "id");
                Some(self.observe_usage(str_at(message, "id"), message.get("usage"), main))
            }
            "message_delta" => {
                let id = self.streaming_message_id.clone();
                Some(self.observe_usage(id.as_deref(), event.get("usage"), main))
            }
            "content_block_delta" => Self::on_content_delta(line, event.get("delta")?),
            "message_stop" => {
                self.streaming_message_id = None;
                Some(Vec::new())
            }
            "content_block_start" | "content_block_stop" | "ping" => Some(Vec::new()),
            _ => None,
        }
    }

    fn on_content_delta(line: &Value, delta: &Value) -> Option<Vec<AgentEvent>> {
        match str_at(delta, "type")? {
            "text_delta" => {
                let Some(text) = str_at(delta, "text").filter(|text| !text.is_empty()) else {
                    return Some(Vec::new());
                };
                Some(vec![AgentEvent::TextDelta {
                    text: text.to_owned(),
                    parent: string_at(line, "parent_tool_use_id"),
                }])
            }
            // Complete assistant messages carry thinking, tool input, and citations whole.
            "thinking_delta" | "input_json_delta" | "signature_delta" | "citations_delta" => {
                Some(Vec::new())
            }
            _ => None,
        }
    }

    fn on_assistant(&mut self, line: &Value) -> Option<Vec<AgentEvent>> {
        let message = line.get("message")?;
        self.turn_open = true;
        let parent = string_at(line, "parent_tool_use_id");
        let mut events = Vec::new();
        let mut text_parts = Vec::new();
        for block in array_at(message, "content") {
            match str_at(block, "type") {
                Some("text") => {
                    let text = str_at(block, "text").unwrap_or_default();
                    if text.is_empty() {
                        continue;
                    }
                    text_parts.push(text);
                    if parent.is_none() {
                        self.last_text = Some(text.to_owned());
                    }
                    events.push(AgentEvent::Message {
                        text: text.to_owned(),
                        parent: parent.clone(),
                    });
                }
                Some("thinking") => {
                    if let Some(text) = str_at(block, "thinking").filter(|t| !t.is_empty()) {
                        events.push(AgentEvent::Reasoning {
                            text: text.to_owned(),
                        });
                    }
                }
                Some("redacted_thinking") => {}
                Some("tool_use" | "server_tool_use" | "mcp_tool_use") => {
                    events.extend(self.start_tool(block, parent.clone()));
                }
                _ => events.push(AgentEvent::Unknown { raw: block.clone() }),
            }
        }
        if let Some(child) = &parent {
            let text = (!text_parts.is_empty()).then(|| text_parts.join("\n"));
            // Before the child's own steps, so the tree has the child when they arrive.
            if let Some(update) = self.subagents.activity(child, text.as_deref()) {
                events.insert(0, update);
            }
        }
        if let Some(code) = str_at(line, "error") {
            events.push(self.api_error(code, &text_parts));
        }
        events.extend(self.observe_usage(
            str_at(message, "id"),
            message.get("usage"),
            parent.is_none(),
        ));
        Some(events)
    }

    fn api_error(&mut self, code: &str, text_parts: &[&str]) -> AgentEvent {
        let text = text_parts.join("\n");
        let category = errors::from_api_error(code, &text);
        let message = if text.is_empty() {
            code.to_owned()
        } else {
            text
        };
        self.error(category, message)
    }

    fn on_user(&mut self, line: &Value) -> Vec<AgentEvent> {
        let parent = string_at(line, "parent_tool_use_id");
        let content = line
            .get("message")
            .map_or(&[][..], |m| array_at(m, "content"));
        let results: Vec<&Value> = content
            .iter()
            .filter(|block| str_at(block, "type") == Some("tool_result"))
            .collect();
        // A subagent's prompt and its tool results come back as user messages.
        let mut events: Vec<AgentEvent> = match &parent {
            Some(child) => {
                self.turn_open = true;
                self.subagents.activity(child, None).into_iter().collect()
            }
            None => Vec::new(),
        };
        // `tool_use_result` belongs to the message, so it is only unambiguous for a single result.
        let structured = match results.as_slice() {
            [_] => line.get("tool_use_result"),
            _ => None,
        };
        for block in results {
            events.extend(self.finish_tool(block, structured, parent.clone()));
        }
        events
    }

    fn on_result(&mut self, line: &Value) -> Vec<AgentEvent> {
        if self.closes_a_left_over_notice(line) {
            return Vec::new();
        }
        self.message_started = false;
        self.results_seen += 1;
        self.turn_open = false;
        let mut events: Vec<AgentEvent> = self.usage.result_totals(line).into_iter().collect();
        events.extend(self.context.observe_result(line));
        let subtype = str_at(line, "subtype").unwrap_or("success");
        let mut is_error = bool_at(line, "is_error") || subtype != "success";
        let text = string_at(line, "result").filter(|t| !t.is_empty());
        self.result_error = None;
        let (reading, limit) = self.limits.turn_ended(is_error, OffsetDateTime::now_utc());
        events.extend(reading.map(|reading| AgentEvent::Quota { reading }));
        if let Some(message) = limit
            && !self.error_reported
        {
            events.push(self.error(ErrorCategory::UsageLimit, message));
        }
        if is_error {
            let message = result_error_message(line, subtype, text.as_deref());
            let category = errors::from_result(line, &message);
            self.result_error = Some(category);
            if !self.error_reported {
                events.push(self.error(category, message));
            }
        } else if let Some(servers) = &self.rejected_mcp {
            // The work may be fine, but the session ran without the status tool, so the
            // rejected config decides how it ends. Reported again as the turn's last error.
            let message = format!(
                "adapter broken: the turn ran without the status tool, since Claude Code \
                 rejected Argonauts' MCP config ({servers})"
            );
            is_error = true;
            self.result_error = Some(ErrorCategory::Cli);
            events.push(self.error(ErrorCategory::Cli, message));
        }
        events.push(AgentEvent::Result {
            text,
            structured: line
                .get("structured_output")
                .filter(|s| !s.is_null())
                .cloned(),
            is_error,
        });
        // The next turn reports its own failure.
        self.error_reported = false;
        events
    }

    /// A resumed session first tells the model about a background task the last process
    /// stopped: a turn with no model call, while your message still waits in the queue, whose
    /// empty result answers nothing you sent.
    fn closes_a_left_over_notice(&self, line: &Value) -> bool {
        !self.queued.is_empty()
            && !self.message_started
            && u64_at(line, "num_turns") == 0
            && str_at(line, "subtype") == Some("success")
            && !bool_at(line, "is_error")
            && str_at(line, "result").is_none_or(str::is_empty)
    }

    fn start_tool(&mut self, block: &Value, parent: Option<String>) -> Vec<AgentEvent> {
        let id = string_at(block, "id").unwrap_or_default();
        let name = string_at(block, "name").unwrap_or_default();
        let input = block.get("input").cloned().unwrap_or(Value::Null);
        let spawned = self
            .subagents
            .tool_started(&id, &name, &input, parent.as_deref());
        self.open_tools.insert(
            id.clone(),
            OpenTool {
                name: name.clone(),
                input: input.clone(),
            },
        );
        let mut events = vec![AgentEvent::ToolStarted {
            id,
            name,
            input,
            parent,
        }];
        events.extend(spawned);
        events
    }

    fn finish_tool(
        &mut self,
        block: &Value,
        structured: Option<&Value>,
        parent: Option<String>,
    ) -> Vec<AgentEvent> {
        let id = string_at(block, "tool_use_id").unwrap_or_default();
        let denial = self.denials.remove(&id);
        let ok = denial.is_none() && !bool_at(block, "is_error");
        let output = denial.or_else(|| tool_output_text(block.get("content")));
        let starts_workflow = self
            .open_tools
            .get(&id)
            .is_some_and(|tool| tool.name == workflows::TOOL);
        let output = output
            .filter(|text| !(ok && starts_workflow && workflows::launched(structured, Some(text))));
        let child_done = self
            .subagents
            .tool_finished(&id, ok, output.as_deref(), structured);
        let patch =
            self.open_tools.get(&id).filter(|_| ok).and_then(|tool| {
                file_patch(&id, &tool.name, &tool.input, structured, parent.clone())
            });
        let mut events: Vec<AgentEvent> = patch.into_iter().collect();
        events.push(AgentEvent::ToolFinished {
            id: id.clone(),
            ok,
            output,
            parent,
        });
        events.extend(child_done);
        if let Some(tool) = self.open_tools.remove(&id)
            && ok
        {
            events.extend(self.file_change(&tool, structured));
            events.extend(self.todos.apply(&tool.name, &tool.input, structured));
        }
        events
    }

    fn file_change(&self, tool: &OpenTool, structured: Option<&Value>) -> Option<AgentEvent> {
        if !FILE_TOOLS.contains(&tool.name.as_str()) {
            return None;
        }
        let path = structured
            .and_then(|s| str_at(s, "filePath"))
            .or_else(|| str_at(&tool.input, "file_path"))
            .or_else(|| str_at(&tool.input, "notebook_path"))?;
        let kind = match structured.and_then(|s| str_at(s, "type")) {
            Some("create") => "add",
            _ => "update",
        };
        Some(AgentEvent::FileChanged {
            path: self.relative_to_cwd(path),
            kind: kind.to_owned(),
        })
    }

    fn relative_to_cwd(&self, path: &str) -> String {
        self.cwd
            .as_deref()
            .and_then(|cwd| path.strip_prefix(cwd))
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|rest| !rest.is_empty())
            .unwrap_or(path)
            .to_owned()
    }

    /// `main` is false for a subagent's messages, which count toward spend but fill their own
    /// context rather than the session's.
    fn observe_usage(
        &mut self,
        id: Option<&str>,
        usage: Option<&Value>,
        main: bool,
    ) -> Vec<AgentEvent> {
        let (Some(id), Some(usage)) = (id, usage) else {
            return Vec::new();
        };
        let mut events: Vec<AgentEvent> = self.usage.observe(id, usage).into_iter().collect();
        if main {
            events.extend(self.context.observe(usage));
        }
        events
    }

    fn error(&mut self, category: ErrorCategory, message: String) -> AgentEvent {
        self.error_reported = true;
        AgentEvent::Error {
            category,
            message,
            retryable: errors::retryable(category),
        }
    }

    /// A process that failed with a result nobody could classify, then exited non-zero, crashed.
    fn crash_after_error_result(&self, exit_code: Option<i32>) -> Option<AgentEvent> {
        let code = exit_code.filter(|code| !matches!(code, 0 | 130 | 143))?;
        (self.result_error == Some(ErrorCategory::Unknown)).then(|| AgentEvent::Error {
            category: ErrorCategory::Unknown,
            message: format!("claude exited with code {code} after an error it didn't explain"),
            retryable: false,
        })
    }
}

impl StreamParser for ClaudeStreamParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![AgentEvent::unknown_line(line)];
        };
        self.on_line(&value)
            .unwrap_or_else(|| vec![AgentEvent::Unknown { raw: value }])
    }

    fn stop_subagent_line(&self, id: &str) -> Option<String> {
        self.subagents.task_of(id).map(control::stop_task_line)
    }

    fn stop_turn_line(&self) -> Option<String> {
        self.spares_background.then(control::stop_turn_line)
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        if line.to_lowercase().starts_with("error") {
            if let Some(category) = errors::from_text(line) {
                return vec![self.error(category, line.to_owned())];
            }
            // Unexplained, so it leaves the turn's verdict to the result or the exit, and it
            // doesn't overrule an error that already explained the turn.
            if !self.error_reported {
                self.stderr_error = Some(line.to_owned());
                return vec![AgentEvent::Error {
                    category: ErrorCategory::Unknown,
                    message: line.to_owned(),
                    retryable: false,
                }];
            }
        }
        // Other stderr is free text, not the event stream, so it never counts as an unknown
        // event. The raw log and the worker's stderr tail keep it.
        Vec::new()
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        if std::mem::replace(&mut self.finished, true) {
            return Vec::new();
        }
        let mut events = self
            .workflows
            .close("its session ended before the workflow did");
        if self.results_seen > 0 && !self.turn_open {
            events.extend(self.crash_after_error_result(exit_code));
            return events;
        }
        if !self.error_reported {
            let (category, message) = match self.limits.unfinished() {
                Some(limit) => (ErrorCategory::UsageLimit, limit),
                None => missing_result_error(exit_code, self.stderr_error.as_deref()),
            };
            events.push(self.error(category, message));
        }
        events.push(AgentEvent::Result {
            text: self.last_text.take(),
            structured: None,
            is_error: true,
        });
        events
    }
}

/// The plugins and mods `init` lists, by name. Older versions list bare names.
fn plugin_names(init: &Value) -> Vec<String> {
    array_at(init, "plugins")
        .iter()
        .filter_map(|plugin| match plugin {
            Value::String(name) => Some(name.clone()),
            other => string_at(other, "name"),
        })
        .collect()
}

/// Argonauts' own server as `mcp_server_errors` names it, with its error, or `None` when it
/// isn't named. Its shape isn't documented, so strings, objects, and a map by server name all read.
fn mcp_error_summary(errors: &Value) -> Option<String> {
    let entries: Vec<String> = match errors {
        Value::Array(items) => items.iter().map(describe_mcp_error).collect(),
        Value::Object(map) => map
            .iter()
            .map(|(name, error)| format!("{name}: {}", describe_mcp_error(error)))
            .collect(),
        Value::String(text) => vec![text.clone()],
        _ => Vec::new(),
    };
    let entries: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.contains(STATUS_SERVER))
        .collect();
    (!entries.is_empty()).then(|| entries.join("; "))
}

fn describe_mcp_error(error: &Value) -> String {
    match error {
        Value::String(text) => text.clone(),
        Value::Object(_) => {
            let name = str_at(error, "name").or_else(|| str_at(error, "server"));
            let detail = str_at(error, "error").or_else(|| str_at(error, "message"));
            match (name, detail) {
                (Some(name), Some(detail)) => format!("{name}: {detail}"),
                (Some(text), None) | (None, Some(text)) => text.to_owned(),
                (None, None) => error.to_string(),
            }
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A crash unless a signal ended it. `said` is the last unexplained stderr error.
fn missing_result_error(exit_code: Option<i32>, said: Option<&str>) -> (ErrorCategory, String) {
    match exit_code {
        None => (
            ErrorCategory::Interrupted,
            "claude was stopped by a signal before it reported a result".to_owned(),
        ),
        // 128 plus SIGINT or SIGTERM.
        Some(code @ (130 | 143)) => (
            ErrorCategory::Interrupted,
            format!("claude was interrupted (exit code {code}) before it reported a result"),
        ),
        Some(code) => {
            let said = said.map(|line| format!(": {line}")).unwrap_or_default();
            (
                ErrorCategory::Unknown,
                format!("claude exited with code {code} before it reported a result{said}"),
            )
        }
    }
}

fn tool_output_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let parts: Vec<String> = blocks
                .iter()
                .map(|block| match str_at(block, "type") {
                    Some("text") => str_at(block, "text").unwrap_or_default().to_owned(),
                    // ToolSearch's result names each tool it loaded (2.1.289).
                    Some("tool_reference") => str_at(block, "tool_name")
                        .map_or_else(|| "[tool_reference]".to_owned(), str::to_owned),
                    Some(other) => format!("[{other}]"),
                    None => String::new(),
                })
                .collect();
            Some(parts.join("\n"))
        }
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn result_error_message(result: &Value, subtype: &str, text: Option<&str>) -> String {
    let errors = strings_at(result, "errors");
    if !errors.is_empty() {
        return errors.join("; ");
    }
    match text {
        Some(text) => text.to_owned(),
        None => format!("Claude Code ended with {subtype}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::SubagentStatus;
    use argonauts_core::limits::QuotaStatus;
    use argonauts_core::{RetryReason, TodoStatus};
    use serde_json::json;

    fn parse(parser: &mut ClaudeStreamParser, line: Value) -> Vec<AgentEvent> {
        parser.parse_stdout_line(&line.to_string())
    }

    fn types(events: &[AgentEvent]) -> Vec<&'static str> {
        events.iter().map(AgentEvent::type_name).collect()
    }

    #[test]
    fn malformed_and_blank_lines() {
        let mut parser = ClaudeStreamParser::new();
        assert!(parser.parse_stdout_line("   ").is_empty());
        assert_eq!(
            parser.parse_stdout_line("{not json"),
            [AgentEvent::Unknown {
                raw: json!("{not json")
            }]
        );
        assert_eq!(
            parser.parse_stdout_line("[1,2]"),
            [AgentEvent::Unknown { raw: json!([1, 2]) }]
        );
        assert_eq!(
            parse(&mut parser, json!({"type": "brand_new", "x": 1})),
            [AgentEvent::Unknown {
                raw: json!({"type": "brand_new", "x": 1})
            }]
        );
        assert_eq!(
            types(&parse(
                &mut parser,
                json!({"type": "system", "subtype": "compact_boundary"})
            )),
            ["unknown"]
        );
    }

    #[test]
    fn init_records_session_and_cwd() {
        let mut parser = ClaudeStreamParser::new();
        let events = parse(
            &mut parser,
            json!({"type": "system", "subtype": "init", "session_id": "s-1", "cwd": "/w",
                   "model": "claude-haiku", "tools": ["Read", "Bash"]}),
        );
        assert_eq!(
            events,
            [AgentEvent::Init {
                session_id: Some("s-1".into()),
                model: Some("claude-haiku".into()),
                tools: vec!["Read".into(), "Bash".into()],
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            }]
        );
        assert_eq!(parser.relative_to_cwd("/w/src/a.rs"), "src/a.rs");
        assert_eq!(parser.relative_to_cwd("/other/a.rs"), "/other/a.rs");
        assert_eq!(parser.relative_to_cwd("/wx/a.rs"), "/wx/a.rs");
    }

    #[test]
    fn stream_text_deltas_and_usage_are_not_double_counted() {
        let mut parser = ClaudeStreamParser::new();
        let start = parse(
            &mut parser,
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {
                "type": "message_start",
                "message": {"id": "msg_1", "usage": {"input_tokens": 10, "output_tokens": 1}}}}),
        );
        assert_eq!(types(&start), ["usage", "context_fill"]);
        let delta = parse(
            &mut parser,
            json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "Hel"}}}),
        );
        assert_eq!(
            delta,
            [AgentEvent::TextDelta {
                text: "Hel".into(),
                parent: None
            }]
        );
        let assistant = parse(
            &mut parser,
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {
                "id": "msg_1", "content": [{"type": "text", "text": "Hello"}],
                "usage": {"input_tokens": 10, "output_tokens": 1}}}),
        );
        assert_eq!(types(&assistant), ["message"]);
        let finished = parse(
            &mut parser,
            json!({"type": "stream_event", "event": {"type": "message_delta",
                   "usage": {"output_tokens": 7}}}),
        );
        assert_eq!(
            finished,
            [AgentEvent::Usage {
                input_tokens: 0,
                output_tokens: 6,
                cached_tokens: 0,
                cost_usd: None,
                cumulative: false
            }]
        );
    }

    #[test]
    fn bookkeeping_lines_produce_no_events() {
        let mut parser = ClaudeStreamParser::new();
        let lines = [
            json!({"type": "stream_event", "event": {"type": "ping"}}),
            json!({"type": "stream_event", "event": {"type": "content_block_start"}}),
            json!({"type": "stream_event", "event": {"type": "content_block_stop"}}),
            json!({"type": "stream_event", "event": {"type": "message_stop"}}),
            json!({"type": "stream_event", "event": {"type": "content_block_delta",
                   "delta": {"type": "input_json_delta", "partial_json": "{"}}}),
            json!({"type": "system", "subtype": "thinking_tokens", "estimated_tokens": 5}),
            json!({"type": "system", "subtype": "status", "status": "requesting"}),
        ];
        for line in lines {
            let line = line.to_string();
            assert!(parser.parse_stdout_line(&line).is_empty(), "{line}");
        }
    }

    #[test]
    fn a_heartbeat_produces_no_event() {
        let mut parser = ClaudeStreamParser::new();
        let line = json!({"type": "tool_progress", "tool_use_id": "t1",
                          "elapsed_time_seconds": 30, "heartbeat": true});
        assert!(parser.parse_stdout_line(&line.to_string()).is_empty());
    }

    #[test]
    fn tool_calls_file_changes_and_todos() {
        let mut parser = ClaudeStreamParser::new();
        parse(
            &mut parser,
            json!({"type": "system", "subtype": "init", "cwd": "/w", "tools": []}),
        );
        let started = parse(
            &mut parser,
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": "m1", "content": [
                {"type": "tool_use", "id": "t1", "name": "Write",
                 "input": {"file_path": "/w/hello.txt", "content": "hi"}},
                {"type": "tool_use", "id": "t2", "name": "TodoWrite",
                 "input": {"todos": [{"content": "Say hi", "status": "in_progress"}]}}
            ]}}),
        );
        assert_eq!(types(&started), ["tool_started", "tool_started"]);
        let finished = parse(
            &mut parser,
            json!({"type": "user", "parent_tool_use_id": null,
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "t1",
                               "content": "File created"}]},
                   "tool_use_result": {"type": "create", "filePath": "/w/hello.txt"}}),
        );
        assert_eq!(
            finished,
            [
                AgentEvent::ToolFinished {
                    id: "t1".into(),
                    ok: true,
                    output: Some("File created".into()),
                    parent: None
                },
                AgentEvent::FileChanged {
                    path: "hello.txt".into(),
                    kind: "add".into()
                }
            ]
        );
        let todo = parse(
            &mut parser,
            json!({"type": "user", "message": {"content": [{"type": "tool_result",
                   "tool_use_id": "t2", "content": [{"type": "text", "text": "ok"}]}]}}),
        );
        assert_eq!(types(&todo), ["tool_finished", "todo"]);
        let AgentEvent::Todo { items } = &todo[1] else {
            panic!("expected todo");
        };
        assert_eq!(items[0].status, TodoStatus::InProgress);
    }

    #[test]
    fn an_edit_reports_its_patch_before_it_finishes() {
        let mut parser = ClaudeStreamParser::new();
        parse(
            &mut parser,
            json!({"type": "system", "subtype": "init", "cwd": "/w", "tools": []}),
        );
        parse(
            &mut parser,
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": "m1", "content": [
                {"type": "tool_use", "id": "t1", "name": "Edit",
                 "input": {"file_path": "/w/notes.md", "old_string": "draft", "new_string": "final"}}
            ]}}),
        );
        let finished = parse(
            &mut parser,
            json!({"type": "user", "parent_tool_use_id": null,
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "t1",
                               "content": "The file has been updated."}]},
                   "tool_use_result": {"filePath": "/w/notes.md", "oldString": "draft",
                       "newString": "final", "structuredPatch": [{"oldStart": 1, "oldLines": 1,
                       "newStart": 1, "newLines": 1,
                       "lines": ["-This is a draft.", "+This is a final."]}]}}),
        );
        assert_eq!(
            types(&finished),
            ["file_patch", "tool_finished", "file_changed"]
        );
        assert_eq!(
            finished[0],
            AgentEvent::FilePatch {
                id: "t1".into(),
                path: "/w/notes.md".into(),
                hunks: "@@ -1,1 +1,1 @@\n-This is a draft.\n+This is a final.\n".into(),
                parent: None,
            }
        );
    }

    #[test]
    fn failed_tools_report_no_file_change() {
        let mut parser = ClaudeStreamParser::new();
        parse(
            &mut parser,
            json!({"type": "assistant", "parent_tool_use_id": "agent-1", "message": {"content": [
                {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "a.rs"}}]}}),
        );
        let finished = parse(
            &mut parser,
            json!({"type": "user", "parent_tool_use_id": "agent-1", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "is_error": true,
                 "content": "String not found"}]}}),
        );
        assert_eq!(
            finished,
            [AgentEvent::ToolFinished {
                id: "t1".into(),
                ok: false,
                output: Some("String not found".into()),
                parent: Some("agent-1".into())
            }]
        );
    }

    #[test]
    fn user_prompts_and_thinking() {
        let mut parser = ClaudeStreamParser::new();
        assert!(
            parse(
                &mut parser,
                json!({"type": "user", "message": {"role": "user", "content": "do it"}})
            )
            .is_empty()
        );
        let events = parse(
            &mut parser,
            json!({"type": "assistant", "message": {"content": [
                {"type": "thinking", "thinking": "Let me see"},
                {"type": "thinking", "thinking": ""},
                {"type": "redacted_thinking", "data": "xx"},
                {"type": "mystery", "v": 1}]}}),
        );
        assert_eq!(
            events,
            [
                AgentEvent::Reasoning {
                    text: "Let me see".into()
                },
                AgentEvent::Unknown {
                    raw: json!({"type": "mystery", "v": 1})
                }
            ]
        );
    }

    #[test]
    fn retries_quota_and_denials() {
        let mut parser = ClaudeStreamParser::new();
        assert_eq!(
            parse(
                &mut parser,
                json!({"type": "system", "subtype": "api_retry", "attempt": 1, "max_retries": 10,
                       "retry_delay_ms": 1200, "error_status": 529, "error": "overloaded"})
            ),
            [AgentEvent::Retry {
                reason: RetryReason::Overloaded,
                delay_ms: Some(1200)
            }]
        );
        let info =
            json!({"status": "allowed_warning", "utilization": 0.9, "rateLimitType": "five_hour"});
        let quota = parse(
            &mut parser,
            json!({"type": "rate_limit_event", "rate_limit_info": info.clone()}),
        );
        let [AgentEvent::Quota { reading }] = quota.as_slice() else {
            panic!("expected one quota reading, got {quota:?}");
        };
        assert_eq!(reading.status, QuotaStatus::Warning);
        assert_eq!(reading.used, Some(0.9));
        assert_eq!(reading.detail, Some(info));
        // A payload without a status can't be normalized, so it stays raw.
        let odd = json!({"note": "new shape"});
        assert_eq!(
            parse(
                &mut parser,
                json!({"type": "rate_limit_event", "rate_limit_info": odd.clone()})
            ),
            [AgentEvent::QuotaSignal { detail: odd }]
        );
        // A denial for a call this parser never saw start has no row to go on.
        let denied = parse(
            &mut parser,
            json!({"type": "system", "subtype": "permission_denied", "tool_name": "Bash",
                   "tool_use_id": "t9", "message": "not allowed"}),
        );
        assert_eq!(
            denied,
            [AgentEvent::Error {
                category: ErrorCategory::Permission,
                message: "Bash was denied: not allowed".into(),
                retryable: false
            }]
        );
    }

    #[test]
    fn successful_result_reports_totals_and_structured_output() {
        let mut parser = ClaudeStreamParser::new();
        let events = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": false, "result": "done",
                   "total_cost_usd": 0.01, "usage": {"input_tokens": 5, "output_tokens": 3},
                   "structured_output": {"status": "done"}}),
        );
        assert_eq!(
            events,
            [
                AgentEvent::Usage {
                    input_tokens: 5,
                    output_tokens: 3,
                    cached_tokens: 0,
                    cost_usd: Some(0.01),
                    cumulative: true
                },
                AgentEvent::Result {
                    text: Some("done".into()),
                    structured: Some(json!({"status": "done"})),
                    is_error: false
                }
            ]
        );
        assert!(parser.finish(Some(0)).is_empty());
    }

    #[test]
    fn error_results_carry_their_category_and_message() {
        let mut parser = ClaudeStreamParser::new();
        let events = parse(
            &mut parser,
            json!({"type": "result", "subtype": "error_max_budget_usd", "is_error": true,
                   "errors": ["Reached maximum budget ($0.01)"], "total_cost_usd": 0.011}),
        );
        assert_eq!(types(&events), ["error", "result"]);
        assert_eq!(
            events[0],
            AgentEvent::Error {
                category: ErrorCategory::Budget,
                message: "Reached maximum budget ($0.01)".into(),
                retryable: false
            }
        );
    }

    #[test]
    fn an_assistant_error_is_not_reported_again_by_the_result() {
        let mut parser = ClaudeStreamParser::new();
        let assistant = parse(
            &mut parser,
            json!({"type": "assistant", "error": "authentication_failed", "message": {
                "id": "x", "content": [{"type": "text", "text": "Not logged in · Please run /login"}]}}),
        );
        assert_eq!(types(&assistant), ["message", "error"]);
        let result = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": true,
                   "result": "Not logged in · Please run /login"}),
        );
        assert_eq!(types(&result), ["result"]);
    }

    #[test]
    fn finish_without_a_result_reports_an_error_and_the_last_text() {
        let mut parser = ClaudeStreamParser::new();
        parse(
            &mut parser,
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "partial"}]}}),
        );
        let events = parser.finish(Some(143));
        assert_eq!(
            events,
            [
                AgentEvent::Error {
                    category: ErrorCategory::Interrupted,
                    message: "claude was interrupted (exit code 143) before it reported a result"
                        .into(),
                    retryable: false
                },
                AgentEvent::Result {
                    text: Some("partial".into()),
                    structured: None,
                    is_error: true
                }
            ]
        );
        assert!(parser.finish(Some(143)).is_empty());

        let mut parser = ClaudeStreamParser::new();
        let events = parser.finish(None);
        assert_eq!(types(&events), ["error", "result"]);

        // Any other exit without a result is a crash, not a sign the adapter broke.
        for code in [0, 1] {
            let events = ClaudeStreamParser::new().finish(Some(code));
            assert_eq!(
                events[0],
                AgentEvent::Error {
                    category: ErrorCategory::Unknown,
                    message: format!("claude exited with code {code} before it reported a result"),
                    retryable: false
                }
            );
        }
    }

    #[test]
    fn unexplained_stderr_errors_leave_the_verdict_to_the_result_or_exit() {
        let mut parser = ClaudeStreamParser::new();
        let events = parser.parse_stderr_line("Error: something broke");
        assert_eq!(
            events,
            [AgentEvent::Error {
                category: ErrorCategory::Unknown,
                message: "Error: something broke".into(),
                retryable: false
            }]
        );
        let result = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": true,
                   "result": "You've hit your usage limit"}),
        );
        assert!(matches!(
            &result[0],
            AgentEvent::Error {
                category: ErrorCategory::UsageLimit,
                ..
            }
        ));

        let mut parser = ClaudeStreamParser::new();
        parser.parse_stderr_line("Error: something broke");
        let events = parser.finish(Some(1));
        assert_eq!(
            events[0],
            AgentEvent::Error {
                category: ErrorCategory::Unknown,
                message: "claude exited with code 1 before it reported a result: \
                          Error: something broke"
                    .into(),
                retryable: false
            }
        );

        // Once an error explained the turn, a vague one on stderr doesn't overrule it, and like
        // other stderr text it isn't an unknown event either.
        let mut parser = ClaudeStreamParser::new();
        parser.parse_stderr_line("Error: Prompt is too long");
        assert_eq!(parser.parse_stderr_line("Error: something broke"), []);
    }

    #[test]
    fn stderr_errors_are_categorized_and_other_lines_ignored() {
        let mut parser = ClaudeStreamParser::new();
        assert!(parser.parse_stderr_line("").is_empty());
        let events = parser.parse_stderr_line(
            "Error: When using --print, --output-format=stream-json requires --verbose",
        );
        assert_eq!(
            events,
            [AgentEvent::Error {
                category: ErrorCategory::InvalidRequest,
                message:
                    "Error: When using --print, --output-format=stream-json requires --verbose"
                        .into(),
                retryable: false
            }]
        );
        assert_eq!(parser.parse_stderr_line("Warning: something"), []);
        assert_eq!(types(&parser.finish(Some(1))), ["result"]);
    }

    #[test]
    fn tool_output_shapes() {
        assert_eq!(tool_output_text(None), None);
        assert_eq!(tool_output_text(Some(&Value::Null)), None);
        assert_eq!(tool_output_text(Some(&json!("x"))).as_deref(), Some("x"));
        assert_eq!(
            tool_output_text(Some(
                &json!([{"type": "text", "text": "a"}, {"type": "image"}])
            ))
            .as_deref(),
            Some("a\n[image]")
        );
        // What ToolSearch loaded, by name (2.1.289's direct turns load Edit and Bash this way).
        let loaded = json!([
            {"type": "tool_reference", "tool_name": "Edit"},
            {"type": "tool_reference", "tool_name": "Bash"}
        ]);
        assert_eq!(
            tool_output_text(Some(&loaded)).as_deref(),
            Some("Edit\nBash")
        );
    }

    fn init_with(mcp_server_errors: Value) -> Value {
        json!({"type": "system", "subtype": "init", "session_id": "s-1", "model": "claude-haiku",
               "tools": ["Read"], "mcp_servers": [], "mcp_server_errors": mcp_server_errors})
    }

    #[test]
    fn your_own_mcp_servers_failing_is_not_the_adapters_fault() {
        let mut parser = ClaudeStreamParser::new();
        let errors = json!([{"name": "github", "error": "connection refused"}]);
        assert_eq!(types(&parse(&mut parser, init_with(errors))), ["init"]);
        let mixed = json!({"github": "connection refused", "argonauts_status": "bad"});
        let events = parse(&mut ClaudeStreamParser::new(), init_with(mixed));
        let AgentEvent::Error { message, .. } = &events[1] else {
            unreachable!("{events:?}")
        };
        assert!(message.contains("argonauts_status: bad"), "{message}");
        assert!(!message.contains("github"), "{message}");
    }

    #[test]
    fn claude_mcp_error_in_init_is_adapter_broken() {
        let shapes = [
            json!([{"name": "argonauts_status", "error": "command: Required"}]),
            json!(["argonauts_status: command: Required"]),
            json!({"argonauts_status": "command: Required"}),
        ];
        for errors in shapes {
            let mut parser = ClaudeStreamParser::new();
            let events = parse(&mut parser, init_with(errors.clone()));
            assert_eq!(types(&events), ["init", "error"], "{errors}");
            let AgentEvent::Error {
                category,
                message,
                retryable,
            } = &events[1]
            else {
                unreachable!()
            };
            assert_eq!(*category, ErrorCategory::Cli);
            assert!(!retryable);
            assert!(message.starts_with("adapter broken"), "{message}");
            assert!(
                message.contains("argonauts_status: command: Required"),
                "{message}"
            );
            // The session still reports its own result error, if it has one.
            let result = parse(
                &mut parser,
                json!({"type": "result", "subtype": "success", "is_error": true,
                       "result": "Invalid API key"}),
            );
            assert_eq!(types(&result), ["error", "result"]);
            assert!(matches!(
                &result[0],
                AgentEvent::Error {
                    category: ErrorCategory::Auth,
                    ..
                }
            ));

            // The next turn's init names the same servers, and its clean result still ran
            // without the status tool.
            assert_eq!(types(&parse(&mut parser, init_with(errors))), ["init"]);
            let result = parse(
                &mut parser,
                json!({"type": "result", "subtype": "success", "is_error": false, "result": "ok"}),
            );
            assert_eq!(types(&result), ["error", "result"]);
            assert!(matches!(
                &result[0],
                AgentEvent::Error { category: ErrorCategory::Cli, message, retryable: false }
                    if message.contains("argonauts_status")
            ));
            assert!(matches!(
                &result[1],
                AgentEvent::Result { is_error: true, .. }
            ));
        }
        for clean in [json!([]), json!({}), Value::Null, json!([null])] {
            let events = parse(&mut ClaudeStreamParser::new(), init_with(clean.clone()));
            assert_eq!(types(&events), ["init"], "{clean}");
        }
    }

    #[test]
    fn claude_api_retry_categories() {
        let retry = |parser: &mut ClaudeStreamParser, error: &str, status: Value| {
            parse(
                parser,
                json!({"type": "system", "subtype": "api_retry", "attempt": 1, "max_retries": 10,
                       "retry_delay_ms": 500, "error_status": status, "error": error}),
            )
        };
        let mut parser = ClaudeStreamParser::new();
        for (error, status, reason) in [
            ("rate_limit", json!(429), RetryReason::RateLimit),
            ("overloaded", json!(529), RetryReason::Overloaded),
            ("server_error", json!(500), RetryReason::ServerError),
            ("unknown", Value::Null, RetryReason::Network),
            (
                "max_output_tokens",
                json!(200),
                RetryReason::Other("max_output_tokens".into()),
            ),
        ] {
            assert_eq!(
                retry(&mut parser, error, status),
                [AgentEvent::Retry {
                    reason,
                    delay_ms: Some(500)
                }],
                "{error}"
            );
        }
        for (error, category) in [
            ("authentication_failed", ErrorCategory::Auth),
            ("oauth_org_not_allowed", ErrorCategory::Auth),
            ("account_on_hold", ErrorCategory::Auth),
            ("billing_error", ErrorCategory::Billing),
            ("model_not_found", ErrorCategory::ModelUnavailable),
        ] {
            let mut parser = ClaudeStreamParser::new();
            let first = retry(&mut parser, error, json!(401));
            assert_eq!(
                first,
                [AgentEvent::Error {
                    category,
                    message: format!("Claude Code's API requests fail with {error} (HTTP 401)"),
                    retryable: false
                }],
                "{error}"
            );
            // Later attempts are only retries, and the result doesn't report it again.
            assert_eq!(types(&retry(&mut parser, error, json!(401))), ["retry"]);
            let end = parse(
                &mut parser,
                json!({"type": "result", "subtype": "success", "is_error": true,
                       "result": "Failed to authenticate"}),
            );
            assert_eq!(types(&end), ["result"]);
        }
    }

    fn rejected() -> Value {
        json!({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected",
               "resetsAt": 1790678400, "rateLimitType": "five_hour"}})
    }

    fn assistant_text(id: &str, text: &str) -> Value {
        json!({"type": "assistant", "message": {"id": id, "content": [{"type": "text", "text": text}]}})
    }

    fn readings(events: &[AgentEvent]) -> Vec<(QuotaStatus, bool)> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Quota { reading } => Some((reading.status, reading.wrap_up)),
                _ => None,
            })
            .collect()
    }

    fn usage_limit_errors(events: &[AgentEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AgentEvent::Error {
                        category: ErrorCategory::UsageLimit,
                        ..
                    }
                )
            })
            .count()
    }

    /// Claude Code's wrap-up allowance: the turn goes on after a `rejected` reading, so the
    /// agent is wrapping up rather than out, and a clean end brings no error.
    #[test]
    fn a_limit_the_turn_goes_on_past_is_a_wrap_up() {
        let mut parser = ClaudeStreamParser::new();
        let mut events = parse(&mut parser, rejected());
        assert_eq!(types(&events), ["quota"]);
        events.extend(parse(
            &mut parser,
            assistant_text("m2", "Finishing the step."),
        ));
        events.extend(parse(
            &mut parser,
            assistant_text("m3", "Left: format_cents."),
        ));
        let end = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": false, "result": "done"}),
        );
        assert_eq!(
            readings(&events),
            [
                (QuotaStatus::Rejected, false),
                (QuotaStatus::Rejected, true)
            ]
        );
        assert_eq!(types(&events)[1..3], ["quota", "message"]);
        // At the end it's out until the reset, and nothing failed.
        assert_eq!(readings(&end), [(QuotaStatus::Rejected, false)]);
        assert_eq!(usage_limit_errors(&events) + usage_limit_errors(&end), 0);
        assert!(matches!(
            end.last(),
            Some(AgentEvent::Result {
                is_error: false,
                ..
            })
        ));
    }

    #[test]
    fn a_limit_the_turn_ends_on_is_one_usage_limit_error_at_the_end() {
        let mut parser = ClaudeStreamParser::new();
        assert_eq!(types(&parse(&mut parser, rejected())), ["quota"]);
        let end = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": true,
                   "result": "You've hit your limit"}),
        );
        assert_eq!(types(&end), ["error", "result"]);
        let AgentEvent::Error {
            category,
            retryable,
            message,
        } = &end[0]
        else {
            unreachable!()
        };
        assert_eq!(*category, ErrorCategory::UsageLimit);
        assert!(*retryable && message.contains("five_hour"));

        // Wrapping up, then failing after all: out, with one error.
        let mut parser = ClaudeStreamParser::new();
        parse(&mut parser, rejected());
        parse(&mut parser, assistant_text("m2", "Finishing."));
        let end = parse(
            &mut parser,
            json!({"type": "result", "subtype": "error_during_execution", "is_error": true}),
        );
        assert_eq!(readings(&end), [(QuotaStatus::Rejected, false)]);
        assert_eq!(usage_limit_errors(&end), 1);
        assert_eq!(
            end.iter()
                .filter(|e| matches!(e, AgentEvent::Error { .. }))
                .count(),
            1
        );

        // Nothing after the reading: the limit stopped the turn.
        let mut parser = ClaudeStreamParser::new();
        parse(&mut parser, rejected());
        let end = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": false, "result": "ok"}),
        );
        assert_eq!(usage_limit_errors(&end), 1);

        // Claude Code's own error message on a hard stop isn't the turn going on.
        let mut parser = ClaudeStreamParser::new();
        parse(&mut parser, rejected());
        let said = parse(
            &mut parser,
            json!({"type": "assistant", "error": "rate_limit", "message": {"id": "m9",
                   "content": [{"type": "text", "text": "You've hit your limit"}]}}),
        );
        assert!(readings(&said).is_empty());
        let end = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": true}),
        );
        assert_eq!(
            usage_limit_errors(&said) + usage_limit_errors(&end),
            1,
            "{said:?} {end:?}"
        );

        // A stream cut off with a limit waiting explains itself.
        let mut parser = ClaudeStreamParser::new();
        parse(&mut parser, rejected());
        let finish = parser.finish(Some(1));
        assert_eq!(usage_limit_errors(&finish), 1);
    }

    #[test]
    fn a_full_window_that_still_allows_or_the_notice_is_a_wrap_up() {
        let mut parser = ClaudeStreamParser::new();
        let full = parse(
            &mut parser,
            json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed",
                   "rateLimitType": "five_hour", "unifiedWindows": {
                       "five_hour": {"utilization": 1.0, "resetsAt": 1790678400}}}}),
        );
        assert_eq!(readings(&full), [(QuotaStatus::Rejected, true)]);
        let end = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": false}),
        );
        assert_eq!(readings(&end), [(QuotaStatus::Rejected, false)]);
        assert_eq!(usage_limit_errors(&end), 0);

        // Paid overage carries the session, so it's neither.
        let mut parser = ClaudeStreamParser::new();
        let paid = parse(
            &mut parser,
            json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed",
                   "isUsingOverage": true, "utilization": 1.0}}),
        );
        assert_eq!(readings(&paid), [(QuotaStatus::Allowed, false)]);

        // The notice alone.
        let mut parser = ClaudeStreamParser::new();
        let notice = parse(
            &mut parser,
            json!({"type": "system", "subtype": "notification", "key": "limit",
                   "text": "Usage limit reached · wrapping up · clau.de/wrap-up",
                   "priority": "high"}),
        );
        assert_eq!(types(&notice), ["quota", "mod_output"]);
        assert_eq!(readings(&notice), [(QuotaStatus::Rejected, true)]);
        // A reading after it keeps the wrap-up.
        let again = parse(&mut parser, rejected());
        assert_eq!(readings(&again), [(QuotaStatus::Rejected, true)]);
        let end = parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": false}),
        );
        assert_eq!(usage_limit_errors(&end), 0);
    }

    /// An empty result with no model call answers your message only when none of yours waits
    /// in the queue. One that does is a resume closing out the last process's task; one that
    /// doesn't, such as a background task's end told between turns, is a turn like any other.
    #[test]
    fn an_empty_result_is_left_over_only_while_your_message_waits() {
        let lifecycle = |uuid: &str, state: &str| json!({"type": "command_lifecycle", "command_uuid": uuid, "state": state});
        let empty = json!({"type": "result", "subtype": "success", "is_error": false,
                           "num_turns": 0, "result": ""});
        let results = |events: &[AgentEvent]| {
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::Result { .. }))
                .count()
        };
        let mut resumed = ClaudeStreamParser::new();
        parse(&mut resumed, lifecycle("u-1", "queued"));
        assert_eq!(results(&parse(&mut resumed, empty.clone())), 0);
        parse(&mut resumed, lifecycle("u-1", "started"));
        let answer = json!({"type": "result", "subtype": "success", "is_error": false,
                            "num_turns": 2, "result": "12 passed"});
        assert_eq!(results(&parse(&mut resumed, answer)), 1);

        let mut between_turns = ClaudeStreamParser::new();
        parse(&mut between_turns, lifecycle("u-1", "queued"));
        parse(&mut between_turns, lifecycle("u-1", "started"));
        parse(&mut between_turns, lifecycle("u-1", "completed"));
        assert_eq!(results(&parse(&mut between_turns, empty)), 1);
    }

    /// Only Claude Code's own notice says it's wrapping up. A mod that pushes the same words is
    /// shown as it is, and Claude Code stays in for new work.
    #[test]
    fn a_mods_push_with_the_wrap_up_words_is_no_wrap_up() {
        let text = "Usage limit reached · wrapping up · clau.de/wrap-up";
        for line in [
            json!({"type": "system", "subtype": "ui_toast", "plugin": "jokes", "text": text}),
            json!({"type": "system", "subtype": "ui_log", "plugin": "jokes", "text": text}),
            json!({"type": "system", "subtype": "ui_status", "plugin": "jokes", "text": text}),
            json!({"type": "system", "subtype": "notification", "plugin": "jokes",
                   "text": text, "priority": "high"}),
        ] {
            let mut parser = ClaudeStreamParser::new();
            let events = parse(&mut parser, line.clone());
            assert_eq!(types(&events), ["mod_output"], "{line}");
        }
        let mut parser = ClaudeStreamParser::new();
        let own = parse(
            &mut parser,
            json!({"type": "system", "subtype": "informational", "level": "warning",
                   "content": text}),
        );
        assert_eq!(readings(&own), [(QuotaStatus::Rejected, true)]);
    }

    #[test]
    fn init_reads_the_version_plugins_and_capabilities() {
        let mut parser = ClaudeStreamParser::new();
        let events = parse(
            &mut parser,
            json!({"type": "system", "subtype": "init", "session_id": "s", "model": "haiku",
                   "tools": ["Read"], "claude_code_version": "2.1.289",
                   "capabilities": ["interrupt_send_now_v1", "ui_surface_v1"],
                   "plugins": [{"name": "cc-plugin-agents-md", "path": "builtin"}, "old-style"]}),
        );
        let [
            AgentEvent::Init {
                plugins,
                capabilities,
                version,
                ..
            },
        ] = events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert_eq!(plugins, &["cc-plugin-agents-md", "old-style"]);
        assert_eq!(capabilities, &["interrupt_send_now_v1", "ui_surface_v1"]);
        assert_eq!(version.as_deref(), Some("2.1.289"));
    }

    #[test]
    fn screen_pushes_and_bookkeeping_are_read() {
        let mut parser = ClaudeStreamParser::new();
        for (line, kind) in [
            (
                json!({"type": "command_lifecycle", "command_uuid": "u", "state": "queued"}),
                "message_state",
            ),
            (
                json!({"type": "system", "subtype": "ui_invalidate", "event": "ui.render"}),
                "screen_changed",
            ),
            (
                json!({"type": "system", "subtype": "commands_changed", "commands": []}),
                "commands_changed",
            ),
        ] {
            assert_eq!(types(&parse(&mut parser, line)), [kind]);
        }
        for quiet in [
            json!({"type": "system", "subtype": "ui_invalidate", "event": "ui.other"}),
            json!({"type": "system", "subtype": "turn_preempted", "reason": "rapid_followup"}),
            json!({"type": "system", "subtype": "plugin_install", "status": "started"}),
            json!({"type": "control_response", "response": {"subtype": "success",
                   "request_id": "r", "response": {"status": "delivering"}}}),
        ] {
            assert!(parse(&mut parser, quiet.clone()).is_empty(), "{quiet}");
        }
        let panes = json!({"type": "system", "subtype": "ui_panes"});
        assert_eq!(types(&parse(&mut parser, panes.clone())), ["mod_output"]);
        assert!(parse(&mut parser, panes).is_empty());
        let toast = parse(
            &mut parser,
            json!({"type": "system", "subtype": "ui_toast", "plugin": "p", "text": "hi"}),
        );
        assert_eq!(types(&toast), ["mod_output"]);
    }

    #[test]
    fn subagent_messages_update_the_tree() {
        let mut parser = ClaudeStreamParser::new();
        let spawn = parse(
            &mut parser,
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": "m1",
                   "content": [{"type": "tool_use", "id": "toolu_a", "name": "Agent",
                                "input": {"description": "Read it", "prompt": "Read README.md",
                                          "subagent_type": "general-purpose"}}]}}),
        );
        assert_eq!(types(&spawn), ["tool_started", "subagent"]);
        let prompt = parse(
            &mut parser,
            json!({"type": "user", "parent_tool_use_id": "toolu_a",
                   "message": {"role": "user", "content": [{"type": "text", "text": "Read README.md"}]}}),
        );
        let [AgentEvent::Subagent { update }] = prompt.as_slice() else {
            panic!("{prompt:?}");
        };
        assert_eq!(update.status, SubagentStatus::Running);
        // A subagent's usage counts toward spend but not the session's context.
        let step = parse(
            &mut parser,
            json!({"type": "assistant", "parent_tool_use_id": "toolu_a", "message": {"id": "m2",
                   "content": [{"type": "tool_use", "id": "toolu_b", "name": "Read",
                                "input": {"file_path": "README.md"}}],
                   "usage": {"input_tokens": 50, "output_tokens": 2}}}),
        );
        assert_eq!(types(&step), ["tool_started", "usage"]);
        assert!(
            matches!(&step[0], AgentEvent::ToolStarted { parent: Some(p), .. } if p == "toolu_a")
        );
        let done = parse(
            &mut parser,
            json!({"type": "user", "parent_tool_use_id": null, "message": {"content": [
                {"type": "tool_result", "tool_use_id": "toolu_a", "content": [{"type": "text", "text": "# Title"}]}]}}),
        );
        assert_eq!(types(&done), ["tool_finished", "subagent"]);
        let AgentEvent::Subagent { update } = &done[1] else {
            unreachable!()
        };
        assert_eq!(update.status, SubagentStatus::Done);
        assert_eq!(update.last_message.as_deref(), Some("# Title"));
    }

    #[test]
    fn each_turn_reports_its_own_result() {
        let mut parser = ClaudeStreamParser::new();
        let failed = json!({"type": "result", "subtype": "error_during_execution", "is_error": true,
                            "errors": ["Prompt is too long"]});
        assert_eq!(
            types(&parse(&mut parser, failed.clone())),
            ["error", "result"]
        );
        parse(
            &mut parser,
            json!({"type": "assistant", "message": {"id": "m9", "content": [{"type": "text", "text": "again"}]}}),
        );
        assert_eq!(types(&parse(&mut parser, failed)), ["error", "result"]);
        // Every turn ended, so a clean exit adds nothing.
        assert!(parser.finish(Some(0)).is_empty());
        assert!(parser.finish(Some(0)).is_empty());

        // A second turn cut short still gets a result.
        let mut parser = ClaudeStreamParser::new();
        parse(
            &mut parser,
            json!({"type": "result", "subtype": "success", "is_error": false, "result": "one"}),
        );
        parse(
            &mut parser,
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m2"}}}),
        );
        assert_eq!(types(&parser.finish(Some(130))), ["error", "result"]);
    }

    #[test]
    fn a_non_zero_exit_after_an_unexplained_error_result_is_a_crash() {
        let unexplained = json!({"type": "result", "subtype": "error_during_execution",
                                 "is_error": true, "errors": ["something odd"]});
        let mut parser = ClaudeStreamParser::new();
        parse(&mut parser, unexplained.clone());
        assert_eq!(
            parser.finish(Some(1)),
            [AgentEvent::Error {
                category: ErrorCategory::Unknown,
                message: "claude exited with code 1 after an error it didn't explain".into(),
                retryable: false
            }]
        );
        for exit in [Some(0), Some(130), None] {
            let mut parser = ClaudeStreamParser::new();
            parse(&mut parser, unexplained.clone());
            assert!(parser.finish(exit).is_empty(), "{exit:?}");
        }
        let mut budget = ClaudeStreamParser::new();
        parse(
            &mut budget,
            json!({"type": "result", "subtype": "error_max_budget_usd", "is_error": true}),
        );
        assert!(budget.finish(Some(1)).is_empty());
    }

    /// A flagged rm turned down without asking: Claude Code's message, hint and all, goes on
    /// the call's own row instead of an error line.
    #[test]
    fn a_denial_goes_whole_on_its_calls_row() {
        let mut parser = ClaudeStreamParser::new();
        let hint = "if BUILD_DIR were empty, this would remove /out. Use \"${BUILD_DIR:?}/out\" \
                    so the shell stops instead, or a literal path.";
        parse(
            &mut parser,
            json!({"type": "assistant", "message": {"id": "m1", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash",
                 "input": {"command": "rm -rf \"$BUILD_DIR/out\""}}]}}),
        );
        let denied = parse(
            &mut parser,
            json!({"type": "system", "subtype": "permission_denied", "tool_name": "Bash",
                   "tool_use_id": "t1", "decision_reason_type": "mode", "message": hint}),
        );
        assert_eq!(denied, []);
        let finished = parse(
            &mut parser,
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "is_error": true,
                 "content": hint}]}}),
        );
        assert_eq!(
            finished,
            [AgentEvent::ToolFinished {
                id: "t1".into(),
                ok: false,
                output: Some(format!("Claude Code turned this down: {hint}")),
                parent: None,
            }]
        );
    }

    #[test]
    fn only_an_isolated_session_reports_your_plugins_as_a_leak() {
        let init = json!({"type": "system", "subtype": "init", "session_id": "s1",
            "plugins": [
                {"name": "memory-keeper", "path": "~/p", "source": "memory-keeper@market"},
                {"name": "cc-plugin-telemetry", "path": "builtin", "source": "cc-plugin-telemetry@builtin"}
            ],
            "skills": ["debug", "anthropic-skills:example-a"]});
        let leaks = |parser: &mut ClaudeStreamParser| -> Vec<AgentEvent> {
            parse(parser, init.clone())
                .into_iter()
                .filter(|event| matches!(event, AgentEvent::IsolationLeak { .. }))
                .collect()
        };
        assert_eq!(leaks(&mut ClaudeStreamParser::new()), []);
        let mut isolated = ClaudeStreamParser::isolated();
        assert_eq!(
            leaks(&mut isolated),
            [AgentEvent::IsolationLeak {
                plugins: vec!["memory-keeper".into()],
                skills: vec!["anthropic-skills:example-a".into()],
                builtins: Vec::new(),
            }]
        );
        // Each turn's init repeats the list. It was said once.
        assert_eq!(leaks(&mut isolated), []);
    }
}
