use argonauts_core::ErrorCategory;
use serde_json::{Map, Value};

/// Prefix of the structured line agy prints before exiting with code 3.
const AGY_ERROR_PREFIX: &str = "AGY_ERROR:";

/// `AGY_ERROR` keys that can hold the error text, in order of preference.
const MESSAGE_KEYS: &[&str] = &[
    "short_error",
    "user_error_message",
    "message",
    "error",
    "model_error_message",
    "full_error",
];
const STATUS_KEYS: &[&str] = &["canonical_status", "status"];
const CODE_KEYS: &[&str] = &["http_status", "status_code", "error_code", "code"];
const RETRYABLE_KEYS: &[&str] = &["retryable", "is_retryable"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgyError {
    pub category: ErrorCategory,
    pub message: String,
    pub retryable: bool,
}

impl AgyError {
    pub fn from_message(message: impl Into<String>, fallback: ErrorCategory) -> Self {
        let message = message.into();
        let category = classify_text(&message).unwrap_or(fallback);
        Self {
            retryable: is_transient(category),
            category,
            message,
        }
    }

    pub fn from_status(status: &str, message: String) -> Self {
        let category = category_for_status(status, &message);
        Self {
            retryable: is_transient(category),
            category,
            message,
        }
    }

    pub fn permission(message: String) -> Self {
        Self {
            category: ErrorCategory::Permission,
            message,
            retryable: false,
        }
    }
}

pub(crate) fn agy_error_payload(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix(AGY_ERROR_PREFIX)
        .map(str::trim)
}

/// agy 1.2.13 sends `short_error`, `retryable`, and `error_id`. The status and code keys the
/// changelog describes are read when present, and otherwise found in the message text.
pub(crate) fn parse_agy_error(payload: &str) -> AgyError {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(payload) else {
        return AgyError::from_message(payload, ErrorCategory::Unknown);
    };

    let status = first_string(&fields, STATUS_KEYS);
    let mut message = first_string(&fields, MESSAGE_KEYS)
        .or(status)
        .unwrap_or(payload)
        .to_owned();
    if let Some(id) = first_string(&fields, &["error_id"]) {
        message.push_str(&format!(" (error id {id})"));
    }

    let category = classify(
        status.or_else(|| canonical_status_in(&message)),
        first_code(&fields, CODE_KEYS).or_else(|| code_in(&message)),
        &message,
    )
    .unwrap_or(ErrorCategory::Unknown);
    let retryable = RETRYABLE_KEYS
        .iter()
        .find_map(|key| fields.get(*key).and_then(Value::as_bool))
        .unwrap_or_else(|| is_transient(category));
    AgyError {
        category,
        message,
        retryable,
    }
}

/// agy prefixes fatal errors with `error:`.
pub(crate) fn strip_error_marker(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let head = line.get(..6)?;
    head.eq_ignore_ascii_case("error:")
        .then(|| line[6..].trim())
        .filter(|rest| !rest.is_empty())
}

/// Whether `line` is agy's notice that headless mode refused a tool that needed approval.
pub(crate) fn is_soft_denial(line: &str) -> bool {
    line.contains("auto-denied")
}

fn category_for_status(status: &str, message: &str) -> ErrorCategory {
    match status {
        "CANCELED" | "CANCELLED" | "INTERRUPTED" => ErrorCategory::Interrupted,
        "RUNNING" => ErrorCategory::Timeout,
        _ => classify_text(message).unwrap_or(ErrorCategory::Cli),
    }
}

/// Classifies free text such as `API error (attempt 1): UNAVAILABLE (code 503): No capacity ...`.
fn classify_text(message: &str) -> Option<ErrorCategory> {
    classify(canonical_status_in(message), code_in(message), message)
}

fn classify(status: Option<&str>, code: Option<u16>, message: &str) -> Option<ErrorCategory> {
    let from_message = category_from_message(message);
    let quota = || match from_message {
        Some(c @ (ErrorCategory::Billing | ErrorCategory::UsageLimit)) => c,
        _ => ErrorCategory::RateLimit,
    };
    let from_status = status.and_then(|s| match s.to_ascii_uppercase().as_str() {
        "UNAUTHENTICATED" | "PERMISSION_DENIED" => Some(ErrorCategory::Auth),
        "RESOURCE_EXHAUSTED" => Some(quota()),
        "UNAVAILABLE" => Some(ErrorCategory::Overloaded),
        "INTERNAL" | "DATA_LOSS" => Some(ErrorCategory::Server),
        "DEADLINE_EXCEEDED" => Some(ErrorCategory::Timeout),
        "CANCELLED" | "CANCELED" | "ABORTED" => Some(ErrorCategory::Interrupted),
        "INVALID_ARGUMENT" | "FAILED_PRECONDITION" | "OUT_OF_RANGE" | "NOT_FOUND" => {
            Some(from_message.unwrap_or(ErrorCategory::InvalidRequest))
        }
        _ => None,
    });
    let from_code = code.and_then(|c| match c {
        401 | 403 => Some(ErrorCategory::Auth),
        429 => Some(quota()),
        400 | 404 | 409 | 413 | 422 => Some(from_message.unwrap_or(ErrorCategory::InvalidRequest)),
        408 | 504 => Some(ErrorCategory::Timeout),
        503 | 529 => Some(ErrorCategory::Overloaded),
        500..=599 => Some(ErrorCategory::Server),
        _ => None,
    });
    from_status.or(from_code).or(from_message)
}

const CANONICAL_STATUSES: &[&str] = &[
    "UNAUTHENTICATED",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "UNAVAILABLE",
    "INTERNAL",
    "DATA_LOSS",
    "DEADLINE_EXCEEDED",
    "CANCELLED",
    "ABORTED",
    "INVALID_ARGUMENT",
    "FAILED_PRECONDITION",
    "OUT_OF_RANGE",
    "NOT_FOUND",
];

fn canonical_status_in(message: &str) -> Option<&str> {
    message
        .split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
        .find(|token| CANONICAL_STATUSES.contains(token))
}

/// The number in `(code 503)`, the form agy uses for HTTP and gRPC codes in error text.
fn code_in(message: &str) -> Option<u16> {
    let rest = message.split_once("(code ")?.1;
    rest.split(')').next()?.trim().parse().ok()
}

/// Keywords for each category. The first match wins, so specific causes come before generic ones.
const CATEGORY_KEYWORDS: &[(ErrorCategory, &[&str])] = &[
    (
        ErrorCategory::Auth,
        &[
            "authentication required",
            "not authenticated",
            "unauthenticated",
            "not logged in",
            "api key not valid",
            "invalid api key",
            "login required",
        ],
    ),
    (
        ErrorCategory::Billing,
        &["billing", "spend cap", "prepaid credits", "out of credits"],
    ),
    (
        ErrorCategory::UsageLimit,
        &[
            "daily quota",
            "per day",
            "quota exhausted",
            "exhausted quota",
            "usage limit",
        ],
    ),
    (
        ErrorCategory::RateLimit,
        &[
            "rate limit",
            "rate-limit",
            "too many requests",
            "per minute",
        ],
    ),
    (
        ErrorCategory::ModelUnavailable,
        &[
            "invalid model",
            "not recognized as a known model",
            "model not found",
        ],
    ),
    (
        ErrorCategory::ContextFull,
        &[
            "context window",
            "context length",
            "prompt is too long",
            "too many tokens",
        ],
    ),
    (
        ErrorCategory::Overloaded,
        &["overloaded", "capacity", "unavailable"],
    ),
    (
        ErrorCategory::Timeout,
        &["deadline exceeded", "timed out", "timeout"],
    ),
    (
        ErrorCategory::Interrupted,
        &["interrupted", "canceled", "cancelled"],
    ),
    (
        ErrorCategory::InvalidRequest,
        &[
            "not supported",
            "not defined",
            "unexpected argument",
            "empty prompt",
            "conflicts with",
            "invalid argument",
        ],
    ),
    (ErrorCategory::Server, &["internal error", "server error"]),
    (
        ErrorCategory::Network,
        &["network", "connection", "no such host", "unreachable"],
    ),
    (
        ErrorCategory::Permission,
        &["auto-denied", "permission denied"],
    ),
];

fn category_from_message(message: &str) -> Option<ErrorCategory> {
    let text = message.to_ascii_lowercase();
    CATEGORY_KEYWORDS
        .iter()
        .find(|(_, needles)| needles.iter().any(|needle| text.contains(needle)))
        .map(|(category, _)| *category)
}

fn is_transient(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::RateLimit
            | ErrorCategory::Overloaded
            | ErrorCategory::Server
            | ErrorCategory::Network
            | ErrorCategory::Timeout
    )
}

fn first_string<'a>(fields: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| fields.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|s| !s.is_empty())
}

fn first_code(fields: &Map<String, Value>, keys: &[&str]) -> Option<u16> {
    keys.iter().find_map(|key| match fields.get(*key)? {
        Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_agy_error_prefix() {
        assert_eq!(
            agy_error_payload(r#"AGY_ERROR: {"short_error":"x"}"#),
            Some(r#"{"short_error":"x"}"#)
        );
        assert_eq!(agy_error_payload("error: something"), None);
    }

    #[test]
    fn fallback_payload_uses_short_error_text() {
        let err = parse_agy_error(r#"{"short_error":"model overloaded, try again later"}"#);
        assert_eq!(err.category, ErrorCategory::Overloaded);
        assert_eq!(err.message, "model overloaded, try again later");
        assert!(err.retryable);
    }

    #[test]
    fn canonical_status_and_code_win_over_text() {
        let err = parse_agy_error(
            r#"{"short_error":"request failed","status":"RESOURCE_EXHAUSTED","error_code":429,"retryable":false,"error_id":"e-1"}"#,
        );
        assert_eq!(err.category, ErrorCategory::RateLimit);
        assert_eq!(err.message, "request failed (error id e-1)");
        assert!(!err.retryable);

        let err = parse_agy_error(r#"{"short_error":"daily quota reached","error_code":"429"}"#);
        assert_eq!(err.category, ErrorCategory::UsageLimit);

        let err = parse_agy_error(r#"{"short_error":"nope","http_status":401}"#);
        assert_eq!(err.category, ErrorCategory::Auth);
        assert!(!err.retryable);

        let err = parse_agy_error(r#"{"short_error":"boom","code":502}"#);
        assert_eq!(err.category, ErrorCategory::Server);
    }

    #[test]
    fn billing_caps_are_not_rate_limits() {
        let err = parse_agy_error(
            r#"{"short_error":"project spend cap reached","status":"RESOURCE_EXHAUSTED"}"#,
        );
        assert_eq!(err.category, ErrorCategory::Billing);
        assert!(!err.retryable);
    }

    #[test]
    fn unparseable_payload_keeps_the_raw_text() {
        let err = parse_agy_error("not json at all");
        assert_eq!(err.category, ErrorCategory::Unknown);
        assert_eq!(err.message, "not json at all");
        let err = parse_agy_error(r#"{"weird":1}"#);
        assert_eq!(err.message, r#"{"weird":1}"#);
    }

    #[test]
    fn error_marker_is_case_insensitive() {
        assert_eq!(
            strip_error_marker("Error: authentication required. Run 'agy' to log in."),
            Some("authentication required. Run 'agy' to log in.")
        );
        assert_eq!(strip_error_marker("error: x"), Some("x"));
        assert_eq!(strip_error_marker("error:"), None);
        assert_eq!(strip_error_marker("warning: y"), None);
        assert_eq!(strip_error_marker("é"), None);
    }

    #[test]
    fn messages_map_to_categories() {
        let cases = [
            (
                "authentication required. Run 'agy' to log in.",
                ErrorCategory::Auth,
            ),
            (
                "invalid model selection (--model \"x\" --effort \"\"): model x is not recognized as a known model",
                ErrorCategory::ModelUnavailable,
            ),
            (
                "flags provided but not defined: -foo",
                ErrorCategory::InvalidRequest,
            ),
            (
                "--effort is not supported for the current model",
                ErrorCategory::InvalidRequest,
            ),
            ("connection reset by peer", ErrorCategory::Network),
            ("something odd", ErrorCategory::Cli),
            (
                "API error (attempt 1): UNAVAILABLE (code 503): No capacity available for model gemini-3.6-flash-low on the server",
                ErrorCategory::Overloaded,
            ),
            (
                "API error: RESOURCE_EXHAUSTED (code 429): Quota exceeded for requests per day",
                ErrorCategory::UsageLimit,
            ),
            (
                "API error: RESOURCE_EXHAUSTED (code 429): Quota exceeded, try again shortly",
                ErrorCategory::RateLimit,
            ),
            (
                "API error: PERMISSION_DENIED (code 403): caller lacks access",
                ErrorCategory::Auth,
            ),
        ];
        for (text, want) in cases {
            assert_eq!(
                AgyError::from_message(text, ErrorCategory::Cli).category,
                want,
                "{text}"
            );
        }
    }

    #[test]
    fn result_statuses_map_to_categories() {
        assert_eq!(
            category_for_status("INTERRUPTED", ""),
            ErrorCategory::Interrupted
        );
        assert_eq!(
            category_for_status("CANCELED", ""),
            ErrorCategory::Interrupted
        );
        assert_eq!(category_for_status("RUNNING", ""), ErrorCategory::Timeout);
        assert_eq!(category_for_status("INVALID", ""), ErrorCategory::Cli);
        assert_eq!(
            category_for_status("ERROR", "model not found"),
            ErrorCategory::ModelUnavailable
        );
    }

    #[test]
    fn status_and_code_are_found_in_text() {
        let text = "API error (attempt 1): UNAVAILABLE (code 503): No capacity";
        assert_eq!(canonical_status_in(text), Some("UNAVAILABLE"));
        assert_eq!(code_in(text), Some(503));
        assert_eq!(canonical_status_in("API error: boom"), None);
        assert_eq!(code_in("(code x)"), None);
    }

    #[test]
    fn soft_denial_notices_are_recognized() {
        assert!(is_soft_denial(
            "the run_command tool(s) required approval that headless mode cannot prompt for, so they were auto-denied."
        ));
        assert!(!is_soft_denial("warning: something"));
    }
}
