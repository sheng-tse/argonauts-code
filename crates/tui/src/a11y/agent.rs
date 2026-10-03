//! What one agent event says: its messages, each tool call and what came of it, errors, and
//! retries. Streamed text is left to [`super::Reader`], which speaks it a line at a time.

use argonauts_core::steps::summary_line;
use argonauts_core::{AgentEvent, ErrorCategory, TodoItem, Verbosity};
use serde_json::Value;

use crate::bridge::tools::{finish_call, is_own, start_call};
use crate::cell::{ToolItem, ToolTitle};
use crate::state::AppState;
use crate::text::sanitize_line;

use super::words::{duration, first_lines, labeled, more_lines, shows, tokens, word};

/// Output lines a tool result shows before `and N more lines`.
const OUTPUT_LINES: usize = 3;

/// A tool call as it started, for reading its result.
pub struct StartedCall<'a> {
    pub name: &'a str,
    pub input: &'a Value,
}

/// Lines for `event` from `who`. `vendor` is the adapter id, which names its tools. `said` is
/// the last whole message already spoken, so a result that repeats it stays quiet. Argonauts'
/// own calls, such as the report, say nothing.
pub fn agent_lines(
    who: &str,
    vendor: &str,
    event: &AgentEvent,
    state: &AppState,
    call: Option<StartedCall<'_>>,
    said: Option<&str>,
) -> Vec<String> {
    let normal = shows(state, Verbosity::Normal);
    let full = shows(state, Verbosity::Full);
    match event {
        AgentEvent::Init { model, .. } if full => {
            let model = model.as_deref().map_or("unknown".into(), sanitize_line);
            vec![format!("{who}: session started, model {model}")]
        }
        AgentEvent::Message { text, parent: None } if normal => labeled(who, text),
        AgentEvent::Message {
            text,
            parent: Some(_),
        } if full => labeled(&format!("{who} subagent"), text),
        AgentEvent::Reasoning { text } if full => labeled(&format!("{who} thinking"), text),
        AgentEvent::ToolStarted {
            name,
            input,
            parent,
            ..
        } if normal && (parent.is_none() || full) && !is_own(name) => {
            let call = start_call(vendor, name, input);
            let title = match &call.title {
                ToolTitle::Call { name, args } if args.is_empty() => name.clone(),
                ToolTitle::Call { name, args } => format!("{name} {args}"),
                other => other.plain(),
            };
            vec![format!("{who}: {}", sanitize_line(&title).trim())]
        }
        AgentEvent::ToolFinished {
            ok, output, parent, ..
        } if (parent.is_none() || full) && !call.as_ref().is_some_and(|call| is_own(call.name)) => {
            if *ok && !normal {
                return Vec::new();
            }
            tool_result(vendor, call, *ok, output.as_deref())
        }
        AgentEvent::FileChanged { path, kind } if full => vec![format!(
            "{who}: {} {}",
            sanitize_line(kind).trim(),
            sanitize_line(path).trim()
        )],
        AgentEvent::Todo { items } if normal => vec![format!("{who}: to-do, {}", todo(items))],
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            ..
        } if full => vec![format!(
            "{who}: {} in, {} out",
            tokens(*input_tokens),
            tokens(*output_tokens)
        )],
        AgentEvent::Retry { reason, delay_ms } if normal => {
            let delay = delay_ms
                .map(|ms| format!(", in {}", duration(ms)))
                .unwrap_or_default();
            vec![format!("{who}: retrying, {}{delay}", word(reason))]
        }
        AgentEvent::Error {
            category: ErrorCategory::Stalled,
            message,
            ..
        } => vec![format!("{who}: {}", sanitize_line(message).trim())],
        AgentEvent::Error { message, .. } => {
            vec![format!("{who} error: {}", sanitize_line(message).trim())]
        }
        AgentEvent::Result {
            text,
            structured,
            is_error,
        } => {
            let mut lines = Vec::new();
            // A structured result reads as its summary, never as JSON.
            let text = match structured {
                Some(report) => summary_line(report),
                None => text.clone(),
            };
            if normal && let Some(text) = text.as_deref() {
                let repeated = said.is_some_and(|said| said.trim() == text.trim());
                if !repeated {
                    lines.extend(labeled(who, text));
                }
            }
            if *is_error {
                lines.push(format!("{who} error: finished with an error"));
            }
            lines
        }
        AgentEvent::Subagent { update } if full => {
            vec![format!("{who}: subagent {}", word(&update.status))]
        }
        AgentEvent::Unknown { .. } if full => vec![format!("{who}: an event no parser knows")],
        _ => Vec::new(),
    }
}

/// `tool: Read 42 lines`, or `tool error: cat: missing.txt: No such file or directory`.
fn tool_result(
    vendor: &str,
    call: Option<StartedCall<'_>>,
    ok: bool,
    output: Option<&str>,
) -> Vec<String> {
    let label = if ok { "tool" } else { "tool error" };
    let said = match call {
        Some(call) => {
            let mut drawn = start_call(vendor, call.name, call.input);
            finish_call(&mut drawn, ok, output);
            let mut lines: Vec<String> = drawn.body.iter().flat_map(item_lines).collect();
            let changed = drawn
                .body
                .iter()
                .any(|item| matches!(item, ToolItem::Diff(_)));
            if lines.is_empty() {
                lines = output_lines(output.unwrap_or_default());
            }
            if lines.is_empty() && changed {
                lines.push("changed the file".to_owned());
            }
            lines
        }
        None => Vec::new(),
    };
    let said = if said.is_empty() {
        output_lines(output.unwrap_or_default())
    } else {
        said
    };
    if said.is_empty() {
        let done = if ok { "done" } else { "failed" };
        return vec![format!("{label}: {done}")];
    }
    said.into_iter()
        .map(|line| format!("{label}: {line}"))
        .collect()
}

fn item_lines(item: &ToolItem) -> Vec<String> {
    match item {
        ToolItem::Text { text, .. } | ToolItem::Markdown(text) => output_lines(text),
        ToolItem::Command { output, hidden, .. } => {
            let mut lines = output_lines(&output.join("\n"));
            if *hidden > 0 && !lines.is_empty() {
                lines.push(more_lines(*hidden));
            }
            lines
        }
        _ => Vec::new(),
    }
}

fn output_lines(text: &str) -> Vec<String> {
    let (mut lines, more) = first_lines(text, OUTPUT_LINES);
    if more > 0 {
        lines.push(more_lines(more));
    }
    lines
}

fn todo(items: &[TodoItem]) -> String {
    items
        .iter()
        .map(|item| {
            format!(
                "{} ({})",
                sanitize_line(&item.text).trim(),
                word(&item.status)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_structured_result_reads_as_its_summary() {
        let report = json!({"status": "done", "summary": "Created hello.txt", "files_changed": []});
        let result = AgentEvent::Result {
            text: Some(report.to_string()),
            structured: Some(report),
            is_error: false,
        };
        let lines = agent_lines("coder", "codex", &result, &AppState::default(), None, None);
        assert_eq!(lines, ["coder: Created hello.txt"]);

        let plain = AgentEvent::Result {
            text: Some("All done.".to_owned()),
            structured: None,
            is_error: false,
        };
        let lines = agent_lines("coder", "codex", &plain, &AppState::default(), None, None);
        assert_eq!(lines, ["coder: All done."]);
    }
}
