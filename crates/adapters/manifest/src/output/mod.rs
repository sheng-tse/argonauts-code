//! How stdout is read, by `output`. Every kind is wrapped in the same checks: the manifest's
//! `errors` patterns over stderr and error messages, its `exit_codes`, the manifest's own
//! agent on every plan usage reading, and a closing `Result` when the CLI printed none.

mod final_json;
mod jsonl;
mod reuse;
mod rules;
mod text;

use std::collections::VecDeque;
use std::sync::Arc;

use argonauts_core::base::names::AgentId;
use argonauts_core::models::registry::CliManifest;
use argonauts_core::models::registry::cli::OutputKind;
use argonauts_core::{Adapter, AgentEvent, ErrorCategory, StreamParser};

pub(crate) use rules::ErrorRules;

/// Enough of stderr to say why a CLI failed.
const STDERR_TAIL_LINES: usize = 5;

/// Stdout kept whole by `final-json` and `text` stops growing here, so a runaway CLI can't
/// take all memory.
const MAX_KEPT_BYTES: usize = 16 * 1024 * 1024;

/// The parser `manifest.output` names. `reused` is the built-in adapter for `output = "parser"`.
pub(crate) fn parser_for(
    manifest: &CliManifest,
    reused: Option<&Arc<dyn Adapter>>,
) -> Box<dyn StreamParser> {
    let inner: Box<dyn StreamParser> = match manifest.output {
        OutputKind::ArgonautsJsonl => Box::new(jsonl::Jsonl),
        OutputKind::Parser => reuse::parser(manifest, reused),
        OutputKind::FinalJson => Box::new(final_json::FinalJson::new(manifest)),
        OutputKind::Text => Box::new(text::Text::new(&manifest.id)),
    };
    Box::new(Checked {
        inner,
        rules: ErrorRules::lenient(manifest),
        id: manifest.id.clone(),
        saw_result: false,
        saw_error: false,
        stderr_tail: VecDeque::new(),
    })
}

struct Checked {
    inner: Box<dyn StreamParser>,
    rules: ErrorRules,
    id: String,
    saw_result: bool,
    saw_error: bool,
    stderr_tail: VecDeque<String>,
}

impl Checked {
    fn pass(&mut self, mut events: Vec<AgentEvent>) -> Vec<AgentEvent> {
        for event in &mut events {
            match event {
                // Another CLI's readings must never land on the board of the agent whose
                // format it borrows, or of any agent a plugin names.
                AgentEvent::PlanUsage { reading } => reading.agent_id = AgentId(self.id.clone()),
                AgentEvent::Error {
                    category,
                    message,
                    retryable,
                } => {
                    if let Some((matched, again)) = self.rules.classify(message) {
                        *category = matched;
                        *retryable = again;
                    }
                    self.saw_error = true;
                }
                AgentEvent::Result { .. } => self.saw_result = true,
                _ => {}
            }
        }
        events
    }

    fn remember_stderr(&mut self, line: &str) {
        if self.stderr_tail.len() == STDERR_TAIL_LINES {
            self.stderr_tail.pop_front();
        }
        self.stderr_tail.push_back(line.to_owned());
    }

    fn exit_message(&self, exit_code: Option<i32>) -> String {
        let how = match exit_code {
            Some(code) => format!("{} exited with code {code}", self.id),
            None => format!("{} was stopped by a signal", self.id),
        };
        match self.stderr_tail.back() {
            Some(last) => format!("{how}: {last}"),
            None => how,
        }
    }
}

impl StreamParser for Checked {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let events = self.inner.parse_stdout_line(line);
        self.pass(events)
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let inner = self.inner.parse_stderr_line(line);
        let mut events = self.pass(inner);
        let line = line.trim();
        if line.is_empty() {
            return events;
        }
        self.remember_stderr(line);
        let named = events
            .iter()
            .any(|event| matches!(event, AgentEvent::Error { .. }));
        if !named && let Some((category, retryable)) = self.rules.classify(line) {
            self.saw_error = true;
            events.push(AgentEvent::Error {
                category,
                message: line.to_owned(),
                retryable,
            });
        }
        events
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let inner = self.inner.finish(exit_code);
        let mut events = self.pass(inner);
        let mut errors = Vec::new();
        let mapped = exit_code.and_then(|code| self.rules.exit_category(code));
        if let Some(category) = mapped {
            errors.push(AgentEvent::Error {
                category,
                message: self.exit_message(exit_code),
                retryable: false,
            });
        } else if exit_code != Some(0) && !self.saw_error {
            let category = match exit_code {
                Some(_) => ErrorCategory::Cli,
                None => ErrorCategory::Interrupted,
            };
            errors.push(AgentEvent::Error {
                category,
                message: self.exit_message(exit_code),
                retryable: false,
            });
        }
        let failed = !errors.is_empty() || self.saw_error;
        self.saw_error |= failed;
        // Errors go before the result, which closes the session.
        let at = events
            .iter()
            .position(|event| matches!(event, AgentEvent::Result { .. }))
            .unwrap_or(events.len());
        events.splice(at..at, errors);
        if !self.saw_result {
            self.saw_result = true;
            events.push(AgentEvent::Result {
                text: None,
                structured: None,
                is_error: failed || exit_code != Some(0),
            });
        }
        events
    }
}

/// Text kept whole up to [`MAX_KEPT_BYTES`], for the kinds read at exit.
#[derive(Debug, Default)]
pub(crate) struct Kept {
    text: String,
    overflowed: bool,
}

impl Kept {
    pub(crate) fn push_line(&mut self, line: &str) {
        if self.text.len() + line.len() + 1 > MAX_KEPT_BYTES {
            self.overflowed = true;
            return;
        }
        self.text.push_str(line);
        self.text.push('\n');
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }
}

pub(crate) fn overflow_error(id: &str) -> AgentEvent {
    AgentEvent::Error {
        category: ErrorCategory::Cli,
        message: format!(
            "{id} printed more than {} MiB, and the rest was dropped",
            MAX_KEPT_BYTES / (1024 * 1024)
        ),
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::models::registry::cli::ErrorPattern;

    fn manifest() -> CliManifest {
        CliManifest {
            id: "plugin".into(),
            errors: vec![ErrorPattern {
                pattern: "(?i)quota exceeded".into(),
                category: ErrorCategory::UsageLimit,
                retryable: true,
            }],
            exit_codes: [("2".to_owned(), ErrorCategory::Timeout)].into(),
            ..CliManifest::default()
        }
    }

    fn is_error(event: &AgentEvent, want: ErrorCategory) -> bool {
        matches!(event, AgentEvent::Error { category, .. } if *category == want)
    }

    #[test]
    fn stderr_patterns_become_errors_and_others_stay_quiet() {
        let mut parser = parser_for(&manifest(), None);
        assert!(parser.parse_stderr_line("loading config").is_empty());
        let events = parser.parse_stderr_line("Quota exceeded for today");
        assert_eq!(
            events,
            [AgentEvent::Error {
                category: ErrorCategory::UsageLimit,
                message: "Quota exceeded for today".into(),
                retryable: true,
            }]
        );
    }

    #[test]
    fn error_events_are_recategorized_by_pattern() {
        let mut parser = parser_for(&manifest(), None);
        let line =
            r#"{"type":"error","category":"cli","message":"quota exceeded","retryable":false}"#;
        let events = parser.parse_stdout_line(line);
        assert!(
            is_error(&events[0], ErrorCategory::UsageLimit),
            "{events:?}"
        );
    }

    #[test]
    fn a_mapped_exit_code_comes_before_the_closing_result() {
        let mut parser = parser_for(&manifest(), None);
        parser.parse_stderr_line("still waiting");
        let events = parser.finish(Some(2));
        assert!(is_error(&events[0], ErrorCategory::Timeout), "{events:?}");
        let AgentEvent::Error { message, .. } = &events[0] else {
            unreachable!()
        };
        assert_eq!(message, "plugin exited with code 2: still waiting");
        assert!(matches!(
            events[1],
            AgentEvent::Result { is_error: true, .. }
        ));
    }

    #[test]
    fn a_clean_exit_without_a_result_still_closes() {
        let mut parser = parser_for(&manifest(), None);
        assert_eq!(
            parser.finish(Some(0)),
            [AgentEvent::Result {
                text: None,
                structured: None,
                is_error: false
            }]
        );
        let mut killed = parser_for(&manifest(), None);
        let events = killed.finish(None);
        assert!(
            is_error(&events[0], ErrorCategory::Interrupted),
            "{events:?}"
        );
    }
}
