use std::collections::HashMap;

use argonauts_core::{AgentEvent, ErrorCategory, StreamParser};
use serde_json::Value;

use crate::errors;
use crate::json::{array_at, bool_at, str_at, string_at, strings_at};
use crate::todo::TodoTracker;
use crate::usage::UsageTracker;

const FILE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// Text streams as [`AgentEvent::TextDelta`] and then arrives whole in [`AgentEvent::Message`],
/// while usage comes as increments until the result reports cumulative totals with cost.
#[derive(Debug, Default)]
pub struct ClaudeStreamParser {
    cwd: Option<String>,
    usage: UsageTracker,
    todos: TodoTracker,
    open_tools: HashMap<String, OpenTool>,
    streaming_message_id: Option<String>,
    last_text: Option<String>,
    result_reported: bool,
    error_reported: bool,
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

    /// `None` when the line isn't one this parser knows.
    fn on_line(&mut self, line: &Value) -> Option<Vec<AgentEvent>> {
        match str_at(line, "type")? {
            "system" => self.on_system(line),
            "stream_event" => self.on_stream_event(line),
            "assistant" => self.on_assistant(line),
            "user" => Some(self.on_user(line)),
            "result" => Some(self.on_result(line)),
            "rate_limit_event" => Some(vec![AgentEvent::QuotaSignal {
                detail: line
                    .get("rate_limit_info")
                    .cloned()
                    .unwrap_or_else(|| line.clone()),
            }]),
            _ => None,
        }
    }

    fn on_system(&mut self, line: &Value) -> Option<Vec<AgentEvent>> {
        let event = match str_at(line, "subtype")? {
            "init" => {
                self.cwd = string_at(line, "cwd");
                AgentEvent::Init {
                    session_id: string_at(line, "session_id"),
                    model: string_at(line, "model"),
                    tools: strings_at(line, "tools"),
                }
            }
            "api_retry" => AgentEvent::Retry {
                reason: errors::retry_reason(line),
                delay_ms: line.get("retry_delay_ms").and_then(Value::as_u64),
            },
            "permission_denied" => {
                let tool = str_at(line, "tool_name").unwrap_or("A tool");
                let reason = str_at(line, "message")
                    .or_else(|| str_at(line, "decision_reason"))
                    .unwrap_or("permission denied");
                AgentEvent::Error {
                    category: ErrorCategory::Permission,
                    message: format!("{tool} was denied: {reason}"),
                    retryable: false,
                }
            }
            // Transient hints about a request in flight or a running estimate of thinking tokens.
            "status" | "thinking_tokens" => return Some(Vec::new()),
            _ => return None,
        };
        Some(vec![event])
    }

    fn on_stream_event(&mut self, line: &Value) -> Option<Vec<AgentEvent>> {
        let event = line.get("event")?;
        match str_at(event, "type")? {
            "message_start" => {
                let message = event.get("message")?;
                self.streaming_message_id = string_at(message, "id");
                Some(self.observe_usage(str_at(message, "id"), message.get("usage")))
            }
            "message_delta" => {
                let id = self.streaming_message_id.clone();
                Some(self.observe_usage(id.as_deref(), event.get("usage")))
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
                    events.push(self.start_tool(block, parent.clone()));
                }
                _ => events.push(AgentEvent::Unknown { raw: block.clone() }),
            }
        }
        if let Some(code) = str_at(line, "error") {
            events.push(self.api_error(code, &text_parts));
        }
        events.extend(self.observe_usage(str_at(message, "id"), message.get("usage")));
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
        // `tool_use_result` belongs to the message, so it is only unambiguous for a single result.
        let structured = match results.as_slice() {
            [_] => line.get("tool_use_result"),
            _ => None,
        };
        results
            .into_iter()
            .flat_map(|block| self.finish_tool(block, structured, parent.clone()))
            .collect()
    }

    fn on_result(&mut self, line: &Value) -> Vec<AgentEvent> {
        self.result_reported = true;
        let mut events: Vec<AgentEvent> = self.usage.result_totals(line).into_iter().collect();
        let subtype = str_at(line, "subtype").unwrap_or("success");
        let is_error = bool_at(line, "is_error") || subtype != "success";
        let text = string_at(line, "result").filter(|t| !t.is_empty());
        if is_error && !self.error_reported {
            let message = result_error_message(line, subtype, text.as_deref());
            let category = errors::from_result(line, &message);
            events.push(self.error(category, message));
        }
        events.push(AgentEvent::Result {
            text,
            structured: line
                .get("structured_output")
                .filter(|s| !s.is_null())
                .cloned(),
            is_error,
        });
        events
    }

    fn start_tool(&mut self, block: &Value, parent: Option<String>) -> AgentEvent {
        let id = string_at(block, "id").unwrap_or_default();
        let name = string_at(block, "name").unwrap_or_default();
        let input = block.get("input").cloned().unwrap_or(Value::Null);
        self.open_tools.insert(
            id.clone(),
            OpenTool {
                name: name.clone(),
                input: input.clone(),
            },
        );
        AgentEvent::ToolStarted {
            id,
            name,
            input,
            parent,
        }
    }

    fn finish_tool(
        &mut self,
        block: &Value,
        structured: Option<&Value>,
        parent: Option<String>,
    ) -> Vec<AgentEvent> {
        let id = string_at(block, "tool_use_id").unwrap_or_default();
        let ok = !bool_at(block, "is_error");
        let mut events = vec![AgentEvent::ToolFinished {
            id: id.clone(),
            ok,
            output: tool_output_text(block.get("content")),
            parent,
        }];
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

    fn observe_usage(&mut self, id: Option<&str>, usage: Option<&Value>) -> Vec<AgentEvent> {
        match (id, usage) {
            (Some(id), Some(usage)) => self.usage.observe(id, usage).into_iter().collect(),
            _ => Vec::new(),
        }
    }

    fn error(&mut self, category: ErrorCategory, message: String) -> AgentEvent {
        self.error_reported = true;
        AgentEvent::Error {
            category,
            message,
            retryable: errors::retryable(category),
        }
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

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        if line.to_lowercase().starts_with("error") {
            let category = errors::from_text(line).unwrap_or(ErrorCategory::Cli);
            return vec![self.error(category, line.to_owned())];
        }
        vec![AgentEvent::Unknown {
            raw: Value::String(line.to_owned()),
        }]
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        if self.result_reported {
            return Vec::new();
        }
        self.result_reported = true;
        let mut events = Vec::new();
        if !self.error_reported {
            let (category, message) = missing_result_error(exit_code);
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

fn missing_result_error(exit_code: Option<i32>) -> (ErrorCategory, String) {
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
        Some(code) => (
            ErrorCategory::Cli,
            format!("claude exited with code {code} before it reported a result"),
        ),
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
        assert_eq!(types(&start), ["usage"]);
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
        assert_eq!(
            parse(
                &mut parser,
                json!({"type": "rate_limit_event", "rate_limit_info": info.clone()})
            ),
            [AgentEvent::QuotaSignal { detail: info }]
        );
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
    }

    #[test]
    fn stderr_errors_are_categorized_and_other_lines_kept() {
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
        assert_eq!(
            parser.parse_stderr_line("Warning: something"),
            [AgentEvent::Unknown {
                raw: json!("Warning: something")
            }]
        );
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
    }
}
