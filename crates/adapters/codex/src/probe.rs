//! Reads your Codex plan's rate limits through `codex app-server`, as Codex's own status
//! screen does, which starts no turn and spends nothing. The exchange is JSON-RPC lines
//! without the `jsonrpc` field: `initialize`, `initialized`, `account/read` for who is signed
//! in, then `account/rateLimits/read`.

use std::path::Path;

use argonauts_core::hash::sha256_hex;
use argonauts_core::limits::{QuotaAnswer, QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::VendorId;
use argonauts_core::plan_usage::{LiveUsage, LiveWindow, plan_name};
use argonauts_core::{CommandSpec, env_remove_except};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::adapter::{ID, SUBSCRIPTION_ENV_VARS};

const RATE_LIMITS_REQUEST_ID: u64 = 2;
const ACCOUNT_REQUEST_ID: u64 = 3;

/// The main Codex bucket's id in `rateLimitsByLimitId`; every other one is a model's own.
const MAIN_BUCKET: &str = "codex";

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
            "id": ACCOUNT_REQUEST_ID,
            "method": "account/read",
            "params": { "refreshToken": false },
        }),
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
    let answer = answer_at_end(stdout, observed_at)?;
    // Only the binding window carries the payload.
    answer
        .windows
        .into_iter()
        .find(|reading| reading.detail.is_some())
}

/// The answer once both the account and the rate limits have replied. `None` until then.
pub fn answer(stdout: &str, observed_at: OffsetDateTime) -> Option<QuotaAnswer> {
    reply(stdout, ACCOUNT_REQUEST_ID)?;
    answer_at_end(stdout, observed_at)
}

/// The answer to the rate-limit request, one reading for each window, the plan, and the whole
/// reading with the account it is for. The binding window carries the payload. `None` until
/// the rate limits reply.
pub fn answer_at_end(stdout: &str, observed_at: OffsetDateTime) -> Option<QuotaAnswer> {
    let answer = reply(stdout, RATE_LIMITS_REQUEST_ID)?;
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
        let error = answer.get("error").cloned().unwrap_or(Value::Null);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .unwrap_or("it answered with an error")
            .to_owned();
        return Some(QuotaAnswer {
            windows: vec![QuotaReading {
                detail: Some(json!({ "error": error })),
                ..unknown
            }],
            ..QuotaAnswer::failed(message)
        });
    };
    let account_id = result
        .as_object_mut()
        .and_then(|fields| fields.remove("accountId"))
        .and_then(|id| id.as_str().map(str::to_owned));
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
    let account_reply = reply(stdout, ACCOUNT_REQUEST_ID);
    let account = account_reply
        .as_ref()
        .and_then(|reply| reply.pointer("/result/account"));
    let plan = snapshot
        .plan
        .as_deref()
        .or_else(|| account.and_then(|account| account.get("planType")?.as_str()))
        .and_then(plan_name);
    let mut live = LiveUsage::new(VendorId(ID.to_owned()), QuotaSource::AppServer, observed_at);
    live.account = account
        .and_then(|account| account.get("email")?.as_str())
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(str::to_owned)
        // Without an email the account's own id still tells two accounts apart. It is kept
        // only as a hash, since nothing shows it.
        .or_else(|| account_id.map(|id| format!("account {}", &sha256_hex(id.as_bytes())[..16])));
    live.plan = plan.clone();
    live.blocked = blocked;
    live.windows = snapshot.live_windows(None);
    for (id, bucket) in &limits.by_limit_id {
        if id != MAIN_BUCKET {
            live.windows
                .extend(bucket.live_windows(bucket.name.as_deref()));
        }
    }
    Some(QuotaAnswer {
        windows,
        plan,
        live: Some(live),
        failed: None,
    })
}

/// The reply to request `id`, once it has come.
fn reply(stdout: &str, id: u64) -> Option<Value> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(id))
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

/// The short label on a card and the title Codex's status screen gives the window.
fn window_words(minutes: Option<u64>) -> (String, String) {
    match minutes {
        Some(300) => ("5-hour".to_owned(), "5h limit".to_owned()),
        Some(1440) => ("daily".to_owned(), "Daily limit".to_owned()),
        Some(10080) => ("weekly".to_owned(), "Weekly limit".to_owned()),
        Some(minutes) if minutes % 60 == 0 => {
            let hours = minutes / 60;
            (format!("{hours}-hour"), format!("{hours}h limit"))
        }
        Some(minutes) => (format!("{minutes}-minute"), format!("{minutes}m limit")),
        None => ("limit".to_owned(), "Limit".to_owned()),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimits {
    /// The main Codex bucket.
    #[serde(rename = "rateLimits", default)]
    snapshot: Snapshot,
    /// Every bucket by its id, the main one too, such as a model's reserve.
    #[serde(rename = "rateLimitsByLimitId", default)]
    by_limit_id: std::collections::BTreeMap<String, Snapshot>,
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
    #[serde(rename = "limitName")]
    name: Option<String>,
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

    /// Its windows as the status screen lists them, a model's bucket under the bucket's name.
    fn live_windows(&self, bucket: Option<&str>) -> Vec<LiveWindow> {
        [self.primary.as_ref(), self.secondary.as_ref()]
            .into_iter()
            .flatten()
            .map(|window| {
                let (label, title) = window_words(window.duration_mins);
                let key = window
                    .duration_mins
                    .map_or_else(|| "limit".to_owned(), window_name);
                let (key, label, title) = match bucket {
                    Some(name) => (
                        format!("{name}:{key}"),
                        format!("{name} {label}"),
                        format!("{name} {title}"),
                    ),
                    None => (key, label, title),
                };
                LiveWindow::new(key, label, title)
                    .used(Some(f64::from(window.used_percent) / 100.0))
                    .resets_at(window.reset_time())
            })
            .collect()
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
    fn reset_time(&self) -> Option<OffsetDateTime> {
        self.resets_at
            .and_then(|at| OffsetDateTime::from_unix_timestamp(at).ok())
    }

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
            resets_at: self.reset_time(),
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
                "\"account/read\"",
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
        let answer = answer_at_end(&stdout, NOW).unwrap();
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
        assert_eq!(answer.plan.as_deref(), Some("Team"));
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

    fn recorded() -> String {
        recording("plan_usage")
    }

    fn recording(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("fixtures/0.160.0/{name}.app-server.jsonl"));
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn reads_that_fail_say_why_and_name_no_account() {
        let answer = answer(&recording("plan_usage_failed"), NOW).unwrap();
        assert_eq!(
            answer.failed.as_deref(),
            Some("failed to fetch codex rate limits: application network policy is unavailable")
        );
        // No account, so no reading of whoever was signed in before is replaced.
        assert!(answer.live.is_none() && answer.plan.is_none());
        let kept = &answer.windows[0];
        assert_eq!(kept.status, QuotaStatus::Unknown);
        assert!(
            kept.detail
                .as_ref()
                .unwrap()
                .pointer("/error/code")
                .is_some()
        );
    }

    #[test]
    fn the_answer_waits_for_the_account_and_ties_the_reading_to_it() {
        let stdout = recorded();
        let without_account: String = stdout
            .lines()
            .filter(|line| !line.starts_with(r#"{"id":3"#))
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(answer(&without_account, NOW).is_none());
        // At the end the rate limits answer alone, for no account.
        let alone = answer_at_end(&without_account, NOW).unwrap().live.unwrap();
        assert!(alone.account.unwrap().starts_with("account "));

        let answer = answer(&stdout, NOW).unwrap();
        let live = answer.live.unwrap();
        assert_eq!(live.account.as_deref(), Some("user@example.com"));
        assert_eq!(live.plan.as_deref(), Some("<plan>"));
        let shown: Vec<(&str, &str, &str)> = live
            .windows
            .iter()
            .map(|w| (w.key.as_str(), w.label.as_str(), w.title.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("five_hour", "5-hour", "5h limit"),
                ("weekly", "weekly", "Weekly limit"),
                (
                    "gpt-reserve:weekly",
                    "gpt-reserve weekly",
                    "gpt-reserve Weekly limit"
                ),
            ]
        );
        assert_eq!(live.windows[0].used, Some(0.0));
        assert_eq!(
            live.windows[1].resets_at,
            OffsetDateTime::from_unix_timestamp(1791259618).ok()
        );
        assert!(!live.blocked);
        // The account's id never reaches the stored payload.
        let detail = answer
            .windows
            .iter()
            .find_map(|w| w.detail.clone())
            .unwrap();
        assert!(detail.get("accountId").is_none());
    }

    #[test]
    fn an_account_read_that_fails_leaves_the_reading_unnamed_or_hashed() {
        let stdout = answer_lines(json!({"rateLimits": {
            "primary": {"usedPercent": 30, "windowDurationMins": 300, "resetsAt": 1790776800}}}));
        let failed = json!({"id": 3, "error": {"code": -32603, "message": "failed to load workspace requirements"}});
        let live = answer(&format!("{stdout}{failed}\n"), NOW)
            .unwrap()
            .live
            .unwrap();
        assert_eq!(live.account, None);
        assert_eq!(live.windows.len(), 1);
    }
}
