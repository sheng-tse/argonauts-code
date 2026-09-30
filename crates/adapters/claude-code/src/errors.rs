use argonauts_core::{ErrorCategory, RetryReason};
use serde_json::Value;

use crate::json::{str_at, u64_at};

pub(crate) fn retryable(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::RateLimit
            | ErrorCategory::UsageLimit
            | ErrorCategory::Overloaded
            | ErrorCategory::Server
            | ErrorCategory::Network
    )
}

pub(crate) fn from_api_error(code: &str, text: &str) -> ErrorCategory {
    let category = match code {
        "authentication_failed"
        | "oauth_org_not_allowed"
        | "account_on_hold"
        | "cloud_credential_error" => ErrorCategory::Auth,
        "billing_error" => ErrorCategory::Billing,
        "rate_limit" => ErrorCategory::RateLimit,
        "overloaded" => ErrorCategory::Overloaded,
        "invalid_request" => ErrorCategory::InvalidRequest,
        "model_not_found" => ErrorCategory::ModelUnavailable,
        "server_error" => ErrorCategory::Server,
        _ => ErrorCategory::Unknown,
    };
    // A subscription's usage limit arrives as `rate_limit`; only the text tells them apart.
    match (category, from_text(text)) {
        (
            ErrorCategory::RateLimit,
            Some(refined @ (ErrorCategory::UsageLimit | ErrorCategory::Billing)),
        ) => refined,
        (ErrorCategory::Unknown, Some(refined)) => refined,
        _ => category,
    }
}

pub(crate) fn retry_reason(event: &Value) -> RetryReason {
    let no_response = event.get("error_status").is_none_or(Value::is_null);
    match str_at(event, "error") {
        Some("rate_limit") => RetryReason::RateLimit,
        Some("overloaded") => RetryReason::Overloaded,
        Some("server_error") => RetryReason::ServerError,
        _ if no_response => RetryReason::Network,
        Some(other) => RetryReason::Other(other.to_owned()),
        None => RetryReason::Other("unknown".to_owned()),
    }
}

pub(crate) fn from_result(result: &Value, message: &str) -> ErrorCategory {
    match str_at(result, "subtype") {
        Some("error_max_budget_usd" | "error_max_turns") => return ErrorCategory::Budget,
        Some("error_max_structured_output_retries") => return ErrorCategory::InvalidRequest,
        _ => {}
    }
    if let Some(category) = str_at(result, "terminal_reason").and_then(from_terminal_reason) {
        return category;
    }
    if let Some(category) = from_http_status(u64_at(result, "api_error_status")) {
        return category;
    }
    if let Some(reason) = str_at(result, "startup_failure_reason") {
        let auth = reason.starts_with("org_") || reason.starts_with("gateway_");
        return if auth {
            ErrorCategory::Auth
        } else {
            ErrorCategory::Cli
        };
    }
    from_text(message).unwrap_or(ErrorCategory::Unknown)
}

fn from_terminal_reason(reason: &str) -> Option<ErrorCategory> {
    match reason {
        "prompt_too_long" => Some(ErrorCategory::ContextFull),
        "aborted_streaming" | "aborted_tools" => Some(ErrorCategory::Interrupted),
        "budget_exhausted" | "max_turns" => Some(ErrorCategory::Budget),
        "structured_output_retry_exhausted" => Some(ErrorCategory::InvalidRequest),
        _ => None,
    }
}

fn from_http_status(status: u64) -> Option<ErrorCategory> {
    match status {
        400 => Some(ErrorCategory::InvalidRequest),
        401 | 403 => Some(ErrorCategory::Auth),
        402 => Some(ErrorCategory::Billing),
        404 => Some(ErrorCategory::ModelUnavailable),
        413 => Some(ErrorCategory::ContextFull),
        429 => Some(ErrorCategory::RateLimit),
        529 => Some(ErrorCategory::Overloaded),
        500..=599 => Some(ErrorCategory::Server),
        _ => None,
    }
}

/// Lowercase phrases from Claude Code's error reference. The first category with a match wins,
/// so "You've hit your monthly spend limit" is billing rather than a usage limit.
const TEXT_HINTS: &[(ErrorCategory, &[&str])] = &[
    (
        ErrorCategory::Billing,
        &[
            "spend limit",
            "credit balance",
            "usage credits required",
            "billing",
        ],
    ),
    (ErrorCategory::UsageLimit, &["hit your", "usage limit"]),
    (
        ErrorCategory::Auth,
        &[
            "not logged in",
            "/login",
            "invalid api key",
            "authentication",
            "oauth token",
            "account is on hold",
            "credentials expired",
        ],
    ),
    (
        ErrorCategory::RateLimit,
        &["rate limit", "429", "temporarily limiting requests"],
    ),
    (
        ErrorCategory::Overloaded,
        &["overloaded", "529", "high load"],
    ),
    (
        ErrorCategory::ContextFull,
        &["prompt is too long", "context window", "context length"],
    ),
    (
        ErrorCategory::ModelUnavailable,
        &[
            "issue with the selected model",
            "model not found",
            "not_found_error",
        ],
    ),
    (
        ErrorCategory::Network,
        &[
            "unable to connect",
            "connection refused",
            "socket is closed",
            "no internet",
            "timed out",
            "econnreset",
            "enotfound",
        ],
    ),
    (
        ErrorCategory::Server,
        &[
            "internal server error",
            "server error",
            "no response from api",
        ],
    ),
    (
        ErrorCategory::InvalidRequest,
        &["unknown option", "not a valid", "invalid", "requires --"],
    ),
];

pub(crate) fn from_text(text: &str) -> Option<ErrorCategory> {
    let text = text.to_lowercase();
    TEXT_HINTS
        .iter()
        .find(|(_, phrases)| phrases.iter().any(|phrase| text.contains(phrase)))
        .map(|&(category, _)| category)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn api_error_codes_map_to_categories() {
        assert_eq!(
            from_api_error("authentication_failed", "Invalid API key"),
            ErrorCategory::Auth
        );
        assert_eq!(from_api_error("billing_error", ""), ErrorCategory::Billing);
        assert_eq!(
            from_api_error("model_not_found", ""),
            ErrorCategory::ModelUnavailable
        );
        assert_eq!(from_api_error("overloaded", ""), ErrorCategory::Overloaded);
        assert_eq!(
            from_api_error("rate_limit", "Request rejected (429)"),
            ErrorCategory::RateLimit
        );
    }

    #[test]
    fn subscription_limits_are_told_apart_from_rate_limits() {
        assert_eq!(
            from_api_error("rate_limit", "You've hit your session limit · resets 3pm"),
            ErrorCategory::UsageLimit
        );
        assert_eq!(
            from_api_error("unknown", "Not logged in · Please run /login"),
            ErrorCategory::Auth
        );
        assert_eq!(from_api_error("unknown", "???"), ErrorCategory::Unknown);
    }

    #[test]
    fn retry_reasons() {
        let retry = |error: Value, status: Value| {
            retry_reason(&json!({"error": error, "error_status": status}))
        };
        assert_eq!(
            retry(json!("rate_limit"), json!(429)),
            RetryReason::RateLimit
        );
        assert_eq!(
            retry(json!("overloaded"), json!(529)),
            RetryReason::Overloaded
        );
        assert_eq!(
            retry(json!("server_error"), json!(500)),
            RetryReason::ServerError
        );
        assert_eq!(retry(json!("unknown"), Value::Null), RetryReason::Network);
        assert_eq!(
            retry(json!("invalid_request"), json!(400)),
            RetryReason::Other("invalid_request".into())
        );
    }

    #[test]
    fn result_errors_prefer_structured_fields() {
        let category = |v: Value| from_result(&v, "");
        assert_eq!(
            category(json!({"subtype": "error_max_budget_usd"})),
            ErrorCategory::Budget
        );
        assert_eq!(
            category(json!({"subtype": "error_max_turns"})),
            ErrorCategory::Budget
        );
        assert_eq!(
            category(
                json!({"subtype": "error_during_execution", "terminal_reason": "prompt_too_long"})
            ),
            ErrorCategory::ContextFull
        );
        assert_eq!(
            category(
                json!({"subtype": "error_during_execution", "terminal_reason": "aborted_streaming"})
            ),
            ErrorCategory::Interrupted
        );
        assert_eq!(
            category(json!({"subtype": "error_during_execution", "api_error_status": 401})),
            ErrorCategory::Auth
        );
        assert_eq!(
            category(
                json!({"subtype": "error_during_execution", "startup_failure_reason": "cwd_unavailable"})
            ),
            ErrorCategory::Cli
        );
        assert_eq!(
            from_result(
                &json!({"subtype": "success", "is_error": true}),
                "Credit balance is too low"
            ),
            ErrorCategory::Billing
        );
        assert_eq!(
            category(json!({"subtype": "success"})),
            ErrorCategory::Unknown
        );
    }

    #[test]
    fn text_heuristics() {
        let cases = [
            ("You've hit your weekly limit", ErrorCategory::UsageLimit),
            (
                "You've hit your monthly spend limit",
                ErrorCategory::Billing,
            ),
            ("OAuth token has expired", ErrorCategory::Auth),
            (
                "API Error: Repeated 529 Overloaded errors",
                ErrorCategory::Overloaded,
            ),
            (
                "Unable to connect to API (ECONNREFUSED)",
                ErrorCategory::Network,
            ),
            (
                "API Error: 500 Internal server error",
                ErrorCategory::Server,
            ),
            (
                "error: unknown option '--nope'",
                ErrorCategory::InvalidRequest,
            ),
            (
                "There's an issue with the selected model",
                ErrorCategory::ModelUnavailable,
            ),
        ];
        for (text, want) in cases {
            assert_eq!(from_text(text), Some(want), "{text}");
        }
        assert_eq!(from_text("something odd happened"), None);
    }

    #[test]
    fn only_transient_categories_are_retryable() {
        assert!(retryable(ErrorCategory::RateLimit));
        assert!(retryable(ErrorCategory::UsageLimit));
        assert!(!retryable(ErrorCategory::Auth));
        assert!(!retryable(ErrorCategory::Budget));
    }
}
