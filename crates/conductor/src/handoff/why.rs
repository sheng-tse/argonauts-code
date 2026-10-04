//! One line on why a session stopped, for the handoff card and the packet.

use argonauts_core::events::session::{EndReason, Watchdog, loaded_words};
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::{BudgetDimension, ErrorCategory};
use time::OffsetDateTime;
use time::macros::format_description;

/// Vendor messages are cut to this many characters.
const MESSAGE_CHARS: usize = 160;

pub fn stop_line(reason: &EndReason) -> String {
    let message = |text: &str| one_line(text, MESSAGE_CHARS);
    match reason {
        EndReason::Completed => "it finished".to_owned(),
        EndReason::BudgetHit { dimension } => match dimension {
            BudgetDimension::Spend => "it used its whole budget".to_owned(),
            BudgetDimension::Minutes => "it reached its time limit".to_owned(),
            BudgetDimension::ToolCalls => "it reached its tool-call limit".to_owned(),
        },
        EndReason::Paused => "the run paused".to_owned(),
        EndReason::Stopped => "you stopped it".to_owned(),
        EndReason::Stuck {
            watchdog: Watchdog::Inactivity,
        } => "stuck, with no output or activity".to_owned(),
        EndReason::Stuck {
            watchdog: Watchdog::Repeat,
        } => "looping on the same call after a nudge".to_owned(),
        EndReason::UsageLimit {
            resets_at: Some(at),
        } => format!("usage limit, resets at {}", clock(*at)),
        EndReason::UsageLimit { resets_at: None } => "usage limit".to_owned(),
        EndReason::RateLimited => "rate limited for longer than its time budget allows".to_owned(),
        EndReason::VendorError {
            category,
            message: text,
        } => {
            format!("{}: {}", category_words(*category), message(text))
        }
        EndReason::Auth { message: text } => format!("logged out: {}", message(text)),
        EndReason::Billing { message: text } => format!("billing cap: {}", message(text)),
        EndReason::ContextFull => "its context window filled".to_owned(),
        EndReason::ModelUnavailable { model } => {
            format!("model {} is no longer available", message(model))
        }
        EndReason::AdapterBroken { detail } => {
            format!("its CLI's output changed: {}", message(detail))
        }
        EndReason::IsolationBroken { plugins, skills } => format!(
            "it loaded {}, which a blind role mustn't see",
            message(&loaded_words(plugins, skills))
        ),
        EndReason::StartFailed { message: text } => {
            format!("it failed to start: {}", message(text))
        }
        EndReason::Crashed { message: text } => format!("it crashed: {}", message(text)),
    }
}

pub fn category_words(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::Auth => "logged out",
        ErrorCategory::UsageLimit => "usage limit",
        ErrorCategory::RateLimit => "rate limited",
        ErrorCategory::Billing => "billing cap",
        ErrorCategory::Overloaded => "overloaded",
        ErrorCategory::Server => "server error",
        ErrorCategory::Network => "network error",
        ErrorCategory::ContextFull => "context full",
        ErrorCategory::ModelUnavailable => "model unavailable",
        ErrorCategory::InvalidRequest => "invalid request",
        ErrorCategory::Permission => "permission denied",
        ErrorCategory::Budget => "native budget cap",
        ErrorCategory::Timeout => "timed out",
        ErrorCategory::Interrupted => "interrupted",
        ErrorCategory::Cli => "CLI error",
        ErrorCategory::Stalled => "quiet past its heartbeat",
        ErrorCategory::Unknown => "unknown error",
    }
}

/// Such as `2026-09-30 15:45 UTC`.
pub fn clock(at: OffsetDateTime) -> String {
    let utc = at.to_offset(time::UtcOffset::UTC);
    utc.format(format_description!(
        "[year]-[month]-[day] [hour]:[minute] UTC"
    ))
    .unwrap_or_else(|_| utc.to_string())
}

/// Agent or vendor text as one clean line: control sequences dropped, whitespace collapsed, and
/// cut to `max_chars` with an ellipsis.
pub fn one_line(text: &str, max_chars: usize) -> String {
    let clean = sanitize_for_terminal(text);
    let joined = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= max_chars {
        return joined;
    }
    let mut cut: String = joined.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_lines_are_one_clean_line() {
        let crashed = EndReason::Crashed {
            message: "boom\n\u{1b}[31mred\u{1b}[0m   tail".to_owned(),
        };
        assert_eq!(stop_line(&crashed), "it crashed: boom red tail");
        let long = one_line(&"x".repeat(300), 10);
        assert_eq!(long.chars().count(), 10);
        assert!(long.ends_with('…'));
        let at = OffsetDateTime::from_unix_timestamp(1_790_000_000).expect("valid time");
        assert_eq!(
            stop_line(&EndReason::UsageLimit {
                resets_at: Some(at)
            }),
            format!("usage limit, resets at {}", clock(at))
        );
        assert!(clock(at).ends_with(" UTC"));
    }
}
