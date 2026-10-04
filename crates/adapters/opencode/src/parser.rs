//! `--format json` events to agent events. Anything it doesn't know stays as `Unknown`.
//!
//! OpenCode 1.18.33 writes one `{type, timestamp, sessionID, ...}` object per line: `step_start`,
//! `step_finish`, `text`, `reasoning` (with `--thinking`), `tool_use` (once a tool completes or
//! fails), and `error`. It sends no final result event and names no model, so `finish` builds
//! the result from the last text.

use argonauts_core::{AgentEvent, ErrorCategory, StreamParser};
use serde_json::Value;

#[derive(Debug, Default)]
pub(crate) struct OpenCodeParser {
    started: bool,
    last_text: Option<String>,
    failed: bool,
    /// The last `Error: ...` line on stderr, which is all 1.18.34 says when it stops before a
    /// session starts, such as for a session id it doesn't know.
    cli_error: Option<String>,
}

impl StreamParser for OpenCodeParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return vec![AgentEvent::unknown_line(line)];
        };
        let Some(kind) = event["type"].as_str() else {
            return vec![AgentEvent::unknown_line(line)];
        };
        let mut out = Vec::new();
        if !self.started
            && let Some(session) = event["sessionID"].as_str()
        {
            self.started = true;
            out.push(AgentEvent::Init {
                session_id: Some(session.to_owned()),
                model: None,
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            });
        }
        let part = &event["part"];
        let parsed = match kind {
            "step_start" => Some(Vec::new()),
            "step_finish" => step_finish(part),
            "text" => part["text"].as_str().map(|text| {
                self.last_text = Some(text.to_owned());
                vec![AgentEvent::Message {
                    text: text.to_owned(),
                    parent: None,
                }]
            }),
            "reasoning" => part["text"].as_str().map(|text| {
                vec![AgentEvent::Reasoning {
                    text: text.to_owned(),
                }]
            }),
            "tool_use" => tool_use(part),
            "error" => {
                self.failed = true;
                Some(vec![error(&event["error"])])
            }
            _ => None,
        };
        match parsed {
            Some(events) => out.extend(events),
            None => out.push(AgentEvent::Unknown { raw: event }),
        }
        out
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        if let Some(message) = without_ansi(line).strip_prefix("Error: ") {
            self.cli_error = Some(message.trim().to_owned());
        }
        vec![AgentEvent::unknown_line(line)]
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let exited_ok = exit_code == Some(0);
        if !exited_ok && !self.failed {
            let mut message = match exit_code {
                Some(code) => format!("opencode exited with code {code}"),
                None => "opencode was stopped by a signal".to_owned(),
            };
            if let Some(why) = self.cli_error.take() {
                message = format!("{message}: {why}");
            }
            out.push(AgentEvent::Error {
                category: ErrorCategory::Cli,
                message,
                retryable: false,
            });
        }
        out.push(AgentEvent::Result {
            text: self.last_text.take(),
            structured: None,
            is_error: self.failed || !exited_ok,
        });
        out
    }
}

/// OpenCode stores `input` without cache reads and writes, and `output` without reasoning, so
/// both are added back for Argonauts' totals. Each step's numbers add to the last.
fn step_finish(part: &Value) -> Option<Vec<AgentEvent>> {
    let tokens = &part["tokens"];
    let count = |value: &Value| value.as_f64().map_or(0, |n| n.max(0.0) as u64);
    let input = tokens["input"].as_f64()?;
    let cache_read = count(&tokens["cache"]["read"]);
    let cache_write = count(&tokens["cache"]["write"]);
    let input_tokens = input.max(0.0) as u64 + cache_read + cache_write;
    let output_tokens = count(&tokens["output"]) + count(&tokens["reasoning"]);
    let used = tokens["total"]
        .as_f64()
        .map_or(input_tokens + output_tokens, |n| n.max(0.0) as u64);
    Some(vec![
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            cached_tokens: cache_read,
            cost_usd: part["cost"].as_f64(),
            cumulative: false,
        },
        AgentEvent::ContextFill { used, size: None },
    ])
}

/// OpenCode reports a tool once, when it has finished, so both events come together.
fn tool_use(part: &Value) -> Option<Vec<AgentEvent>> {
    let id = part["callID"].as_str().or(part["id"].as_str())?.to_owned();
    let name = part["tool"].as_str()?.to_owned();
    let state = &part["state"];
    let (ok, output) = match state["status"].as_str()? {
        "completed" => (true, state["output"].as_str()),
        "error" => (false, state["error"].as_str()),
        _ => return None,
    };
    Some(vec![
        AgentEvent::ToolStarted {
            id: id.clone(),
            name,
            input: state["input"].clone(),
            parent: None,
        },
        AgentEvent::ToolFinished {
            id,
            ok,
            output: output.map(str::to_owned),
            parent: None,
        },
    ])
}

/// OpenCode's named errors, `{name, data: {message, ...}}`.
fn error(error: &Value) -> AgentEvent {
    let name = error["name"].as_str().unwrap_or("UnknownError");
    let data = &error["data"];
    let message = data["message"]
        .as_str()
        .map_or_else(|| name.to_owned(), str::to_owned);
    let status = data["statusCode"].as_u64();
    let category = match name {
        "ProviderAuthError" => ErrorCategory::Auth,
        "ContextOverflowError" | "MessageOutputLengthError" => ErrorCategory::ContextFull,
        "MessageAbortedError" => ErrorCategory::Interrupted,
        "ContentFilterError" => ErrorCategory::InvalidRequest,
        "APIError" => status.map_or(ErrorCategory::Unknown, category_for_status),
        _ => ErrorCategory::Unknown,
    };
    AgentEvent::Error {
        category,
        message,
        retryable: data["isRetryable"].as_bool().unwrap_or(false),
    }
}

fn category_for_status(status: u64) -> ErrorCategory {
    match status {
        401 | 403 => ErrorCategory::Auth,
        402 => ErrorCategory::Billing,
        404 => ErrorCategory::ModelUnavailable,
        408 => ErrorCategory::Timeout,
        413 => ErrorCategory::ContextFull,
        429 => ErrorCategory::RateLimit,
        503 | 529 => ErrorCategory::Overloaded,
        400..=499 => ErrorCategory::InvalidRequest,
        500..=599 => ErrorCategory::Server,
        _ => ErrorCategory::Unknown,
    }
}

/// `line` without its color codes (`ESC [ ... letter`).
fn without_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_types_and_plain_lines_are_kept() {
        let mut parser = OpenCodeParser::default();
        let events = parser.parse_stdout_line(r#"{"type":"patch","sessionID":"ses_1"}"#);
        assert!(matches!(events[0], AgentEvent::Init { .. }));
        assert!(matches!(events[1], AgentEvent::Unknown { .. }));
        assert!(matches!(
            parser.parse_stdout_line("not json").as_slice(),
            [AgentEvent::Unknown { .. }]
        ));
        let finished = parser.finish(None);
        assert!(matches!(
            finished[0],
            AgentEvent::Error {
                category: ErrorCategory::Cli,
                ..
            }
        ));
        assert!(matches!(
            finished[1],
            AgentEvent::Result { is_error: true, .. }
        ));
    }

    #[test]
    fn api_errors_are_categorized_by_status() {
        let event = error(&serde_json::json!({
            "name": "APIError",
            "data": {"message": "slow down", "statusCode": 429, "isRetryable": true}
        }));
        assert_eq!(
            event,
            AgentEvent::Error {
                category: ErrorCategory::RateLimit,
                message: "slow down".to_owned(),
                retryable: true,
            }
        );
        assert_eq!(category_for_status(500), ErrorCategory::Server);
        assert_eq!(category_for_status(400), ErrorCategory::InvalidRequest);
    }
}
