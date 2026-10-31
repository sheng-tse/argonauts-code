//! `parser`: a built-in adapter's parser, for a CLI that prints another CLI's stream. The
//! checks around it stamp the manifest's own agent on every plan usage reading.

use std::sync::Arc;

use argonauts_core::models::registry::CliManifest;
use argonauts_core::{Adapter, AgentEvent, ErrorCategory, StreamParser};

pub(super) fn parser(
    manifest: &CliManifest,
    reused: Option<&Arc<dyn Adapter>>,
) -> Box<dyn StreamParser> {
    match reused {
        Some(adapter) => adapter.parser(),
        None => Box::new(Missing {
            message: format!(
                "{} reuses the {} parser, which this build didn't pass in",
                manifest.id,
                manifest.parser.as_deref().unwrap_or("unnamed")
            ),
        }),
    }
}

/// `command` refuses to launch without the parser, so this only keeps lines and says why.
struct Missing {
    message: String,
}

impl StreamParser for Missing {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::unknown_line(line)]
    }

    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }

    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        vec![AgentEvent::Error {
            category: ErrorCategory::Cli,
            message: self.message.clone(),
            retryable: false,
        }]
    }
}
