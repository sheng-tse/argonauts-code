//! Claude Code's plan usage, read live and for nothing: a headless process that is sent
//! `initialize`, which names the account, and the `get_usage` control request, which answers
//! with the plan's windows as Claude Code's own `/usage` draws them. No message is ever sent,
//! so no turn runs.
//!
//! `get_usage` is asked twice. The first skips the behaviors, Claude Code's scan of its own
//! recent transcripts, so the windows come back at once; the second has them. The windows show
//! as soon as they're in, the probe waits for both, and when the scan is slow the windows alone
//! are the answer.

pub(crate) mod plan_limits;

use std::path::Path;

use argonauts_core::base::names::AgentId;
use argonauts_core::models::limits::{PlanUsageAnswer, PlanUsageSource};
use argonauts_core::models::plan_usage::{
    Behavior, Behaviors, ExtraUsage, LiveUsage, LiveWindow, UsageReport, UsageShare, plan_name,
};
use argonauts_core::{CommandSpec, env_remove_except};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::launch::command::{SUBSCRIPTION_KEYS, flag_value};
use crate::parser::json::{array_at, bool_at, str_at, string_at};

const ID: &str = "claude-code";
const INITIALIZE: &str = "initialize";
const WINDOWS: &str = "usage-windows";
const BEHAVIORS: &str = "usage-behaviors";

/// Plans whose `/usage` lists the Sonnet week, as Claude Code 2.1.295 decides it.
const SONNET_WEEK_PLANS: &[&str] = &["max", "team"];

/// The command that reads the plan's windows. It sends no message, and names `model` all the
/// same, as every session does: whatever the CLI did with one, it would never be on the
/// model your settings make its default.
pub fn probe_command(program: &str, workdir: &Path, model: &str) -> CommandSpec {
    let args = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        // No MCP server starts, and no settings file runs a hook: the probe reads, nothing else.
        "--strict-mcp-config",
        "--setting-sources",
        "",
    ];
    let mut spec = CommandSpec::new(program, workdir).args(args);
    // One that isn't a plain name is left off, since a reading is still worth having.
    if let Ok(model) = flag_value("model", model) {
        spec = spec.args(["--model", model]);
    }
    let requests = [
        (INITIALIZE, json!({"subtype": "initialize"})),
        (
            WINDOWS,
            json!({"subtype": "get_usage", "skip_behaviors": true}),
        ),
        (BEHAVIORS, json!({"subtype": "get_usage"})),
    ];
    spec.stdin = Some(
        requests
            .iter()
            .map(|(id, body)| {
                let line = json!({"type": "control_request", "request_id": id, "request": body});
                format!("{line}\n")
            })
            .collect(),
    );
    // Closing stdin ends the session before it answers.
    spec.keep_stdin_open = true;
    // The plan's windows need the subscription login; an API key has none.
    spec.env_remove = env_remove_except(SUBSCRIPTION_KEYS);
    spec
}

/// The answer once both `get_usage` replies are in, or at once when the windows failed.
/// `None` until then.
pub fn answer(stdout: &str, read_at: OffsetDateTime) -> Option<PlanUsageAnswer> {
    let replies = Replies::of(stdout);
    if replies.failed.is_none() {
        replies.behaviors.as_ref()?;
    }
    replies.answer(read_at)
}

/// The windows, to show while the behaviors scan goes on. Only once `initialize` has named the
/// account, so they're kept for the account the whole answer will name.
pub fn windows_first(stdout: &str, read_at: OffsetDateTime) -> Option<PlanUsageAnswer> {
    let replies = Replies::of(stdout);
    replies.account.as_ref()?;
    replies.windows.as_ref()?;
    replies.answer(read_at)
}

/// The answer from whatever came before the probe ended: the windows without the behaviors
/// when the scan was slow.
pub fn answer_at_end(stdout: &str, read_at: OffsetDateTime) -> Option<PlanUsageAnswer> {
    Replies::of(stdout).answer(read_at)
}

#[derive(Default)]
struct Replies {
    account: Option<Value>,
    windows: Option<Value>,
    behaviors: Option<Value>,
    /// Claude Code's words when the windows' request failed.
    failed: Option<String>,
}

impl Replies {
    fn of(stdout: &str) -> Self {
        let mut replies = Self::default();
        for line in stdout.lines() {
            let Ok(line) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let Some(reply) = line.get("response") else {
                continue;
            };
            let ok = str_at(reply, "subtype") == Some("success");
            let body = reply.get("response").cloned().unwrap_or(Value::Null);
            match str_at(reply, "request_id") {
                Some(INITIALIZE) if ok => replies.account = body.get("account").cloned(),
                Some(WINDOWS) if ok => replies.windows = Some(body),
                Some(WINDOWS) => {
                    let error = str_at(reply, "error").map(str::trim);
                    let error = error.filter(|error| !error.is_empty());
                    replies.failed = Some(error.unwrap_or("it answered with an error").to_owned());
                }
                Some(BEHAVIORS) => replies.behaviors = Some(if ok { body } else { Value::Null }),
                _ => {}
            }
        }
        replies
    }

    fn answer(self, read_at: OffsetDateTime) -> Option<PlanUsageAnswer> {
        if let Some(failed) = self.failed {
            return Some(PlanUsageAnswer::failed(failed));
        }
        let usage = self.windows.filter(|body| !body.is_null())?;
        let mut live = reading(&usage, self.account.as_ref(), read_at);
        live.behaviors = self
            .behaviors
            .as_ref()
            .and_then(|body| body.get("behaviors"))
            .and_then(behaviors);
        Some(PlanUsageAnswer {
            windows: live.plan_usage_readings(),
            plan: live.plan.clone(),
            live: Some(live),
            failed: None,
        })
    }
}

/// `get_usage`'s answer as a reading of the account `initialize` named.
fn reading(usage: &Value, account: Option<&Value>, read_at: OffsetDateTime) -> LiveUsage {
    let mut live = LiveUsage::new(AgentId(ID.to_owned()), PlanUsageSource::GetUsage, read_at);
    live.account = account
        .and_then(|account| string_at(account, "email"))
        .map(|email| email.trim().to_owned())
        .filter(|email| !email.is_empty());
    let subscription = str_at(usage, "subscription_type");
    live.plan = subscription
        .or_else(|| account.and_then(|account| str_at(account, "subscriptionType")))
        .and_then(plan_name);
    let Some(limits) = usage.get("rate_limits").filter(|limits| limits.is_object()) else {
        return live;
    };
    let window = |key: &str, label: &str, title: &str| {
        let limit = limits.get(key).filter(|limit| limit.is_object())?;
        let used = limit.get("utilization").and_then(Value::as_f64)?;
        Some(
            LiveWindow::new(key, label, title)
                .used(Some(used / 100.0))
                .resets_at(time_at(limit, "resets_at")),
        )
    };
    live.windows
        .extend(window("five_hour", "session", "Current session"));
    live.windows
        .extend(window("seven_day", "week", "Current week (all models)"));
    // As Claude Code's own screen: the Sonnet week for these plans, and for a plan it
    // doesn't name.
    if subscription.is_none_or(|plan| SONNET_WEEK_PLANS.contains(&plan)) {
        live.windows.extend(window(
            "seven_day_sonnet",
            "Sonnet week",
            "Current week (Sonnet only)",
        ));
    }
    for scoped in array_at(limits, "model_scoped") {
        let (Some(name), Some(used)) = (
            string_at(scoped, "display_name"),
            scoped.get("utilization").and_then(Value::as_f64),
        ) else {
            continue;
        };
        live.windows.push(
            LiveWindow::new(
                format!("model:{name}"),
                format!("{name} week"),
                format!("Current week ({name})"),
            )
            .used(Some(used / 100.0))
            .resets_at(time_at(scoped, "resets_at")),
        );
    }
    live.extra = limits.get("extra_usage").and_then(extra_usage);
    live
}

/// Usage credits. Amounts come in the currency's minor units, cents for dollars.
fn extra_usage(extra: &Value) -> Option<ExtraUsage> {
    if !extra.is_object() {
        return None;
    }
    let places = extra
        .get("decimal_places")
        .and_then(Value::as_u64)
        .unwrap_or(2)
        .min(6);
    let unit = 10f64.powi(i32::try_from(places).unwrap_or(2));
    let amount = |key: &str| {
        extra
            .get(key)
            .and_then(Value::as_f64)
            .map(|minor| minor / unit)
    };
    Some(ExtraUsage {
        enabled: bool_at(extra, "is_enabled"),
        spent: amount("used_credits").unwrap_or(0.0),
        limit: amount("monthly_limit"),
        currency: string_at(extra, "currency").unwrap_or_else(|| "USD".to_owned()),
        used: extra
            .get("utilization")
            .and_then(Value::as_f64)
            .map(|percent| (percent / 100.0).clamp(0.0, 1.0)),
    })
}

fn behaviors(body: &Value) -> Option<Behaviors> {
    if !body.is_object() {
        return None;
    }
    let of = |span: &str| -> Vec<Behavior> {
        let Some(span) = body.get(span) else {
            return Vec::new();
        };
        let mut found: Vec<Behavior> = array_at(span, "behaviors")
            .iter()
            .filter_map(|behavior| {
                Some(Behavior {
                    key: string_at(behavior, "key")?,
                    share: (behavior.get("pct")?.as_f64()? / 100.0).clamp(0.0, 1.0),
                })
            })
            .filter(|behavior| behavior.share > 0.0)
            .collect();
        found.sort_by(|a, b| b.share.total_cmp(&a.share));
        found
    };
    Some(Behaviors {
        day: of("day"),
        week: of("week"),
        day_report: body.get("day").map(usage_report),
        week_report: body.get("week").map(usage_report),
    })
}

/// The rest of `/usage`'s report on a span: requests, sessions, and the share each agent,
/// skill, plugin and MCP server took, the largest first.
fn usage_report(span: &Value) -> UsageReport {
    let count = |key: &str| span.get(key).and_then(Value::as_u64).unwrap_or(0);
    let shares = |key: &str| -> Vec<UsageShare> {
        let mut found: Vec<UsageShare> = array_at(span, key)
            .iter()
            .filter_map(|item| {
                Some(UsageShare {
                    name: string_at(item, "name")?,
                    share: (item.get("pct")?.as_f64()? / 100.0).clamp(0.0, 1.0),
                })
            })
            .filter(|item| item.share > 0.0)
            .collect();
        found.sort_by(|a, b| b.share.total_cmp(&a.share));
        found
    };
    UsageReport {
        requests: count("request_count"),
        sessions: count("session_count"),
        agents: shares("agents"),
        skills: shares("skills"),
        plugins: shares("plugins"),
        mcp_servers: shares("mcp_servers"),
    }
}

fn time_at(value: &Value, key: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(str_at(value, key)?.trim(), &Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::models::limits::PlanUsageStatus;
    use time::macros::datetime;

    const READ: OffsetDateTime = datetime!(2026-10-09 02:55:16 UTC);

    fn recorded() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/plan_usage.stdout.jsonl");
        std::fs::read_to_string(path).unwrap()
    }

    fn behaviors_line(body: Value) -> String {
        let line = json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": BEHAVIORS, "response": body}});
        format!("{line}\n")
    }

    #[test]
    fn the_probe_sends_three_requests_and_never_a_message() {
        let spec = probe_command("claude", Path::new("/work"), "haiku");
        assert!(spec.keep_stdin_open);
        let named = spec.args.iter().position(|arg| arg == "--model");
        let model = named.and_then(|at| spec.args.get(at + 1));
        assert_eq!(model.map(String::as_str), Some("haiku"));
        let odd = probe_command(
            "claude",
            Path::new("/work"),
            "--dangerously-skip-permissions",
        );
        assert!(
            !odd.args
                .iter()
                .any(|arg| arg == "--model" || arg.starts_with("--danger"))
        );
        let lines: Vec<Value> = spec
            .stdin
            .as_deref()
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(lines.iter().all(|line| line["type"] == "control_request"));
        let subtypes: Vec<&str> = lines
            .iter()
            .map(|line| line["request"]["subtype"].as_str().unwrap())
            .collect();
        assert_eq!(subtypes, ["initialize", "get_usage", "get_usage"]);
        assert_eq!(lines[1]["request"]["skip_behaviors"], true);
        assert!(spec.args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(spec.env_remove.iter().any(|var| var == "ANTHROPIC_API_KEY"));
        assert!(
            !spec
                .env_remove
                .iter()
                .any(|var| var == "CLAUDE_CODE_OAUTH_TOKEN")
        );
    }

    #[test]
    fn the_recorded_reply_reads_as_the_accounts_windows() {
        let stdout = recorded();
        // The recording skipped the behaviors, so the probe would still be waiting.
        assert!(answer(&stdout, READ).is_none());
        let answer = answer_at_end(&stdout, READ).unwrap();
        let live = answer.live.unwrap();
        assert_eq!(live.account.as_deref(), Some("user@example.com"));
        assert_eq!(live.plan.as_deref(), Some("<plan>"));
        assert_eq!(answer.plan.as_deref(), Some("<plan>"));
        let shown: Vec<(&str, &str, Option<f64>)> = live
            .windows
            .iter()
            .map(|window| (window.label.as_str(), window.title.as_str(), window.used))
            .collect();
        assert_eq!(
            shown,
            [
                ("session", "Current session", Some(0.07)),
                ("week", "Current week (all models)", Some(0.23)),
                ("Fable week", "Current week (Fable)", Some(0.0)),
            ]
        );
        assert_eq!(
            live.windows[0].resets_at,
            Some(datetime!(2026-10-09 05:40:00.346149 UTC))
        );
        let extra = live.extra.unwrap();
        assert!(extra.enabled);
        assert_eq!((extra.spent, extra.limit), (0.0, Some(50.0)));
        assert_eq!(extra.currency, "USD");
        assert_eq!(answer.windows.len(), 3);
        assert!(
            answer
                .windows
                .iter()
                .all(|r| r.status == PlanUsageStatus::Allowed)
        );
        assert_eq!(answer.windows[0].window.as_deref(), Some("five_hour"));
        assert!(live.behaviors.is_none());
    }

    #[test]
    fn the_windows_show_first_once_the_account_is_named() {
        let stdout = recorded();
        let early = windows_first(&stdout, READ).unwrap().live.unwrap();
        assert_eq!(early.account.as_deref(), Some("user@example.com"));
        assert_eq!(early.windows.len(), 3);
        // Without `initialize` they'd be kept for no account, then read as another one's.
        let windows_only: String = stdout
            .lines()
            .filter(|line| !line.contains(r#""request_id":"initialize""#))
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(windows_only.contains(WINDOWS));
        assert!(windows_first(&windows_only, READ).is_none());
        assert!(windows_first("", READ).is_none());
    }

    #[test]
    fn the_behaviors_reply_completes_the_answer() {
        // The shape Claude Code 2.1.295 declares for `behaviors`.
        let body = json!({
            "session": {}, "subscription_type": "team", "rate_limits_available": true,
            "rate_limits": null,
            "behaviors": {
                "day": {"request_count": 40, "session_count": 3, "behaviors": [
                    {"key": "long_context", "pct": 95, "count": 30},
                    {"key": "subagent_heavy", "pct": 90, "count": 20},
                    {"key": "cron", "pct": 0, "count": 0},
                ], "agents": [], "skills": [], "plugins": [], "mcp_servers": []},
                "week": {"request_count": 90, "session_count": 9, "behaviors": [
                    {"key": "high_parallel", "pct": 62, "count": 12},
                ], "agents": [], "skills": [], "plugins": [], "mcp_servers": []},
            },
        });
        let stdout = recorded() + &behaviors_line(body);
        let live = answer(&stdout, READ).unwrap().live.unwrap();
        let behaviors = live.behaviors.unwrap();
        let day: Vec<(&str, f64)> = behaviors
            .day
            .iter()
            .map(|b| (b.key.as_str(), b.share))
            .collect();
        assert_eq!(day, [("long_context", 0.95), ("subagent_heavy", 0.9)]);
        assert_eq!(behaviors.week[0].key, "high_parallel");
        // The windows come from the first reply all the same.
        assert_eq!(live.windows.len(), 3);
    }

    #[test]
    fn the_report_counts_requests_and_sessions_and_who_took_the_most() {
        let body = json!({
            "session": {}, "subscription_type": "team", "rate_limits_available": true,
            "behaviors": {
                "day": {"request_count": 312, "session_count": 4, "behaviors": [],
                        "agents": [{"name": "Explore", "pct": 30}],
                        "skills": [{"name": "test", "pct": 12}, {"name": "review", "pct": 40}],
                        "plugins": [], "mcp_servers": [{"name": "github", "pct": 20}]},
                "week": {"request_count": 1, "session_count": 1, "behaviors": [],
                         "agents": [], "skills": [], "plugins": [], "mcp_servers": []},
            },
        });
        let stdout = recorded() + &behaviors_line(body);
        let behaviors = answer(&stdout, READ)
            .unwrap()
            .live
            .unwrap()
            .behaviors
            .unwrap();
        let day = behaviors.day_report.unwrap();
        assert_eq!((day.requests, day.sessions), (312, 4));
        let skills: Vec<(&str, f64)> = day
            .skills
            .iter()
            .map(|skill| (skill.name.as_str(), skill.share))
            .collect();
        assert_eq!(
            skills,
            [("review", 0.4), ("test", 0.12)],
            "the largest first"
        );
        assert_eq!(day.agents[0].name, "Explore");
        assert_eq!(day.mcp_servers[0].name, "github");
        assert_eq!(behaviors.week_report.unwrap().sessions, 1);
    }

    #[test]
    fn a_failed_behaviors_scan_still_answers_with_the_windows() {
        let failed = json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": BEHAVIORS, "error": "scan failed"}});
        let stdout = format!("{}{failed}\n", recorded());
        let live = answer(&stdout, READ).unwrap().live.unwrap();
        assert!(live.behaviors.is_none() && !live.windows.is_empty());
    }

    #[test]
    fn an_api_key_session_has_no_plan_windows() {
        let usage = json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": WINDOWS, "response": {
                "session": {}, "subscription_type": null, "rate_limits_available": false,
                "rate_limits": null, "behaviors": null}}});
        let live = answer_at_end(&format!("{usage}\n"), READ)
            .unwrap()
            .live
            .unwrap();
        assert!(live.windows.is_empty() && live.extra.is_none());
        assert_eq!(live.account, None);
        assert_eq!(live.plan, None);
    }

    #[test]
    fn a_pro_plan_lists_no_sonnet_week_and_null_windows_are_skipped() {
        let usage = json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": WINDOWS, "response": {
                "subscription_type": "pro", "rate_limits": {
                    "five_hour": {"utilization": 99.5, "resets_at": "2026-10-09T05:40:00Z"},
                    "seven_day": {"utilization": null, "resets_at": null},
                    "seven_day_sonnet": {"utilization": 10, "resets_at": null},
                    "extra_usage": {"is_enabled": false, "monthly_limit": null,
                                    "used_credits": null, "utilization": null}}}}});
        let answer = answer_at_end(&format!("{usage}\n"), READ).unwrap();
        let live = answer.live.unwrap();
        assert_eq!(live.windows.len(), 1);
        assert_eq!(live.plan.as_deref(), Some("Pro"));
        assert_eq!(answer.windows[0].status, PlanUsageStatus::Warning);
        let extra = live.extra.unwrap();
        assert!(!extra.enabled && extra.limit.is_none());
    }

    #[test]
    fn nothing_before_the_windows_reply_is_an_answer() {
        assert!(answer_at_end("", READ).is_none());
        let init_only: String = recorded().lines().take(1).collect();
        assert!(answer_at_end(&init_only, READ).is_none());
    }

    #[test]
    fn a_refused_windows_request_answers_at_once_with_why() {
        let init: String = recorded().lines().take(1).collect();
        let refused = json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": WINDOWS, "error": "get_usage is not supported"}});
        // The behaviors reply never has to come.
        let answer = answer(&format!("{init}\n{refused}\n"), READ).unwrap();
        assert_eq!(answer.failed.as_deref(), Some("get_usage is not supported"));
        // Not tied to the account, so nothing kept for it is touched.
        assert!(answer.live.is_none() && answer.windows.is_empty());
    }
}
