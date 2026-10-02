//! `text`: only a final answer, which becomes the result. The step runner finds the last JSON
//! object in it.

use argonauts_core::{AgentEvent, StreamParser};

use super::Kept;

#[derive(Debug)]
pub(super) struct Text {
    id: String,
    stdout: Kept,
}

impl Text {
    pub(super) fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            stdout: Kept::default(),
        }
    }
}

impl StreamParser for Text {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        self.stdout.push_line(line);
        Vec::new()
    }

    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let text = self.stdout.text().trim();
        let mut events = Vec::new();
        if self.stdout.overflowed() {
            events.push(super::overflow_error(&self.id));
        }
        events.push(AgentEvent::Result {
            text: (!text.is_empty()).then(|| text.to_owned()),
            structured: None,
            is_error: exit_code != Some(0) || self.stdout.overflowed(),
        });
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_final_text_is_the_result() {
        let mut parser = Text::new("text-cli");
        assert!(parser.parse_stdout_line("Done.").is_empty());
        parser.parse_stdout_line(r#"{"status":"done"}"#);
        assert_eq!(
            parser.finish(Some(0)),
            [AgentEvent::Result {
                text: Some("Done.\n{\"status\":\"done\"}".into()),
                structured: None,
                is_error: false
            }]
        );
    }

    #[test]
    fn no_text_and_a_failed_exit() {
        let mut parser = Text::new("text-cli");
        assert_eq!(
            parser.finish(Some(1)),
            [AgentEvent::Result {
                text: None,
                structured: None,
                is_error: true
            }]
        );
    }
}
