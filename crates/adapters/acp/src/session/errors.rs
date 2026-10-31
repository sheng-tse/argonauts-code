//! How an ACP session's failures get their categories: stop reasons, JSON-RPC codes, and the
//! profile's message patterns, each pinned by a recorded fixture.

use argonauts_core::ErrorCategory;
use argonauts_core::agents::driver::DriverStop;
use argonauts_core::models::registry::cli::ErrorPattern;
use regex::Regex;

use crate::session::rpc::RpcError;

/// JSON-RPC's "authentication required", which `argonauts doctor` explains with the agent's own
/// login command.
pub(crate) const AUTH_REQUIRED: i64 = -32000;

#[derive(Debug, Default)]
pub(crate) struct ErrorRules {
    rules: Vec<(Regex, ErrorCategory, bool)>,
}

impl ErrorRules {
    pub fn compile(patterns: &[ErrorPattern]) -> Result<Self, String> {
        let rules = patterns
            .iter()
            .map(|pattern| {
                Regex::new(&pattern.pattern)
                    .map(|regex| (regex, pattern.category, pattern.retryable))
                    .map_err(|err| {
                        format!("the error pattern {:?} is invalid: {err}", pattern.pattern)
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { rules })
    }

    /// The first matching pattern's category and whether it's retryable.
    pub fn classify(&self, text: &str) -> Option<(ErrorCategory, bool)> {
        self.rules
            .iter()
            .find(|(regex, _, _)| regex.is_match(text))
            .map(|(_, category, retryable)| (*category, *retryable))
    }

    /// An error the agent answered with. `fallback` is the category when nothing else fits.
    pub fn rpc_failure(&self, error: &RpcError, fallback: ErrorCategory) -> DriverStop {
        let text = error.text();
        let category = if error.code == AUTH_REQUIRED {
            ErrorCategory::Auth
        } else {
            self.classify(&text)
                .map_or(fallback, |(category, _)| category)
        };
        DriverStop::Failed {
            category,
            message: text,
        }
    }
}

/// The stop reason of a `session/prompt` answer. `None` for one ACP v1 doesn't define.
pub(crate) fn stop_for(reason: &str) -> Option<DriverStop> {
    Some(match reason {
        "end_turn" => DriverStop::EndTurn,
        "cancelled" => DriverStop::Cancelled,
        "max_tokens" => DriverStop::MaxTokens,
        "max_turn_requests" => DriverStop::MaxTurnRequests,
        "refusal" => DriverStop::Refusal,
        _ => return None,
    })
}

pub(crate) fn failed(category: ErrorCategory, message: impl Into<String>) -> DriverStop {
    DriverStop::Failed {
        category,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testing;

    fn hermes_rules() -> ErrorRules {
        ErrorRules::compile(&[
            ErrorPattern {
                pattern: "credentials or agent init failed".into(),
                category: ErrorCategory::Auth,
                retryable: false,
            },
            ErrorPattern {
                pattern: "(?i)rate limit".into(),
                category: ErrorCategory::RateLimit,
                retryable: true,
            },
            ErrorPattern {
                pattern: "(?i)context (window|length)".into(),
                category: ErrorCategory::ContextFull,
                retryable: false,
            },
        ])
        .unwrap()
    }

    fn rpc(code: i64, message: &str) -> RpcError {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    #[tokio::test]
    async fn acp_error_classification() {
        let stops = [
            ("end_turn", DriverStop::EndTurn),
            ("cancelled", DriverStop::Cancelled),
            ("max_tokens", DriverStop::MaxTokens),
            ("max_turn_requests", DriverStop::MaxTurnRequests),
            ("refusal", DriverStop::Refusal),
        ];
        for (reason, stop) in stops {
            assert_eq!(stop_for(reason), Some(stop), "{reason}");
        }
        assert_eq!(stop_for("paused"), None);

        let rules = hermes_rules();
        let cases = [
            (rpc(-32000, "Authentication required"), ErrorCategory::Auth),
            (
                rpc(-32603, "provider says: rate limit exceeded"),
                ErrorCategory::RateLimit,
            ),
            (
                rpc(-32603, "context window is full"),
                ErrorCategory::ContextFull,
            ),
            (
                rpc(-32603, "credentials or agent init failed"),
                ErrorCategory::Auth,
            ),
            (rpc(-32603, "something odd"), ErrorCategory::Cli),
        ];
        for (error, category) in cases {
            assert_eq!(
                rules.rpc_failure(&error, ErrorCategory::Cli),
                failed(category, error.message.clone()),
                "{error:?}"
            );
        }
        assert_eq!(
            rules.classify("Rate limit, retry in 20s"),
            Some((ErrorCategory::RateLimit, true))
        );
        assert!(
            ErrorRules::compile(&[ErrorPattern {
                pattern: "(".into(),
                category: ErrorCategory::Auth,
                retryable: false,
            }])
            .unwrap_err()
            .contains("invalid")
        );

        // A prompt answered with an error ends the session with the pattern's category.
        let mut driven = testing::launch(
            testing::profile_with_rules(hermes_rules()),
            testing::start("/w"),
        );
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        driven
            .agent
            .fail(&prompt, -32603, "429 from provider: rate limit exceeded")
            .await;
        let (end, _) = driven.finish().await;
        assert_eq!(
            end.stop,
            failed(
                ErrorCategory::RateLimit,
                "429 from provider: rate limit exceeded"
            )
        );

        // A stderr line that matches a pattern names the failure when the pipes then close.
        let mut driven = testing::launch(
            testing::profile_with_rules(hermes_rules()),
            testing::start("/w"),
        );
        let initialize = driven.agent.expect("initialize").await;
        driven
            .stderr
            .send("hermes: credentials or agent init failed".into())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(initialize);
        driven.agent.close().await;
        let (end, _) = driven.finish().await;
        assert_eq!(
            end.stop,
            failed(
                ErrorCategory::Auth,
                "hermes: credentials or agent init failed"
            )
        );

        // Closed pipes with nothing on stderr are a disconnect.
        let mut driven =
            testing::launch(testing::profile(Default::default()), testing::start("/w"));
        driven.agent.open(json!({}), json!({})).await;
        driven.agent.expect("session/prompt").await;
        driven.agent.close().await;
        assert_eq!(driven.finish().await.0.stop, DriverStop::Disconnected);
    }
}
