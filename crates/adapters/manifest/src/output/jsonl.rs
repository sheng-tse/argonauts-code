//! `argonauts-jsonl`: one normalized `AgentEvent` JSON object per line, Argonauts Code's own plugin
//! protocol. Any other line is kept as `Unknown`.

use argonauts_core::{AgentEvent, StreamParser};

pub(super) struct Jsonl;

impl StreamParser for Jsonl {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let event = serde_json::from_str(line).unwrap_or_else(|_| AgentEvent::unknown_line(line));
        vec![event]
    }

    /// Stderr is kept in the raw log. The checks around every parser read it for errors.
    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }

    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_events_and_keeps_the_rest() {
        let mut parser = Jsonl;
        assert_eq!(
            parser.parse_stdout_line(r#"{"type":"message","text":"hi","parent":null}"#),
            [AgentEvent::Message {
                text: "hi".into(),
                parent: None
            }]
        );
        assert_eq!(
            parser.parse_stdout_line(r#"{"type":"mystery","x":1}"#),
            [AgentEvent::Unknown {
                raw: json!({"type": "mystery", "x": 1})
            }]
        );
        assert_eq!(
            parser.parse_stdout_line("plain words"),
            [AgentEvent::Unknown {
                raw: json!("plain words")
            }]
        );
        assert!(parser.parse_stdout_line("   ").is_empty());
    }
}
