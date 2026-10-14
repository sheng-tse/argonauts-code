//! What the main session asks of Argonauts Code, as the conversation draws it: a plain line in
//! words, such as `Looked up reviews`, never a tool's internal name. Starting a workflow shows as
//! the session's own line and the workflow's row, so its call draws nothing unless Argonauts Code
//! turned it down.

use serde_json::Value;

use crate::cell::{ToolCall, ToolKind};
use crate::text::sanitize_line;

/// One of the main session's own tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Own {
    StartWorkflow,
    Goal,
    Setup,
    Guide,
    Lookup,
}

impl Own {
    /// However the CLI prefixes Argonauts Code's server: Claude Code's
    /// `mcp__argonauts_status__argonauts_lookup`, Codex's `mcp:argonauts_status/argonauts_lookup`
    /// or OpenCode's `argonauts_status_argonauts_lookup`.
    pub(crate) fn named(name: &str) -> Option<Self> {
        let normal: String = name
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        [
            ("argonautsstartworkflow", Own::StartWorkflow),
            ("argonautssetgoal", Own::Goal),
            ("argonautssetup", Own::Setup),
            ("argonautsguide", Own::Guide),
            ("argonautslookup", Own::Lookup),
        ]
        .into_iter()
        .find_map(|(tool, own)| normal.ends_with(tool).then_some(own))
    }

    /// The call's line, or `None` for one that shows through what it starts.
    pub(crate) fn line(self, input: &Value) -> Option<ToolCall> {
        let field = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .map(|text| sanitize_line(text.trim()).into_owned())
                .filter(|text| !text.is_empty())
        };
        let text = match self {
            Own::StartWorkflow => return None,
            Own::Goal => match field("action").as_deref() {
                Some("show") => "Read the goal".to_owned(),
                Some("pause") => "Paused the goal".to_owned(),
                Some("resume") => "Resumed the goal".to_owned(),
                Some("clear") => "Cleared the goal".to_owned(),
                _ => "Set a goal".to_owned(),
            },
            Own::Setup => "Proposed a change to your setup".to_owned(),
            Own::Guide => match field("topic") {
                Some(topic) => format!("Read Argonauts Code's guide on {topic}"),
                None => "Read Argonauts Code's guide".to_owned(),
            },
            Own::Lookup => match field("what") {
                Some(what) => format!("Looked up {what} in Argonauts Code's records"),
                None => "Looked up Argonauts Code's records".to_owned(),
            },
        };
        Some(ToolCall::described(ToolKind::Other, text))
    }
}

/// A start Argonauts Code turned down, as one plain line: its answer's first line, such as
/// `No workflow started.` The session reads the rest, which says why, and tries again.
pub(crate) fn turned_down(ok: bool, output: Option<&str>) -> Option<ToolCall> {
    let first = output
        .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(|line| sanitize_line(line).into_owned());
    let refused = !ok
        || first
            .as_deref()
            .is_some_and(|line| line.starts_with("No workflow started"));
    if !refused {
        return None;
    }
    let mut call = ToolCall::described(
        ToolKind::Other,
        first.unwrap_or_else(|| "No workflow started".to_owned()),
    );
    call.finish(false);
    Some(call)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::cell::ToolState;

    #[test]
    fn every_cli_s_name_for_an_own_tool_is_known() {
        for name in [
            "mcp__argonauts_status__argonauts_lookup",
            "mcp:argonauts_status/argonauts_lookup",
            "argonauts_status_argonauts_lookup",
            "argonauts_lookup",
        ] {
            assert_eq!(Own::named(name), Some(Own::Lookup), "{name}");
        }
        assert_eq!(
            Own::named("mcp__argonauts_status__argonauts_start_workflow"),
            Some(Own::StartWorkflow)
        );
        assert_eq!(Own::named("mcp__slack__lookup"), None);
        assert_eq!(Own::named("Read"), None);
    }

    #[test]
    fn a_line_says_what_was_asked_in_words() {
        let line = |own: Own, input| own.line(&input).map(|call| call.title.plain());
        assert_eq!(
            line(Own::Lookup, json!({"what": "reviews"})).as_deref(),
            Some("Looked up reviews in Argonauts Code's records")
        );
        assert_eq!(
            line(Own::Goal, json!({"action": "pause"})).as_deref(),
            Some("Paused the goal")
        );
        assert_eq!(
            line(Own::Goal, json!({"condition": "tests pass"})).as_deref(),
            Some("Set a goal")
        );
        assert_eq!(
            line(Own::Guide, json!({"topic": "workflows"})).as_deref(),
            Some("Read Argonauts Code's guide on workflows")
        );
        assert_eq!(line(Own::StartWorkflow, json!({"request": "x"})), None);
    }

    #[test]
    fn only_a_start_turned_down_has_a_line() {
        assert!(turned_down(true, Some("Started on this.")).is_none());
        let refused = turned_down(
            true,
            Some("No workflow started.\n\"use codex\" (review): dropped, name a CLI"),
        )
        .expect("a line");
        assert_eq!(refused.title.plain(), "No workflow started.");
        assert_eq!(refused.state, ToolState::Failed);
        assert!(turned_down(false, None).is_some());
    }
}
