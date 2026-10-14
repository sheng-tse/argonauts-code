//! A failed session or run in one plain line, such as "Codex refused the answer format
//! Argonauts Code asked for". The CLI's or git's own text stays where it was, behind `r`.

use crate::ErrorCategory;
use crate::events::session::EndReason;
use crate::model_catalog::display_name_from_id;

impl EndReason {
    /// What failed, in words a person reads at a glance, with `cli` the CLI's name ("Codex").
    /// `None` for a session that wasn't a failure.
    pub fn plain_line(&self, cli: &str) -> Option<String> {
        let line = match self {
            Self::Completed | Self::Paused | Self::Stopped => return None,
            Self::CliError { category, message } => cli_error(cli, *category, message),
            Self::Auth { .. } => format!("{cli} got a sign-in error"),
            Self::RateLimited => format!("{cli} is rate limited"),
            Self::UsageLimit { .. } => format!("{cli} is out of plan usage until it resets"),
            Self::Billing { .. } => format!("{cli} is out of credits"),
            Self::ModelUnavailable { model } => {
                format!("{cli} can't run {}", display_name_from_id(model))
            }
            Self::ContextFull => format!("{cli}'s context window is full"),
            Self::LimitHit { .. } => "the session reached its limit".to_owned(),
            Self::Stuck { .. } => format!("{cli} stopped responding"),
            Self::AdapterBroken { .. } => {
                format!("{cli} wrote output this version of Argonauts Code can't read")
            }
            Self::IsolationBroken { .. } => {
                format!("{cli} loaded your plugins into a session that must keep them out")
            }
            Self::StartFailed { .. } => format!("{cli} couldn't start"),
            Self::Crashed { .. } => format!("{cli} stopped without an answer"),
        };
        Some(line)
    }
}

/// The longest failure line, so one long error never fills the screen.
const LONGEST_LINE: usize = 200;

/// What ended a failed run, in one line, from the detail its end recorded: the first line of
/// git's or the CLI's text, whose whole text stays with the run.
pub fn run_failure_line(detail: Option<&str>) -> String {
    let first = detail
        .into_iter()
        .flat_map(str::lines)
        .map(|line| line.trim().trim_end_matches(':').trim_end())
        .find(|line| !line.is_empty());
    let Some(first) = first else {
        return "it recorded no reason".to_owned();
    };
    match first.char_indices().nth(LONGEST_LINE) {
        Some((cut, _)) => format!("{}…", first[..cut].trim_end()),
        None => first.to_owned(),
    }
}

fn cli_error(cli: &str, category: ErrorCategory, message: &str) -> String {
    match category {
        ErrorCategory::InvalidRequest if about_the_answer_format(message) => {
            format!("{cli} refused the answer format Argonauts Code asked for")
        }
        ErrorCategory::InvalidRequest => format!("{cli}'s service rejected the request as invalid"),
        ErrorCategory::AnswerMissed => {
            format!("{cli} didn't answer in the format Argonauts Code asked for")
        }
        ErrorCategory::Auth => format!("{cli} got a sign-in error"),
        ErrorCategory::UsageLimit => format!("{cli} is out of plan usage until it resets"),
        ErrorCategory::RateLimit => format!("{cli} is rate limited"),
        ErrorCategory::Billing => format!("{cli} is out of credits"),
        ErrorCategory::Overloaded => format!("{cli}'s service is overloaded"),
        ErrorCategory::Server => format!("{cli}'s service had a server error"),
        ErrorCategory::Network | ErrorCategory::Timeout => {
            format!("{cli} couldn't reach its service")
        }
        ErrorCategory::ContextFull => format!("{cli}'s context window is full"),
        ErrorCategory::ModelUnavailable => format!("{cli}'s model isn't available"),
        ErrorCategory::Permission => format!("{cli} was refused permission for an action"),
        ErrorCategory::Limit => format!("{cli} reached its own spend cap"),
        ErrorCategory::Interrupted => format!("{cli}'s answer was cut off"),
        ErrorCategory::Cli | ErrorCategory::Stalled | ErrorCategory::Unknown => {
            format!("{cli} gave an error")
        }
    }
}

/// Codex says "Invalid schema for response_format"; other services name the schema or the
/// output format.
fn about_the_answer_format(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["schema", "response_format", "output format", "json_schema"]
        .iter()
        .any(|words| message.contains(words))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_format_is_said_in_words_and_never_retried() {
        let refused = EndReason::CliError {
            category: ErrorCategory::InvalidRequest,
            message: "Invalid schema for response_format 'codex_output_schema': 'schema.required' \
                      is required to include every key in properties. Missing 'summary'"
                .to_owned(),
        };
        assert_eq!(
            refused.plain_line("Codex").as_deref(),
            Some("Codex refused the answer format Argonauts Code asked for")
        );
        assert!(!refused.may_pass());
    }

    #[test]
    fn only_errors_that_may_pass_are_retried() {
        let agent_id = |category| EndReason::CliError {
            category,
            message: String::new(),
        };
        for passing in [
            EndReason::RateLimited,
            agent_id(ErrorCategory::RateLimit),
            agent_id(ErrorCategory::Overloaded),
            agent_id(ErrorCategory::Server),
            agent_id(ErrorCategory::Network),
            agent_id(ErrorCategory::Timeout),
            agent_id(ErrorCategory::AnswerMissed),
            EndReason::Crashed {
                message: String::new(),
            },
        ] {
            assert!(passing.may_pass(), "{passing:?}");
        }
        for repeats in [
            agent_id(ErrorCategory::InvalidRequest),
            agent_id(ErrorCategory::Auth),
            agent_id(ErrorCategory::Permission),
            agent_id(ErrorCategory::Unknown),
            EndReason::Auth {
                message: String::new(),
            },
            EndReason::StartFailed {
                message: String::new(),
            },
            EndReason::ModelUnavailable {
                model: "gpt-x".to_owned(),
            },
        ] {
            assert!(!repeats.may_pass(), "{repeats:?}");
            assert!(repeats.plain_line("Codex").is_some());
        }
        assert_eq!(EndReason::Completed.plain_line("Codex"), None);
    }

    #[test]
    fn a_model_is_named_as_people_read_it() {
        let unavailable = EndReason::ModelUnavailable {
            model: "gpt-5.6-luna".to_owned(),
        };
        let line = unavailable.plain_line("Codex").unwrap();
        assert_eq!(line, "Codex can't run GPT 5.6 Luna");
        assert!(!line.contains("gpt-5.6-luna"), "{line}");
    }

    #[test]
    fn a_failed_run_is_said_in_its_first_line() {
        let git = "`git add --all -- .argonauts/goal/run-1.md` failed (exit status: 1): The \
                   following paths are ignored by one of your .gitignore files:\n.argonauts\n\
                   hint: Use -f if you really want to add them.";
        assert_eq!(
            run_failure_line(Some(git)),
            "`git add --all -- .argonauts/goal/run-1.md` failed (exit status: 1): The following \
             paths are ignored by one of your .gitignore files"
        );
        assert_eq!(run_failure_line(Some("\n  \n")), "it recorded no reason");
        assert_eq!(run_failure_line(None), "it recorded no reason");
        let long = "x".repeat(500);
        let line = run_failure_line(Some(&long));
        assert_eq!(line.chars().count(), LONGEST_LINE + 1);
        assert!(line.ends_with('…'));
    }
}
