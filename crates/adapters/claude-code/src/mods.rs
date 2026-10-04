//! What Claude Code 2.1.287 and later push for the screen under stream-json: a mod's toast,
//! status line, and log line, the CLI's own notices, and a turn moved to a fallback model.
//! Each becomes [`AgentEvent::ModOutput`], to be shown where Claude Code shows it. None of it
//! reaches the model.

use argonauts_core::{AgentEvent, ModOutputKind};
use serde_json::Value;

use crate::json::{str_at, string_at};

/// The system subtypes that only tell a screen to redraw or refetch, or that Argonauts has no
/// use for yet. They're read, so they never count as unknown.
pub const QUIET: &[&str] = &[
    "ui_invalidate",
    "ui_focus",
    "ui_scroll",
    "commands_changed",
    "turn_preempted",
    "plugin_install",
];

/// What a session pushes, by system subtype. `None` when `subtype` isn't one of them.
pub fn output(subtype: &str, line: &Value) -> Option<AgentEvent> {
    let plugin = string_at(line, "plugin");
    let timeout_ms = line.get("timeout_ms").and_then(Value::as_u64);
    let (kind, text) = match subtype {
        "ui_toast" => (ModOutputKind::Toast, string_at(line, "text")),
        // A null text clears the plugin's line.
        "ui_status" => (ModOutputKind::Status, string_at(line, "text")),
        "ui_log" => (ModOutputKind::Log, string_at(line, "text")),
        "notification" => {
            let urgent = matches!(str_at(line, "priority"), Some("high" | "immediate"));
            let kind = if urgent {
                ModOutputKind::Urgent
            } else {
                ModOutputKind::Notice
            };
            (kind, string_at(line, "text"))
        }
        "informational" => {
            let kind = match str_at(line, "level") {
                Some("warning") => ModOutputKind::Warning,
                _ => ModOutputKind::Info,
            };
            (kind, string_at(line, "content"))
        }
        "model_fallback" => (ModOutputKind::Notice, Some(fallback_notice(line))),
        _ => return None,
    };
    Some(AgentEvent::ModOutput {
        kind,
        plugin,
        text,
        timeout_ms,
    })
}

/// A pane a mod opened shows only in Claude Code's own screen until Argonauts draws panes.
pub fn pane_notice() -> AgentEvent {
    AgentEvent::ModOutput {
        kind: ModOutputKind::Notice,
        plugin: None,
        text: Some("A mod opened a pane, which shows in Claude Code".to_owned()),
        timeout_ms: None,
    }
}

/// Both models, and why the turn moved.
fn fallback_notice(line: &Value) -> String {
    let from = str_at(line, "original_model").unwrap_or("the model");
    let to = str_at(line, "fallback_model").unwrap_or("the fallback model");
    let why = match str_at(line, "trigger") {
        Some("model_not_found") => "isn't available",
        Some("permission_denied" | "model_blocked") => "isn't allowed for this account",
        Some("overloaded") => "is overloaded",
        Some("server_error") => "had a server error",
        _ => "failed",
    };
    format!("{from} {why}, so this turn runs on {to}")
}

/// Whether a line is Claude Code's own notice that it's on its wrap-up allowance. A mod can
/// push the same words, so a toast, a log line, or anything a plugin sent never counts.
pub fn says_wrapping_up(subtype: &str, line: &Value) -> bool {
    let text = match subtype {
        "notification" => str_at(line, "text"),
        "informational" => str_at(line, "content"),
        _ => None,
    };
    line.get("plugin").is_none_or(Value::is_null)
        && text.is_some_and(|text| text.starts_with(crate::quota::WRAP_UP_NOTICE))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn pushed(line: Value) -> (ModOutputKind, Option<String>, Option<String>, Option<u64>) {
        let subtype = line["subtype"].as_str().unwrap().to_owned();
        match output(&subtype, &line) {
            Some(AgentEvent::ModOutput {
                kind,
                plugin,
                text,
                timeout_ms,
            }) => (kind, plugin, text, timeout_ms),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn each_push_lands_as_claude_code_draws_it() {
        let toast = json!({"type": "system", "subtype": "ui_toast", "plugin": "pr-tools",
                           "text": "Checklist loaded", "timeout_ms": 4000});
        assert_eq!(
            pushed(toast),
            (
                ModOutputKind::Toast,
                Some("pr-tools".into()),
                Some("Checklist loaded".into()),
                Some(4000)
            )
        );
        let cleared = json!({"type": "system", "subtype": "ui_status", "plugin": "pr-tools",
                             "text": null});
        assert_eq!(
            pushed(cleared),
            (ModOutputKind::Status, Some("pr-tools".into()), None, None)
        );
        let log = json!({"type": "system", "subtype": "ui_log", "plugin": "pr-tools",
                         "text": "3 reviewers asked"});
        assert_eq!(pushed(log).0, ModOutputKind::Log);
        let urgent = json!({"type": "system", "subtype": "notification", "key": "k",
                            "text": "Build broke", "priority": "immediate"});
        assert_eq!(pushed(urgent).0, ModOutputKind::Urgent);
        let low = json!({"type": "system", "subtype": "notification", "key": "k",
                         "text": "Saved", "priority": "low", "timeout_ms": 2000});
        assert_eq!(pushed(low).0, ModOutputKind::Notice);
        let warning = json!({"type": "system", "subtype": "informational",
                             "content": "CLAUDE.md is large", "level": "warning"});
        assert_eq!(
            pushed(warning),
            (
                ModOutputKind::Warning,
                None,
                Some("CLAUDE.md is large".into()),
                None
            )
        );
        let info = json!({"type": "system", "subtype": "informational",
                          "content": "Indexed", "level": "notice"});
        assert_eq!(pushed(info).0, ModOutputKind::Info);
        let fallback = json!({"type": "system", "subtype": "model_fallback",
                              "trigger": "overloaded", "original_model": "claude-opus-5-5",
                              "fallback_model": "claude-sonnet-5-5", "content": "x"});
        assert_eq!(
            pushed(fallback).2.as_deref(),
            Some("claude-opus-5-5 is overloaded, so this turn runs on claude-sonnet-5-5")
        );
        assert!(output("ui_invalidate", &json!({})).is_none());
        assert!(output("task_started", &json!({})).is_none());
    }
}
