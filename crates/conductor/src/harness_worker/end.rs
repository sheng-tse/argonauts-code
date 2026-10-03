//! How a session ended. The adapter's error mapping and the meter decide; the limits service
//! turns the reason into an action.

use argonauts_core::driver::DriverStop;
use argonauts_core::events::session::EndReason;
use argonauts_core::{BudgetDimension, ErrorCategory};
use time::OffsetDateTime;

use super::stop::StopCause;

/// Who cancelled a driven session's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelledBy {
    /// The run paused (`StopAfterTool`).
    Pause,
    /// You, `/stop`, `/reassign`, or a dropped handle.
    Stop,
}

/// The end reason for an error the parser classified. `model` is the model the session ran,
/// which a rejected model's reason names; the vendor's message never stands in for it.
pub fn from_error(
    category: ErrorCategory,
    message: &str,
    resets_at: Option<OffsetDateTime>,
    model: Option<&str>,
) -> EndReason {
    let message = message.to_owned();
    match category {
        ErrorCategory::Auth => EndReason::Auth { message },
        ErrorCategory::UsageLimit => EndReason::UsageLimit { resets_at },
        ErrorCategory::RateLimit => EndReason::RateLimited,
        ErrorCategory::Billing => EndReason::Billing { message },
        ErrorCategory::ContextFull => EndReason::ContextFull,
        // Empty when unknown, and the limits service finds it from the worker and settings.
        ErrorCategory::ModelUnavailable => EndReason::ModelUnavailable {
            model: model.unwrap_or_default().to_owned(),
        },
        // A native cap such as `--max-budget-usd` fired before the meter did.
        ErrorCategory::Budget => EndReason::BudgetHit {
            dimension: BudgetDimension::Spend,
        },
        // Adapters keep `Cli` for a CLI that no longer takes what they give it, such as a
        // rejected config or another protocol version. A crash or a missing result is `Unknown`.
        ErrorCategory::Cli => EndReason::AdapterBroken { detail: message },
        // The conductor's own stall note, which no parser sends, ends nothing by itself.
        ErrorCategory::Stalled | ErrorCategory::Unknown => EndReason::Crashed { message },
        ErrorCategory::Overloaded
        | ErrorCategory::Server
        | ErrorCategory::Network
        | ErrorCategory::InvalidRequest
        | ErrorCategory::Permission
        | ErrorCategory::Timeout
        | ErrorCategory::Interrupted => EndReason::VendorError { category, message },
    }
}

/// A driven session's end: `end_turn` completes, `cancelled` is paused or stopped by who
/// cancelled, `max_turn_requests` hit the tool-call limit, `max_tokens` and `refusal` are vendor
/// errors, a failure keeps its category, and closed pipes mid-turn are a crash.
pub fn from_driver(stop: &DriverStop, cancelled: Option<CancelledBy>) -> EndReason {
    match stop {
        DriverStop::EndTurn => EndReason::Completed,
        DriverStop::Cancelled => match cancelled {
            Some(CancelledBy::Pause) => EndReason::Paused,
            Some(CancelledBy::Stop) | None => EndReason::Stopped,
        },
        DriverStop::MaxTurnRequests => EndReason::BudgetHit {
            dimension: BudgetDimension::ToolCalls,
        },
        DriverStop::MaxTokens => EndReason::VendorError {
            category: ErrorCategory::Unknown,
            message: "max_tokens: the agent reached its output token limit".to_owned(),
        },
        DriverStop::Refusal => EndReason::VendorError {
            category: ErrorCategory::Unknown,
            message: "refusal: the model refused to go on".to_owned(),
        },
        DriverStop::Failed { category, message } => from_error(*category, message, None, None),
        DriverStop::Disconnected => EndReason::Crashed {
            message: "the agent's pipes closed in the middle of a turn".to_owned(),
        },
    }
}

/// An error event as the parser classified it.
#[derive(Debug, Clone, PartialEq)]
pub struct SeenError {
    pub category: ErrorCategory,
    pub message: String,
}

/// What a line session left behind, for [`classify`].
#[derive(Debug, Clone, Default)]
pub struct EndFacts {
    /// Why the worker stopped it, if it did.
    pub stopped_by: Option<StopCause>,
    /// Every turn had its result before any stop began.
    pub finished_before_stop: bool,
    /// The last result, and whether it was an error.
    pub result_is_error: Option<bool>,
    pub result_text: Option<String>,
    /// The last error the parser said was final.
    pub fatal_error: Option<SeenError>,
    /// The last error of any kind, such as a retry notice.
    pub last_error: Option<SeenError>,
    /// A quota reading said the plan's limit was reached, with its reset when known.
    pub quota_rejected: Option<Option<OffsetDateTime>>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// The last stderr lines, for a crash message.
    pub stderr_tail: Option<String>,
    /// The model the session asked for, else the one its CLI reported.
    pub model: Option<String>,
}

/// The end reason of a line session. The worker's own stop wins unless every turn had already
/// finished; then a clean result completes, and errors, quota, and the exit explain the rest.
pub fn classify(facts: &EndFacts) -> EndReason {
    if let Some(cause) = &facts.stopped_by
        && !facts.finished_before_stop
    {
        return cause.reason();
    }
    if facts.result_is_error == Some(false) {
        return EndReason::Completed;
    }
    let resets_at = facts.quota_rejected.flatten();
    if let Some(error) = facts.fatal_error.as_ref().or(facts.last_error.as_ref()) {
        return from_error(
            error.category,
            &error.message,
            resets_at,
            facts.model.as_deref(),
        );
    }
    if let Some(resets_at) = facts.quota_rejected {
        return EndReason::UsageLimit { resets_at };
    }
    if facts.result_is_error == Some(true) {
        let message = facts
            .result_text
            .clone()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| "the agent ended its turn with an error".to_owned());
        return EndReason::Crashed { message };
    }
    EndReason::Crashed {
        message: exit_message(facts),
    }
}

fn exit_message(facts: &EndFacts) -> String {
    let said = facts
        .stderr_tail
        .as_deref()
        .map(str::trim)
        .filter(|tail| !tail.is_empty())
        .map(|tail| format!(": {tail}"))
        .unwrap_or_default();
    match (facts.exit_code, facts.signal) {
        (Some(0), _) => format!("the agent exited without a result{said}"),
        (Some(code), _) => format!("the agent exited with code {code} and no result{said}"),
        (None, Some(signal)) => format!("the agent was killed by signal {signal}{said}"),
        (None, None) => format!("the agent's process was lost{said}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::Watchdog;

    fn error(category: ErrorCategory, message: &str) -> SeenError {
        SeenError {
            category,
            message: message.to_owned(),
        }
    }

    #[test]
    fn errors_map_to_reasons() {
        let resets = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(
            from_error(ErrorCategory::UsageLimit, "limit", Some(resets), None),
            EndReason::UsageLimit {
                resets_at: Some(resets)
            }
        );
        assert_eq!(
            from_error(ErrorCategory::RateLimit, "slow down", None, None),
            EndReason::RateLimited
        );
        assert_eq!(
            from_error(ErrorCategory::Auth, "log in", None, None),
            EndReason::Auth {
                message: "log in".into()
            }
        );
        assert_eq!(
            from_error(ErrorCategory::ContextFull, "full", None, None),
            EndReason::ContextFull
        );
        assert!(matches!(
            from_error(ErrorCategory::Overloaded, "busy", None, None),
            EndReason::VendorError {
                category: ErrorCategory::Overloaded,
                ..
            }
        ));
        assert!(matches!(
            from_error(ErrorCategory::Cli, "mcp server failed", None, None),
            EndReason::AdapterBroken { .. }
        ));
    }

    #[test]
    fn crashes_and_missing_results_go_to_the_retry_ladder() {
        // What the adapters report for an exit 1 with no result.
        let exits = [
            "claude exited with code 1 before it reported a result",
            "codex exited with code 1",
            "agy exited with code 1 before sending a result",
        ];
        for message in exits {
            let facts = EndFacts {
                fatal_error: Some(error(ErrorCategory::Unknown, message)),
                result_is_error: Some(true),
                exit_code: Some(1),
                ..EndFacts::default()
            };
            assert_eq!(
                classify(&facts),
                EndReason::Crashed {
                    message: message.into()
                }
            );
        }
        // ACP's driver after the agent never sent a valid report.
        let no_report = DriverStop::Failed {
            category: ErrorCategory::Unknown,
            message: "no valid report".into(),
        };
        assert_eq!(
            from_driver(&no_report, None),
            EndReason::Crashed {
                message: "no valid report".into()
            }
        );
    }

    #[test]
    fn a_rejected_model_is_named_by_the_session_not_the_message() {
        let said = "There's an issue with the selected model (claude-old-1). Run /model.";
        let facts = EndFacts {
            fatal_error: Some(error(ErrorCategory::ModelUnavailable, said)),
            model: Some("claude-old-1".into()),
            exit_code: Some(1),
            ..EndFacts::default()
        };
        assert_eq!(
            classify(&facts),
            EndReason::ModelUnavailable {
                model: "claude-old-1".into()
            }
        );
        assert_eq!(
            from_error(ErrorCategory::ModelUnavailable, said, None, None),
            EndReason::ModelUnavailable {
                model: String::new()
            }
        );
    }

    #[test]
    fn driver_stops_map_to_reasons() {
        assert_eq!(
            from_driver(&DriverStop::EndTurn, None),
            EndReason::Completed
        );
        assert_eq!(
            from_driver(&DriverStop::Cancelled, Some(CancelledBy::Pause)),
            EndReason::Paused
        );
        assert_eq!(
            from_driver(&DriverStop::Cancelled, Some(CancelledBy::Stop)),
            EndReason::Stopped
        );
        assert_eq!(
            from_driver(&DriverStop::MaxTurnRequests, None),
            EndReason::BudgetHit {
                dimension: BudgetDimension::ToolCalls
            }
        );
        let failed = DriverStop::Failed {
            category: ErrorCategory::Auth,
            message: "run hermes login".into(),
        };
        assert_eq!(
            from_driver(&failed, None),
            EndReason::Auth {
                message: "run hermes login".into()
            }
        );
        assert!(matches!(
            from_driver(&DriverStop::Refusal, None),
            EndReason::VendorError { .. }
        ));
        assert!(matches!(
            from_driver(&DriverStop::Disconnected, None),
            EndReason::Crashed { .. }
        ));
    }

    #[test]
    fn own_stop_wins_until_the_work_finished() {
        let mut facts = EndFacts {
            stopped_by: Some(StopCause::Stuck(Watchdog::Inactivity)),
            result_is_error: Some(false),
            ..EndFacts::default()
        };
        assert_eq!(
            classify(&facts),
            EndReason::Stuck {
                watchdog: Watchdog::Inactivity
            }
        );
        facts.finished_before_stop = true;
        assert_eq!(classify(&facts), EndReason::Completed);
    }

    #[test]
    fn errors_quota_and_exits_explain_the_rest() {
        let facts = EndFacts {
            last_error: Some(error(ErrorCategory::RateLimit, "retrying")),
            fatal_error: Some(error(ErrorCategory::Billing, "no credits")),
            exit_code: Some(1),
            ..EndFacts::default()
        };
        assert_eq!(
            classify(&facts),
            EndReason::Billing {
                message: "no credits".into()
            }
        );
        let facts = EndFacts {
            quota_rejected: Some(None),
            exit_code: Some(1),
            ..EndFacts::default()
        };
        assert_eq!(classify(&facts), EndReason::UsageLimit { resets_at: None });
        let facts = EndFacts {
            exit_code: Some(3),
            stderr_tail: Some("boom".into()),
            ..EndFacts::default()
        };
        match classify(&facts) {
            EndReason::Crashed { message } => {
                assert_eq!(message, "the agent exited with code 3 and no result: boom");
            }
            other => panic!("{other:?}"),
        }
        let facts = EndFacts {
            result_is_error: Some(true),
            result_text: Some("tool failed".into()),
            exit_code: Some(1),
            ..EndFacts::default()
        };
        assert_eq!(
            classify(&facts),
            EndReason::Crashed {
                message: "tool failed".into()
            }
        );
    }
}
