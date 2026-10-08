//! Reads your Codex plan's rate limits through `codex app-server`, which starts no turn and
//! spends nothing. The exchange is JSON-RPC lines without the `jsonrpc` field: `initialize`,
//! `initialized`, then `account/rateLimits/read`.

use std::path::Path;

use argonauts_core::limits::{QuotaAnswer, QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::VendorId;
use argonauts_core::{CommandSpec, env_remove_except};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::adapter::{ID, SUBSCRIPTION_ENV_VARS};

const RATE_LIMITS_REQUEST_ID: u64 = 2;

/// A share used from which the reading is a warning.
pub const WARNING_FROM: f64 = 0.8;

/// codex-cli 0.158.0 exits when stdin closes, before the rate-limit answer arrives, so stdin
/// stays open. The runner stops once [`answer`] reads the answer, or at its timeout.
pub fn command(program: &str, workdir: &Path) -> CommandSpec {
    let mut spec = CommandSpec::new(program, workdir).args(["app-server", "--listen", "stdio://"]);
    spec.stdin = Some(request_lines());
    spec.keep_stdin_open = true;
    // The plan's limits need the ChatGPT login; an API key would answer for the key instead.
    spec.env_remove = env_remove_except(SUBSCRIPTION_ENV_VARS);
    spec
}

fn request_lines() -> String {
    let requests = [
        json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "argonauts",
                    "title": "Argonauts",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            },
        }),
        json!({ "method": "initialized" }),
        json!({
            "id": RATE_LIMITS_REQUEST_ID,
            "method": "account/rateLimits/read",
            "params": { "excludeResetCreditDetails": true },
        }),
    ];
    requests.iter().map(|r| format!("{r}\n")).collect()
}

/// The answer to the rate-limit request, as the most constraining window. An error answer
/// reads as unknown, with the error kept for `doctor`. `None` until the answer arrives.
pub fn parse(stdout: &str, observed_at: OffsetDateTime) -> Option<QuotaReading> {
    let answer = answer(stdout, observed_at)?;
    // Only the binding window carries the payload.
    answer
        .windows
        .into_iter()
        .find(|reading| reading.detail.is_some())
}

/// The answer to the rate-limit request, one reading for each window, and the plan. The
/// binding window carries the account's own block and the payload. `None` until the answer
/// arrives.
pub fn answer(stdout: &str, observed_at: OffsetDateTime) -> Option<QuotaAnswer> {
    let answer = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|message| {
            message.get("id").and_then(Value::as_u64) == Some(RATE_LIMITS_REQUEST_ID)
        })?;
    let unknown = QuotaReading {
        vendor: VendorId(ID.to_owned()),
        status: QuotaStatus::Unknown,
        used: None,
        resets_at: None,
        window: None,
        source: QuotaSource::AppServer,
        observed_at,
        detail: None,
        wrap_up: false,
    };
    let Some(mut result) = answer.get("result").cloned() else {
        let error = json!({ "error": answer.get("error").cloned().unwrap_or(Value::Null) });
        return Some(QuotaAnswer {
            windows: vec![QuotaReading {
                detail: Some(error),
                ..unknown
            }],
            plan: None,
        });
    };
    if let Some(fields) = result.as_object_mut() {
        fields.remove("accountId");
    }
    let limits = RateLimits::deserialize(&result).unwrap_or_default();
    let snapshot = &limits.snapshot;
    let binding = snapshot.binding_window();
    let mut windows = Vec::new();
    let mut binding_at = 0;
    for window in [snapshot.primary.as_ref(), snapshot.secondary.as_ref()]
        .into_iter()
        .flatten()
    {
        if binding.is_some_and(|binding| std::ptr::eq(binding, window)) {
            binding_at = windows.len();
        }
        windows.push(window.reading(&unknown));
    }
    if windows.is_empty() {
        windows.push(unknown);
    }
    let blocked = limits.ordinary_usage_allowed == Some(false) || snapshot.reached.is_some();
    let binding = &mut windows[binding_at];
    if blocked {
        binding.status = QuotaStatus::Rejected;
    }
    binding.detail = Some(result);
    Some(QuotaAnswer {
        windows,
        plan: snapshot.plan.clone().filter(|plan| !plan.is_empty()),
    })
}

/// Such as `five_hour` or `weekly`, matching the names Claude Code uses where they overlap.
fn window_name(minutes: u64) -> String {
    match minutes {
        300 => "five_hour".to_owned(),
        1440 => "daily".to_owned(),
        10080 => "weekly".to_owned(),
        other => format!("{other}_minutes"),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimits {
    /// The main Codex bucket. Other buckets, such as a model's reserve, stay in the detail.
    #[serde(rename = "rateLimits", default)]
    snapshot: Snapshot,
    ordinary_usage_allowed: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    primary: Option<Window>,
    secondary: Option<Window>,
    #[serde(rename = "rateLimitReachedType")]
    reached: Option<String>,
    #[serde(rename = "planType")]
    plan: Option<String>,
}

impl Snapshot {
    /// The fullest window. When two are full, the later reset is the one you wait for.
    fn binding_window(&self) -> Option<&Window> {
        [self.primary.as_ref(), self.secondary.as_ref()]
            .into_iter()
            .flatten()
            .reduce(|best, window| {
                let fuller = window.used_percent > best.used_percent;
                let both_full = window.used_percent >= 100 && best.used_percent >= 100;
                if fuller || (both_full && window.resets_at > best.resets_at) {
                    window
                } else {
                    best
                }
            })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    used_percent: i32,
    #[serde(rename = "windowDurationMins")]
    duration_mins: Option<u64>,
    /// Unix seconds.
    resets_at: Option<i64>,
}

impl Window {
    /// The window on its own, named by its length: a full one refuses work until its reset.
    fn reading(&self, blank: &QuotaReading) -> QuotaReading {
        let used = (f64::from(self.used_percent) / 100.0).clamp(0.0, 1.0);
        let status = if used >= 1.0 {
            QuotaStatus::Rejected
        } else if used >= WARNING_FROM {
            QuotaStatus::Warning
        } else {
            QuotaStatus::Allowed
        };
        QuotaReading {
            status,
            used: Some(used),
            resets_at: self
                .resets_at
                .and_then(|at| OffsetDateTime::from_unix_timestamp(at).ok()),
            window: self.duration_mins.map(window_name),
            ..blank.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-30 12:00 UTC);

    fn answer_lines(result: Value) -> String {
        let init = json!({"id": 1, "result": {"platformOs": "macos"}});
        format!("{init}\n{}\n", json!({"id": 2, "result": result}))
    }

    #[test]
    fn requests_initialize_then_read_rate_limits() {
        let spec = command("codex", Path::new("/w"));
        assert_eq!(spec.args, ["app-server", "--listen", "stdio://"]);
        assert!(spec.keep_stdin_open);
        let methods: Vec<String> = spec
            .stdin
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["method"].to_string())
            .collect();
        assert_eq!(
            methods,
            [
                "\"initialize\"",
                "\"initialized\"",
                "\"account/rateLimits/read\""
            ]
        );
        assert!(spec.env_remove.iter().any(|v| v == "CODEX_API_KEY"));
        assert!(!spec.env_remove.iter().any(|v| v == "CODEX_ACCESS_TOKEN"));
    }

    #[test]
    fn no_answer_yet_is_none() {
        let init = json!({"id": 1, "result": {}}).to_string();
        assert_eq!(parse(&init, NOW), None);
        assert_eq!(parse("", NOW), None);
    }

    #[test]
    fn a_full_window_is_rejected_until_its_reset() {
        let stdout = answer_lines(json!({
            "rateLimits": {
                "primary": {"usedPercent": 100, "windowDurationMins": 300, "resetsAt": 1790776800},
                "secondary": {"usedPercent": 40, "windowDurationMins": 10080, "resetsAt": 1791259619},
            },
            "accountId": "acct-private",
        }));
        let reading = parse(&stdout, NOW).unwrap();
        assert_eq!(reading.status, QuotaStatus::Rejected);
        assert_eq!(reading.used, Some(1.0));
        assert_eq!(reading.window.as_deref(), Some("five_hour"));
        assert_eq!(reading.resets_at, Some(datetime!(2026-09-30 14:00 UTC)));
        assert!(!reading.detail.unwrap().to_string().contains("acct-private"));
    }

    #[test]
    fn each_window_is_a_reading_named_by_its_length_and_the_plan_comes_along() {
        let stdout = answer_lines(json!({
            "rateLimits": {
                "primary": {"usedPercent": 92, "windowDurationMins": 300, "resetsAt": 1790776800},
                "secondary": {"usedPercent": 41, "windowDurationMins": 10080, "resetsAt": 1791259619},
                "planType": "team",
            },
        }));
        let answer = answer(&stdout, NOW).unwrap();
        let windows: Vec<_> = answer
            .windows
            .iter()
            .map(|r| (r.window.as_deref().unwrap(), r.used.unwrap(), r.status))
            .collect();
        assert_eq!(
            windows,
            [
                ("five_hour", 0.92, QuotaStatus::Warning),
                ("weekly", 0.41, QuotaStatus::Allowed)
            ]
        );
        assert_eq!(answer.plan.as_deref(), Some("team"));
        assert_eq!(
            answer.binding().unwrap().window.as_deref(),
            Some("five_hour")
        );
        // The payload goes with the binding window only.
        assert!(answer.windows[0].detail.is_some() && answer.windows[1].detail.is_none());
    }

    #[test]
    fn warnings_blocks_and_errors() {
        let warning = answer_lines(json!({"rateLimits": {
            "primary": {"usedPercent": 85, "windowDurationMins": 90, "resetsAt": null}
        }}));
        let reading = parse(&warning, NOW).unwrap();
        assert_eq!(reading.status, QuotaStatus::Warning);
        assert_eq!(reading.window.as_deref(), Some("90_minutes"));

        let blocked = answer_lines(json!({
            "rateLimits": {"primary": {"usedPercent": 10}, "rateLimitReachedType": "workspace_owner_credits_depleted"},
        }));
        assert_eq!(parse(&blocked, NOW).unwrap().status, QuotaStatus::Rejected);
        let not_allowed = answer_lines(json!({"rateLimits": {}, "ordinaryUsageAllowed": false}));
        assert_eq!(
            parse(&not_allowed, NOW).unwrap().status,
            QuotaStatus::Rejected
        );

        let error = json!({"id": 2, "error": {"code": -32600, "message": "not logged in"}});
        let reading = parse(&error.to_string(), NOW).unwrap();
        assert_eq!(reading.status, QuotaStatus::Unknown);
        assert!(
            reading
                .detail
                .unwrap()
                .to_string()
                .contains("not logged in")
        );
    }
}
