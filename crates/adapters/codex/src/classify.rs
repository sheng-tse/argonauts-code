//! `codex exec --json` reports errors as bare messages and exits with 1 for every failure, so
//! the message text is all there is to go on.

use argonauts_core::{ErrorCategory, RetryReason};

/// Rules of (phrases, category, retryable), checked in order against the lowercased message. The
/// first match wins, so the more specific rules come first.
const RULES: &[(&[&str], ErrorCategory, bool)] = &[
    (
        &[
            "usage limit",
            "usage not included",
            "upgrade to plus",
            "upgrade to pro",
        ],
        ErrorCategory::UsageLimit,
        false,
    ),
    (
        &[
            "quota exceeded",
            "insufficient_quota",
            "billing",
            "purchase more credits",
            "credit balance",
        ],
        ErrorCategory::Billing,
        false,
    ),
    (&["token budget exhausted"], ErrorCategory::Budget, false),
    (
        &[
            "context window",
            "contextwindowexceeded",
            "context_window_exceeded",
            "context_length_exceeded",
            "ran out of room",
        ],
        ErrorCategory::ContextFull,
        false,
    ),
    (
        &["rate limit", "rate_limit", "too many requests"],
        ErrorCategory::RateLimit,
        true,
    ),
    (
        &[
            "at capacity",
            "overloaded",
            "high demand",
            "capacity unavailable",
        ],
        ErrorCategory::Overloaded,
        true,
    ),
    (
        &[
            "unauthorized",
            "not logged in",
            "codex login",
            "authentication",
            "refresh token",
            "api key",
            "access token",
        ],
        ErrorCategory::Auth,
        false,
    ),
    (
        &[
            "model rerouted",
            "model_not_found",
            "model not found",
            "unknown model",
            "model is not supported",
            "not supported when using codex",
            "does not exist",
        ],
        ErrorCategory::ModelUnavailable,
        false,
    ),
    (
        &["interrupted", "turn aborted", "cancelled"],
        ErrorCategory::Interrupted,
        false,
    ),
    (
        &[
            "not inside a trusted directory",
            "no prompt provided",
            "failed to read prompt",
            "failed to initialize",
            "agent loop died",
            "fatal error",
        ],
        ErrorCategory::Cli,
        false,
    ),
    (&["timed out", "timeout"], ErrorCategory::Timeout, true),
    (
        &[
            "stream disconnected",
            "connection",
            "network",
            "dns error",
            "error sending request",
        ],
        ErrorCategory::Network,
        true,
    ),
    (
        &[
            "internal server error",
            "server error",
            "bad gateway",
            "service unavailable",
        ],
        ErrorCategory::Server,
        true,
    ),
    (
        &[
            "sandbox",
            "permission denied",
            "operation not permitted",
            "declined",
        ],
        ErrorCategory::Permission,
        false,
    ),
    (
        &[
            "unexpected argument",
            "invalid value",
            "error parsing",
            "error loading config",
            "invalid_request",
            "invalid request",
            "invalid schema",
            "output schema",
            "bad request",
        ],
        ErrorCategory::InvalidRequest,
        false,
    ),
];

/// The category of a Codex error message, and whether retrying soon may help.
pub fn classify_error(message: &str) -> (ErrorCategory, bool) {
    let lower = message.to_lowercase();
    RULES
        .iter()
        .find(|(phrases, _, _)| phrases.iter().any(|p| lower.contains(p)))
        .map_or(
            (ErrorCategory::Unknown, false),
            |&(_, category, retryable)| (category, retryable),
        )
}

/// Codex reports retries as errors such as `Reconnecting... 2/5 (stream disconnected ...)`.
pub fn is_retry_notice(message: &str) -> bool {
    let lower = message.trim_start().to_lowercase();
    lower.starts_with("reconnecting") || lower.starts_with("retrying")
}

pub fn retry_reason(message: &str) -> RetryReason {
    match classify_error(message).0 {
        ErrorCategory::RateLimit => RetryReason::RateLimit,
        ErrorCategory::Overloaded => RetryReason::Overloaded,
        ErrorCategory::Network | ErrorCategory::Timeout => RetryReason::Network,
        ErrorCategory::Server => RetryReason::ServerError,
        _ => RetryReason::Other(message.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_codex_messages() {
        use ErrorCategory::*;
        let cases = [
            (
                "You’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), \
                 or try again at 3:00 PM.",
                UsageLimit,
                false,
            ),
            (
                "Quota exceeded. Check your plan and billing details.",
                Billing,
                false,
            ),
            (
                "Codex ran out of room in the model's context window. Start a new thread or \
                 clear earlier history before retrying.",
                ContextFull,
                false,
            ),
            (
                "exceeded retry limit, last status: 429 Too Many Requests",
                RateLimit,
                true,
            ),
            (
                "Selected model is at capacity. Please try a different model.",
                Overloaded,
                true,
            ),
            (
                "unexpected status 401 Unauthorized: Missing bearer",
                Auth,
                false,
            ),
            (
                "model rerouted: gpt-5.6-luna -> gpt-6-luna (HighRiskCyberActivity)",
                ModelUnavailable,
                false,
            ),
            (
                "The 'gpt-9' model is not supported when using Codex with a ChatGPT account.",
                ModelUnavailable,
                false,
            ),
            (
                "turn aborted. Something went wrong? Hit `/feedback` to report the issue.",
                Interrupted,
                false,
            ),
            (
                "Not inside a trusted directory and --skip-git-repo-check was not specified.",
                Cli,
                false,
            ),
            ("request timed out", Timeout, true),
            (
                "stream disconnected before completion: error sending request",
                Network,
                true,
            ),
            (
                "exceeded retry limit, last status: 500 Internal Server Error",
                Server,
                true,
            ),
            (
                "sandbox error: command was killed by a signal",
                Permission,
                false,
            ),
            (
                "error: unexpected argument '-a' found",
                InvalidRequest,
                false,
            ),
            (
                "Invalid schema for response_format 'codex_output_schema'",
                InvalidRequest,
                false,
            ),
            ("ContextWindowExceeded", ContextFull, false),
            ("something odd happened", Unknown, false),
        ];
        for (message, category, retryable) in cases {
            assert_eq!(classify_error(message), (category, retryable), "{message}");
        }
    }

    #[test]
    fn retry_notices_are_recognized() {
        assert!(is_retry_notice(
            "Reconnecting... 2/5 (stream disconnected before completion: idle timeout)"
        ));
        assert!(is_retry_notice("  retrying request"));
        assert!(!is_retry_notice(
            "Codex ran out of room in the model's context window. Clear history before retrying."
        ));
        assert!(!is_retry_notice("rate limit exceeded: slow down"));
    }

    #[test]
    fn retry_reasons_follow_the_category() {
        assert_eq!(
            retry_reason("Reconnecting... 1/5 (stream disconnected before completion: eof)"),
            RetryReason::Network
        );
        assert_eq!(
            retry_reason("Reconnecting... 1/5 (rate limit exceeded: try later)"),
            RetryReason::RateLimit
        );
        assert_eq!(
            retry_reason("Reconnecting... 1/5 (We’re currently experiencing high demand)"),
            RetryReason::Overloaded
        );
        assert_eq!(
            retry_reason("Reconnecting... 3/5 (500 Internal Server Error)"),
            RetryReason::ServerError
        );
        assert_eq!(
            retry_reason("Reconnecting... 1/5"),
            RetryReason::Other("Reconnecting... 1/5".into())
        );
    }
}
