//! What one agent event says: its messages, each tool call and what came of it, errors, and
//! retries. Streamed text is left to [`super::Reader`], which speaks it a line at a time.

use argonauts_core::workflows::steps::summary_line;
use argonauts_core::{AgentEvent, ErrorCategory, TodoItem, Verbosity};
use serde_json::Value;

use crate::base::text::sanitize_line;
use crate::bridge::tools::{finish_call, is_own, is_tool_lookup, start_call};
use crate::bridge::transcript::own_calls::{Own, turned_down};
use crate::cells::cell::{PhaseLine, ToolItem, ToolTitle, WorkflowEnd, WorkflowResult};
use crate::frame::state::AppState;

use super::words::{duration, first_lines, labeled, more_lines, shows, tokens, word};

/// Output lines a tool result shows before `and N more lines`.
const OUTPUT_LINES: usize = 3;

/// A tool call as it started, for reading its result.
pub struct StartedCall<'a> {
    pub name: &'a str,
    pub input: &'a Value,
}

/// For the answer to a call of `name`: whether Argonauts Code turned down the workflow it
/// asked to start, and `None` for any other call.
pub(super) fn start_turned_down(name: &str, ok: bool, output: Option<&str>) -> Option<bool> {
    (Own::named(name) == Some(Own::StartWorkflow)).then(|| turned_down(ok, output).is_some())
}

/// Lines for `event` from `who`. `agent` is the adapter id, which names its tools. `said` is
/// the last whole message already spoken, so a result that repeats it stays quiet. Argonauts Code's
/// own calls, such as the report, say nothing.
pub fn agent_lines(
    who: &str,
    agent_id: &str,
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
        } if normal && (parent.is_none() || full) && !is_own(name) && !is_tool_lookup(name) => {
            // What the session asks of Argonauts Code reads as its plain line, as on screen.
            let own = Own::named(name);
            if own == Some(Own::StartWorkflow) {
                return Vec::new();
            }
            let call = own
                .and_then(|own| own.line(input))
                .unwrap_or_else(|| start_call(agent_id, name, input));
            let title = match &call.title {
                ToolTitle::Call { name, args } if args.is_empty() => name.clone(),
                ToolTitle::Call { name, args } => format!("{name} {args}"),
                other => other.plain(),
            };
            vec![format!("{who}: {}", sanitize_line(&title).trim())]
        }
        AgentEvent::ToolFinished {
            ok, output, parent, ..
        } if (parent.is_none() || full)
            && !call
                .as_ref()
                .is_some_and(|call| is_own(call.name) || is_tool_lookup(call.name)) =>
        {
            if *ok && !normal {
                return Vec::new();
            }
            // An answer to one of the session's own calls is the session's to read; only a
            // start Argonauts Code turned down is said.
            if let Some(own) = call.as_ref().and_then(|call| Own::named(call.name)) {
                return match own {
                    Own::StartWorkflow => turned_down(*ok, output.as_deref())
                        .map(|line| format!("{who}: {}", line.title.plain()))
                        .into_iter()
                        .collect(),
                    _ => Vec::new(),
                };
            }
            tool_result(agent_id, call, *ok, output.as_deref())
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
        // Its turn was stopped, by you or to hand it a message. Nothing failed.
        AgentEvent::Error {
            category: ErrorCategory::Interrupted,
            ..
        } => vec![format!("{who}: stopped")],
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
    agent_id: &str,
    call: Option<StartedCall<'_>>,
    ok: bool,
    output: Option<&str>,
) -> Vec<String> {
    let refused = !ok && output.is_some_and(crate::bridge::refusal::is_refusal);
    let label = match (ok, refused) {
        (true, _) => "tool",
        (false, true) => "tool not allowed",
        (false, false) => "tool error",
    };
    let said = match call {
        Some(call) => {
            let mut drawn = start_call(agent_id, call.name, call.input);
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
        ToolItem::Phases(phases) => phases.iter().enumerate().map(phase_line).collect(),
        ToolItem::PhasesBeside { phases, beside } => {
            let mut lines: Vec<String> = phases.iter().enumerate().map(phase_line).collect();
            lines.push(sanitize_line(beside).trim().to_owned());
            lines
        }
        ToolItem::Command { output, hidden, .. } => {
            let mut lines = output_lines(&output.join("\n"));
            if *hidden > 0 && !lines.is_empty() {
                lines.push(more_lines(*hidden));
            }
            lines
        }
        ToolItem::Workflow(WorkflowResult::Running) => {
            vec!["Running in background. /workflows opens it.".to_owned()]
        }
        ToolItem::Workflow(WorkflowResult::Ended { how, counts }) => {
            let word = match how {
                WorkflowEnd::Completed => "Completed",
                WorkflowEnd::Failed => "Failed",
                WorkflowEnd::Stopped => "Stopped",
            };
            vec![format!("{word} in {}", sanitize_line(counts).trim())]
        }
        _ => Vec::new(),
    }
}

/// `2. Secrets scan, a step you require: the reviewers scan the change`.
fn phase_line((i, phase): (usize, &PhaseLine)) -> String {
    let yours = if phase.required {
        ", a step you require"
    } else {
        ""
    };
    let said = format!("{}. {}{yours}", i + 1, sanitize_line(&phase.name).trim());
    match sanitize_line(&phase.detail).trim() {
        "" => said,
        detail => format!("{said}: {detail}"),
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

#[cfg(test)]
mod own_call_tests {
    use serde_json::json;

    use super::*;

    fn started(name: &str, input: Value) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: "c1".into(),
            name: name.into(),
            input,
            parent: None,
        }
    }

    /// The session's own calls are said as the screen draws them, and a tool lookup not at all.
    #[test]
    fn own_calls_are_said_plainly_and_tool_lookups_not_at_all() {
        let state = AppState::default();
        let lookup = "mcp__argonauts_status__argonauts_lookup";
        let said = agent_lines(
            "Claude Code",
            "claude-code",
            &started(lookup, json!({"what": "reviews"})),
            &state,
            None,
            None,
        );
        assert_eq!(
            said,
            ["Claude Code: Looked up reviews in Argonauts Code's records"]
        );
        let input = json!({"what": "reviews"});
        let answer = AgentEvent::ToolFinished {
            id: "c1".into(),
            ok: true,
            output: Some("{\"reviews\": []}".into()),
            parent: None,
        };
        let call = StartedCall {
            name: lookup,
            input: &input,
        };
        let said = agent_lines(
            "Claude Code",
            "claude-code",
            &answer,
            &state,
            Some(call),
            None,
        );
        assert!(said.is_empty(), "{said:?}");
        for quiet in [
            started("ToolSearch", json!({"query": "select:Read"})),
            started(
                "mcp__argonauts_status__argonauts_start_workflow",
                json!({"request": "x"}),
            ),
        ] {
            let said = agent_lines("Claude Code", "claude-code", &quiet, &state, None, None);
            assert!(said.is_empty(), "{said:?}");
        }
    }
}
