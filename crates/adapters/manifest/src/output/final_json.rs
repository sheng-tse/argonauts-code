//! `final-json`: stdout kept whole, then read at exit through the manifest's JSON pointers
//! into `Init`, `Usage`, `Result`, and `Error`.

use argonauts_core::registry::HarnessManifest;
use argonauts_core::registry::harness::FinalJsonPointers;
use argonauts_core::{AgentEvent, ErrorCategory, StreamParser};
use serde_json::Value;

use super::Kept;

pub(super) struct FinalJson {
    id: String,
    pointers: FinalJsonPointers,
    /// `schema.field`, where the CLI puts the output its schema enforced.
    structured_field: Option<String>,
    stdout: Kept,
}

impl FinalJson {
    pub(super) fn new(manifest: &HarnessManifest) -> Self {
        Self {
            id: manifest.id.clone(),
            pointers: manifest.final_json.clone().unwrap_or_default(),
            structured_field: manifest.schema.as_ref().map(|schema| schema.field.clone()),
            stdout: Kept::default(),
        }
    }

    /// The whole of stdout, or else its last line that is a JSON object, for a CLI that logs
    /// a little before its document.
    fn document(&self) -> Option<Value> {
        let text = self.stdout.text().trim();
        serde_json::from_str(text).ok().or_else(|| {
            text.lines()
                .rev()
                .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
                .find(Value::is_object)
        })
    }

    fn read(&self, doc: &Value, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let p = &self.pointers;
        let at = |pointer: &Option<String>| pointer.as_deref().and_then(|at| doc.pointer(at));
        let text_at = |pointer: &Option<String>| at(pointer).and_then(as_text);
        let mut events = Vec::new();

        let session_id = text_at(&p.session_id);
        let model = text_at(&p.model);
        if session_id.is_some() || model.is_some() {
            events.push(AgentEvent::Init {
                session_id,
                model,
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            });
        }

        let input = at(&p.input_tokens).and_then(as_count);
        let output = at(&p.output_tokens).and_then(as_count);
        let cached = at(&p.cached_tokens).and_then(as_count);
        let cost_usd = at(&p.cost_usd).and_then(as_number);
        if input.is_some() || output.is_some() || cached.is_some() || cost_usd.is_some() {
            events.push(AgentEvent::Usage {
                input_tokens: input.unwrap_or(0),
                output_tokens: output.unwrap_or(0),
                cached_tokens: cached.unwrap_or(0),
                cost_usd,
                cumulative: true,
            });
        }

        let status = text_at(&p.status);
        let status_ok = match &status {
            Some(status) => p.ok_status.is_empty() || p.ok_status.contains(status),
            None => true,
        };
        let kind = text_at(&p.error_kind);
        let message = text_at(&p.error_message);
        if !status_ok || kind.is_some() || message.is_some() {
            let category = kind
                .as_deref()
                .and_then(category_named)
                .unwrap_or(ErrorCategory::Cli);
            let message = message
                .or_else(|| kind.clone())
                .or_else(|| {
                    status.map(|status| format!("{} finished with status {status}", self.id))
                })
                .unwrap_or_else(|| format!("{} reported an error", self.id));
            events.push(AgentEvent::Error {
                category,
                message,
                retryable: false,
            });
        }

        let failed = events
            .iter()
            .any(|event| matches!(event, AgentEvent::Error { .. }));
        events.push(AgentEvent::Result {
            text: text_at(&p.text),
            structured: self.structured(doc),
            is_error: failed || exit_code != Some(0),
        });
        events
    }

    /// A JSON pointer, or else a top-level field name such as `structured_output`.
    fn structured(&self, doc: &Value) -> Option<Value> {
        let field = self.structured_field.as_deref()?;
        let value = if field.starts_with('/') {
            doc.pointer(field)
        } else {
            doc.get(field)
        };
        value.filter(|value| !value.is_null()).cloned()
    }
}

impl StreamParser for FinalJson {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        self.stdout.push_line(line);
        Vec::new()
    }

    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        if self.stdout.overflowed() {
            return vec![super::overflow_error(&self.id), failed_result()];
        }
        match self.document() {
            Some(doc) => self.read(&doc, exit_code),
            // A CLI stopped early prints nothing, and the exit checks say why.
            None if self.stdout.text().trim().is_empty() => Vec::new(),
            None => vec![
                AgentEvent::Error {
                    category: ErrorCategory::Cli,
                    message: format!("{} didn't print a JSON document", self.id),
                    retryable: false,
                },
                failed_result(),
            ],
        }
    }
}

fn failed_result() -> AgentEvent {
    AgentEvent::Result {
        text: None,
        structured: None,
        is_error: true,
    }
}

/// Strings as they are, numbers and booleans as JSON text. Null and empty strings are absent.
fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) if text.trim().is_empty() => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

fn as_count(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|n| *n >= 0.0)
                .map(|n| n.round() as u64)
        }),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// An `error_kind` that names one of Argonauts' categories, such as `timeout` or `rate-limit`.
fn category_named(kind: &str) -> Option<ErrorCategory> {
    let name = kind.trim().to_ascii_lowercase().replace('-', "_");
    serde_json::from_value(Value::String(name)).ok()
}
