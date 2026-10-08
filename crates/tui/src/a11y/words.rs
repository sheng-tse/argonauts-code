//! Small pieces of the spoken lines: labels, enum values as words, amounts, and text split into
//! one labeled line per line.

use argonauts_core::names::SessionKey;
use argonauts_core::{RunId, Verbosity};
use serde::Serialize;
use serde_json::Value;

use crate::state::AppState;
use crate::text::sanitize_line;

/// Fields worth reading aloud after an event's name, in this order.
const SPOKEN_FIELDS: [&str; 7] = [
    "title", "name", "summary", "reason", "text", "detail", "message",
];

/// An enum value as words: `branch_left` becomes `branch left`. Values with data give their
/// tag, such as `{"reason": "vendor_down", ...}` giving `vendor down`.
pub fn word<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(value) => value_word(&value),
        Err(_) => String::new(),
    }
}

/// A serialized name as words, with a state's journal name put the way the screen says it.
fn plain(name: &str) -> String {
    argonauts_core::text::state_words(name).replace(['_', '-'], " ")
}

fn value_word(value: &Value) -> String {
    match value {
        Value::String(text) => plain(text),
        Value::Object(map) => map
            .values()
            .find_map(|value| value.as_str().map(plain))
            .unwrap_or_default(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::Null | Value::Array(_) => String::new(),
    }
}

/// A run event read from its serialized form: its kind, what happened, and the first few text
/// fields, such as `plan: sealed, 5 cards`. For the events that have no wording of their own.
pub fn event_words<T: Serialize>(kind: &T) -> String {
    let Ok(value) = serde_json::to_value(kind) else {
        return String::new();
    };
    let what = value.get("type").map(value_word).unwrap_or_default();
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let event = match &data {
        Value::Object(map) => ["event", "state", "kind", "status"]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_str))
            .map(|text| text.replace(['_', '-'], " "))
            .unwrap_or_default(),
        other => value_word(other),
    };
    let mut parts: Vec<String> = Vec::new();
    if !event.is_empty() {
        parts.push(event);
    }
    if let Value::Object(map) = &data {
        parts.extend(
            SPOKEN_FIELDS
                .iter()
                .filter_map(|key| map.get(*key).and_then(Value::as_str))
                .map(|text| sanitize_line(text).trim().to_owned())
                .filter(|text| !text.is_empty())
                .take(2),
        );
    }
    format!("{what}: {}", parts.join(", "))
}

/// Who a run event is from: `claude-code, owner, card-002` for a session, else its source.
pub fn speaker(
    state: &AppState,
    run: &RunId,
    session: Option<&SessionKey>,
    source: &str,
) -> String {
    let known = session.and_then(|key| state.session(run, key));
    match known {
        Some(session) => {
            let mut parts = vec![session.vendor.clone(), session.role.as_str().to_owned()];
            if let Some(card) = &session.card {
                parts.push(card.to_string());
            }
            parts.join(", ")
        }
        None => match session {
            Some(key) => key.to_string(),
            None => source.to_owned(),
        },
    }
}

/// One line as it's read out: sanitized, trimmed, and with the dots that set parts apart on
/// screen read as commas.
pub fn spoken(text: &str) -> String {
    sanitize_line(text).trim().replace(" · ", ", ")
}

/// One `label: line` per non-empty line of `text`, each sanitized.
pub fn labeled(label: &str, text: &str) -> Vec<String> {
    text.lines()
        .map(|line| sanitize_line(line).trim_end().to_owned())
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("{label}: {line}"))
        .collect()
}

/// The first `max` non-empty lines of `text`, and how many more there were.
pub fn first_lines(text: &str, max: usize) -> (Vec<String>, usize) {
    let lines: Vec<String> = text
        .lines()
        .map(|line| sanitize_line(line).trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    let more = lines.len().saturating_sub(max);
    (lines.into_iter().take(max).collect(), more)
}

pub fn more_lines(count: usize) -> String {
    match count {
        1 => "and 1 more line".to_owned(),
        n => format!("and {n} more lines"),
    }
}

/// Milliseconds as `4s`, `3m12s`, or `1h05m`.
pub fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

pub fn tokens(count: u64) -> String {
    match count {
        0..1000 => format!("{count} tokens"),
        _ => format!("{:.1}k tokens", count as f64 / 1000.0),
    }
}

/// The verbosity in effect: the latest `[ui]` section, or normal before one arrives.
pub fn verbosity(state: &AppState) -> Verbosity {
    state
        .ui
        .as_ref()
        .map_or(Verbosity::Normal, |ui| ui.verbosity)
}

/// Whether `needed` shows at the verbosity in effect.
pub fn shows(state: &AppState, needed: Verbosity) -> bool {
    rank(verbosity(state)) >= rank(needed)
}

fn rank(verbosity: Verbosity) -> u8 {
    match verbosity {
        Verbosity::Quiet => 0,
        Verbosity::Normal => 1,
        Verbosity::Full => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::RunOutcome;

    #[test]
    fn enum_values_read_as_words() {
        assert_eq!(word(&RunOutcome::BranchLeft), "branch left");
        assert_eq!(word(&Verbosity::Full), "full");
    }

    #[test]
    fn durations_and_tokens() {
        assert_eq!(duration(4_200), "4s");
        assert_eq!(duration(192_000), "3m12s");
        assert_eq!(duration(3_900_000), "1h05m");
        assert_eq!(tokens(950), "950 tokens");
        assert_eq!(tokens(18_100), "18.1k tokens");
    }

    #[test]
    fn labeled_lines_skip_blanks_and_escapes() {
        assert_eq!(
            labeled("codex", "one\n\n\x1b[31mtwo\x1b[0m\n"),
            vec!["codex: one", "codex: two"]
        );
    }

    #[test]
    fn first_lines_count_the_rest() {
        let (shown, more) = first_lines("a\nb\n\nc\nd", 2);
        assert_eq!(shown, vec!["a", "b"]);
        assert_eq!(more, 2);
        assert_eq!(more_lines(1), "and 1 more line");
    }
}
