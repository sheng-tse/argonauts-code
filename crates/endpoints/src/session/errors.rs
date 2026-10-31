//! HTTP failures to end reasons. 401 and 403 are `Auth`, 402 `Billing`, a 429 with
//! `Retry-After` waits and retries within the time limit unless its error code says the credits
//! ran out, used-up plan or credit messages are `UsageLimit` or `Billing`, 5xx and timeouts
//! `AgentError`, context-length errors `ContextFull`, and an unknown model `ModelUnavailable`.
//! Whatever the endpoint sends back is cleaned of the session's credentials before it's kept.

use std::time::Duration;

use argonauts_core::ErrorCategory;
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::safety::redact::redact_secrets;
use serde_json::Value;
use time::format_description::well_known::Rfc2822;
use time::{OffsetDateTime, SignedDuration};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Failure {
    End(EndReason),
    /// Rate limited: wait this long, if the session's time limit allows, then send again.
    Retry {
        after: Duration,
    },
}

/// The longest `Retry-After` taken at its word, a month, which covers a monthly limit's reset.
/// Longer waits are cut to this so no date or clock sum can overflow.
const LONGEST_WAIT: Duration = Duration::from_secs(31 * 24 * 60 * 60);

/// Error codes that say credits or a spend cap ran out, which no wait fixes. OpenAI sends them
/// as `code` or `type`. OpenRouter sends a numeric `code` of 402 instead.
const BILLING_CODES: &[&str] = &[
    "insufficient_quota",
    "billing_hard_limit_reached",
    "billing_not_active",
];

/// Credits or a spend cap ran out. Checked before the usage words, since OpenAI's
/// `insufficient_quota` means unpaid credits even though it says "quota".
const BILLING: &[&str] = &[
    "insufficient_quota",
    "insufficient credit",
    "insufficient funds",
    "insufficient balance",
    "credit balance",
    "out of credits",
    "no credits",
    "more credits",
    "payment",
    "billing",
    "spend limit",
];

/// A plan's usage limit, such as OpenRouter's `free-models-per-day`.
const USAGE: &[&str] = &[
    "quota",
    "usage limit",
    "usage_limit",
    "per-day",
    "per day",
    "daily limit",
    "monthly limit",
];

const CONTEXT: &[&str] = &[
    "context_length_exceeded",
    "context length",
    "maximum context",
    "context window",
    "context_window",
    "too many tokens",
    "prompt is too long",
    "input is too long",
    "exceeds the context",
];

const UNKNOWN_MODEL: &[&str] = &[
    "model_not_found",
    "no such model",
    "unknown model",
    "invalid model",
    "not a valid model",
];

/// `retry_after` is the `Retry-After` header, when there was one. A `status` of 0 is an error
/// sent inside a successful stream with no code. The loose billing and usage words only decide
/// when there's no wait to try: a 429 with `Retry-After` often says "quota" or "billing" about
/// a per-minute limit.
pub(crate) fn classify(
    status: u16,
    retry_after: Option<Duration>,
    body: &str,
    model: &str,
    secrets: &Secrets,
) -> Failure {
    let text = body.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|word| text.contains(word));
    let parsed = serde_json::from_str::<Value>(body).ok();
    let error = parsed.as_ref().map(error_object);
    let message = error_message(error, body, secrets);
    let end = |reason| Failure::End(reason);
    let agent_id = |category| {
        end(EndReason::CliError {
            category,
            message: with_status(status, &message),
        })
    };
    if says(CONTEXT) {
        return end(EndReason::ContextFull);
    }
    let names_a_missing_model = text.contains("model")
        && (status == 404 || text.contains("not found") || text.contains("does not exist"));
    if says(UNKNOWN_MODEL) || names_a_missing_model {
        return end(EndReason::ModelUnavailable {
            model: model.to_owned(),
        });
    }
    if matches!(status, 401 | 403) {
        return end(EndReason::Auth { message });
    }
    if status == 402 || error.is_some_and(out_of_credits) {
        return end(EndReason::Billing { message });
    }
    if status == 429
        && let Some(after) = retry_after
    {
        return Failure::Retry { after };
    }
    match status {
        _ if says(BILLING) => end(EndReason::Billing { message }),
        _ if says(USAGE) => end(EndReason::UsageLimit {
            resets_at: retry_after.and_then(|after| reset_time(OffsetDateTime::now_utc(), after)),
        }),
        429 => end(EndReason::RateLimited),
        408 | 504 => agent_id(ErrorCategory::Timeout),
        503 | 529 => agent_id(ErrorCategory::Overloaded),
        _ if (status >= 500 || status == 0) && text.contains("overloaded") => {
            agent_id(ErrorCategory::Overloaded)
        }
        0 | 500..=599 => agent_id(ErrorCategory::Server),
        400..=499 => agent_id(ErrorCategory::InvalidRequest),
        _ => agent_id(ErrorCategory::Unknown),
    }
}

/// Seconds, or an HTTP date. A date in the past means now, and a wait past `LONGEST_WAIT` is cut
/// to it. A negative or non-finite number counts as no header.
pub(crate) fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<f64>() {
        if !seconds.is_finite() || seconds < 0.0 {
            return None;
        }
        let seconds = seconds.min(LONGEST_WAIT.as_secs_f64());
        return Duration::try_from_secs_f64(seconds).ok();
    }
    let at = OffsetDateTime::parse(value, &Rfc2822).ok()?;
    let wait = at - OffsetDateTime::now_utc();
    Some(Duration::try_from(wait).map_or(Duration::ZERO, |wait| wait.min(LONGEST_WAIT)))
}

/// `now` plus `after`, or `None` past the last date `time` can hold.
pub(crate) fn reset_time(now: OffsetDateTime, after: Duration) -> Option<OffsetDateTime> {
    now.checked_add(SignedDuration::try_from(after).ok()?)
}

/// The error inside a body: `error`, the first item of a list (Gemini's form), or the body
/// itself.
fn error_object(value: &Value) -> &Value {
    let value = value
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or(value);
    value.get("error").unwrap_or(value)
}

fn out_of_credits(error: &Value) -> bool {
    let named = |field| {
        error
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|code| BILLING_CODES.contains(&code.to_lowercase().as_str()))
    };
    named("code") || named("type") || error.get("code").and_then(Value::as_u64) == Some(402)
}

/// The human part of an error: `error.message`, a bare `error` string (Ollama's native form),
/// or the body itself, without credentials and cut short.
fn error_message(error: Option<&Value>, body: &str, secrets: &Secrets) -> String {
    let found = error.and_then(|error| {
        error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
    });
    // Cleaned before the cut, so half a key can't slip past.
    let message = secrets.redact(found.unwrap_or(body).trim());
    const LIMIT: usize = 500;
    match message.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{}…", &message[..cut]),
        None if message.is_empty() => "no details".to_owned(),
        None => message,
    }
}

/// The session's credential values. A proxy or self-hosted server may repeat the key in what it
/// sends back, and a key with no known prefix passes the shared patterns, so each value is
/// replaced exactly before the patterns run.
#[derive(Debug, Clone, Default)]
pub(crate) struct Secrets(Vec<String>);

impl Secrets {
    pub(crate) fn new(values: Vec<String>) -> Self {
        Self(
            values
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect(),
        )
    }

    pub(crate) fn redact(&self, text: &str) -> String {
        let mut clean = text.to_owned();
        for secret in &self.0 {
            clean = clean.replace(secret.as_str(), "[redacted]");
        }
        redact_secrets(&clean).into_owned()
    }

    /// Every string in `value`, in place.
    pub(crate) fn redact_json(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.redact(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.redact_json(item)),
            Value::Object(map) => map.values_mut().for_each(|item| self.redact_json(item)),
            _ => {}
        }
    }
}

/// `url` without its user, password, query, or fragment, any of which may carry a credential.
pub(crate) fn shown_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            // These fail only for a URL that can't hold a user part at all.
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.into()
        }
        // A URL that doesn't parse is never sent, so cut generously: through the last `@`,
        // then from the first `?` or `#`.
        Err(_) => {
            let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
            let rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
            let rest = rest.split(['?', '#']).next().unwrap_or_default();
            match scheme {
                "" => rest.to_owned(),
                _ => format!("{scheme}://{rest}"),
            }
        }
    }
}

fn with_status(status: u16, message: &str) -> String {
    match status {
        0 => message.to_owned(),
        _ => format!("HTTP {status}: {message}"),
    }
}

/// The error category an end reason is reported under.
pub(crate) fn category(reason: &EndReason) -> ErrorCategory {
    match reason {
        EndReason::Auth { .. } => ErrorCategory::Auth,
        EndReason::Billing { .. } => ErrorCategory::Billing,
        EndReason::UsageLimit { .. } => ErrorCategory::UsageLimit,
        EndReason::RateLimited => ErrorCategory::RateLimit,
        EndReason::ContextFull => ErrorCategory::ContextFull,
        EndReason::ModelUnavailable { .. } => ErrorCategory::ModelUnavailable,
        EndReason::CliError { category, .. } => *category,
        EndReason::LimitHit { .. } => ErrorCategory::Limit,
        EndReason::Stopped | EndReason::Paused => ErrorCategory::Interrupted,
        _ => ErrorCategory::Unknown,
    }
}

/// Failures that say the endpoint itself is unwell, which the health board counts.
pub(crate) fn is_health_failure(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::Server
            | ErrorCategory::Overloaded
            | ErrorCategory::Network
            | ErrorCategory::Timeout
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn end(status: u16, body: &Value) -> EndReason {
        match classify(status, None, &body.to_string(), "m", &Secrets::default()) {
            Failure::End(reason) => reason,
            Failure::Retry { .. } => panic!("expected an end"),
        }
    }

    fn openai(message: &str, code: &str) -> Value {
        json!({"error": {"message": message, "type": code, "code": code}})
    }

    #[test]
    fn statuses_and_messages_map_to_end_reasons() {
        assert!(
            matches!(end(401, &openai("bad key", "invalid_api_key")), EndReason::Auth { message } if message == "bad key")
        );
        assert!(matches!(
            end(403, &openai("forbidden", "x")),
            EndReason::Auth { .. }
        ));
        assert!(matches!(
            end(402, &openai("pay", "x")),
            EndReason::Billing { .. }
        ));
        let openai_plan_usage = openai(
            "You exceeded your current quota, please check your plan and billing details.",
            "insufficient_quota",
        );
        assert!(matches!(
            end(429, &openai_plan_usage),
            EndReason::Billing { .. }
        ));
        let per_day = openai(
            "Rate limit exceeded: free-models-per-day. Add 10 credits",
            "429",
        );
        assert!(matches!(
            end(429, &per_day),
            EndReason::UsageLimit { resets_at: None }
        ));
        assert_eq!(
            end(429, &openai("slow down", "rate_limit_exceeded")),
            EndReason::RateLimited
        );
        assert_eq!(
            end(
                400,
                &openai("This model's maximum context length is 8192 tokens", "x")
            ),
            EndReason::ContextFull
        );
        let ollama = json!({"error": "model \"llama9\" not found, try pulling it first"});
        assert_eq!(
            end(404, &ollama),
            EndReason::ModelUnavailable { model: "m".into() }
        );
        assert!(matches!(
            end(503, &openai("busy", "x")),
            EndReason::CliError {
                category: ErrorCategory::Overloaded,
                ..
            }
        ));
        assert!(matches!(
            end(500, &openai("boom", "server_error")),
            EndReason::CliError { category: ErrorCategory::Server, message } if message == "HTTP 500: boom"
        ));
        assert!(matches!(
            end(504, &json!("")),
            EndReason::CliError {
                category: ErrorCategory::Timeout,
                ..
            }
        ));
        assert!(matches!(
            end(422, &openai("bad field", "x")),
            EndReason::CliError {
                category: ErrorCategory::InvalidRequest,
                ..
            }
        ));
    }

    fn classify_429(body: &Value, after: Duration) -> Failure {
        classify(
            429,
            Some(after),
            &body.to_string(),
            "m",
            &Secrets::default(),
        )
    }

    #[test]
    fn a_429_with_retry_after_retries() {
        let after = Duration::from_secs(2);
        let retry = Failure::Retry { after };
        let plain = openai("slow down", "rate_limit_exceeded");
        assert_eq!(classify_429(&plain, after), retry);
        // OpenAI's per-minute limit for an account with no payment method.
        let no_billing = openai(
            "Rate limit reached for requests per min. Please try again in 2s. You can increase \
             your rate limit by adding a payment method at https://platform.openai.com/account/billing.",
            "rate_limit_exceeded",
        );
        assert_eq!(classify_429(&no_billing, after), retry);
        let gemini = json!([{"error": {
            "code": 429,
            "message": "Quota exceeded for metric generate_content_free_tier_requests, limit 15 per minute",
            "status": "RESOURCE_EXHAUSTED",
        }}]);
        assert_eq!(classify_429(&gemini, after), retry);
    }

    #[test]
    fn a_spent_credit_code_ends_even_with_retry_after() {
        let after = Duration::from_secs(2);
        let used_up = openai("You exceeded your current quota.", "insufficient_quota");
        assert!(matches!(
            classify_429(&used_up, after),
            Failure::End(EndReason::Billing { .. })
        ));
        let openrouter = json!({"error": {"code": 402, "message": "Insufficient credits"}});
        assert!(matches!(
            classify_429(&openrouter, after),
            Failure::End(EndReason::Billing { .. })
        ));
        // With no status and no code, the words still decide.
        let in_stream = json!({"error": {"message": "Your credit balance is too low"}});
        assert!(matches!(end(0, &in_stream), EndReason::Billing { .. }));
    }

    #[test]
    fn huge_waits_are_cut_and_never_overflow() {
        for huge in ["1e12", "1e19", "1e20", "1e308", "18446744073709551616"] {
            assert_eq!(parse_retry_after(huge), Some(LONGEST_WAIT), "{huge}");
        }
        for impossible in ["inf", "NaN", "-0.5"] {
            assert_eq!(parse_retry_after(impossible), None, "{impossible}");
        }
        assert_eq!(
            parse_retry_after("Fri, 31 Dec 9999 23:59:59 GMT"),
            Some(LONGEST_WAIT)
        );
        let now = OffsetDateTime::now_utc();
        assert_eq!(reset_time(now, Duration::MAX), None);
        assert_eq!(reset_time(now, Duration::from_secs(300_000_000_000)), None);
        assert!(reset_time(now, LONGEST_WAIT).is_some_and(|at| at > now));
        let usage = openai("daily limit reached", "x").to_string();
        let failure = classify(400, Some(Duration::MAX), &usage, "m", &Secrets::default());
        assert_eq!(
            failure,
            Failure::End(EndReason::UsageLimit { resets_at: None })
        );
    }

    #[test]
    fn messages_never_carry_the_key() {
        const KEY: &str = "plain-test-key-0042";
        let secrets = Secrets::new(vec![KEY.to_owned(), String::new()]);
        let echo = openai(
            &format!("Incorrect API key provided: {KEY}"),
            "invalid_api_key",
        );
        let failure = classify(401, None, &echo.to_string(), "m", &secrets);
        assert_eq!(
            failure,
            Failure::End(EndReason::Auth {
                message: "Incorrect API key provided: [redacted]".into()
            })
        );
        // A plain body with the key across the cut.
        let body = format!("{}{KEY}{}", "x".repeat(495), "y".repeat(100));
        let Failure::End(EndReason::CliError { message, .. }) =
            classify(502, None, &body, "m", &secrets)
        else {
            panic!("expected an agent error");
        };
        assert!(!message.contains("plain-"), "{message}");
        let mut detail = json!({"error": {"message": KEY, "list": [KEY]}});
        secrets.redact_json(&mut detail);
        assert!(!detail.to_string().contains(KEY));
    }

    #[test]
    fn shown_urls_drop_users_and_queries() {
        let shown = |url| shown_url(url);
        assert_eq!(
            shown("https://agent:hunter22@example.test:8443/v1/chat/completions?key=abc#frag"),
            "https://example.test:8443/v1/chat/completions"
        );
        assert_eq!(
            shown("http://127.0.0.1:11434/v1/responses"),
            "http://127.0.0.1:11434/v1/responses"
        );
        // Doesn't parse, since the port isn't a number.
        assert_eq!(
            shown("http://agent:pa/ss@example.test:port/v1?key=abc"),
            "http://example.test:port/v1"
        );
    }

    #[test]
    fn retry_after_reads_seconds_and_dates() {
        assert_eq!(parse_retry_after(" 3 "), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after("0.5"), Some(Duration::from_millis(500)));
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(Duration::ZERO)
        );
        let soon = OffsetDateTime::now_utc() + Duration::from_secs(120);
        let wait = parse_retry_after(&soon.format(&Rfc2822).unwrap()).unwrap();
        assert!(wait > Duration::from_secs(100) && wait <= Duration::from_secs(120));
        assert_eq!(parse_retry_after("soon"), None);
    }

    #[test]
    fn long_messages_are_cut_short() {
        let none = Secrets::default();
        let message = error_message(None, &"x".repeat(2000), &none);
        assert_eq!(message.chars().count(), 501);
        assert_eq!(error_message(None, "", &none), "no details");
    }
}
