//! The free quota probe: `agy -p "/usage" --output-format json`, with `--model` when the
//! sessions name one. The CLI answers `/usage` itself, so it starts no turn and spends no quota.

use std::path::Path;

use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::VendorId;
use argonauts_core::{CommandSpec, env_remove_except};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::command::{API_KEY_ENV, AUTO_UPDATE_ENV, AUTO_UPDATE_VALUE, flag_value};

/// Used share of a window from which the reading counts as a warning.
const WARNING_AT: f64 = 0.8;

pub fn quota_probe_command(program: &str, workdir: &Path, model: Option<&str>) -> CommandSpec {
    let mut spec =
        CommandSpec::new(program, workdir).args(["-p", "/usage", "--output-format", "json"]);
    // Every call names its model, as sessions do; one that isn't a plain name is left off,
    // since a reading is still worth having.
    if let Some(model) = model.and_then(|model| flag_value("model", model).ok()) {
        spec = spec.args(["--model", model]);
    }
    spec.env_set = vec![(AUTO_UPDATE_ENV.to_owned(), AUTO_UPDATE_VALUE.to_owned())];
    // The key stays, so an API-key login reads its own quota. agy ignores it otherwise.
    spec.env_remove = env_remove_except(&[API_KEY_ENV]);
    spec
}

/// agy 1.2.13 puts the buckets in `command.data.groups[].buckets[]` and repeats them as tab
/// separated lines in `response`. The shape is undocumented, so both are read, and `None`
/// means nothing usable came back.
pub fn parse_usage(vendor: &str, stdout: &str) -> Option<QuotaReading> {
    let payload = last_json_object(stdout)?;
    if payload
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status != "SUCCESS")
    {
        return None;
    }
    let data = payload.pointer("/command/data");
    let groups = data
        .map(groups_from_data)
        .filter(|groups| !groups.is_empty())
        .or_else(|| {
            payload
                .get("response")
                .and_then(Value::as_str)
                .map(groups_from_text)
        })?;
    let bucket = tightest(&groups)?;
    let remaining = bucket.remaining.clamp(0.0, 1.0);
    let used = 1.0 - remaining;
    let status = if remaining <= 0.0 {
        QuotaStatus::Rejected
    } else if used >= WARNING_AT {
        QuotaStatus::Warning
    } else {
        QuotaStatus::Allowed
    };
    Some(QuotaReading {
        vendor: VendorId(vendor.to_owned()),
        status,
        used: Some(used),
        resets_at: bucket.resets_at,
        window: bucket.window.clone(),
        source: QuotaSource::UsageProbe,
        observed_at: OffsetDateTime::now_utc(),
        detail: Some(data.cloned().unwrap_or(payload)),
        wrap_up: false,
    })
}

#[derive(Debug, Clone, PartialEq)]
struct Group {
    name: String,
    buckets: Vec<Bucket>,
}

#[derive(Debug, Clone, PartialEq)]
struct Bucket {
    window: Option<String>,
    /// 0 to 1.
    remaining: f64,
    resets_at: Option<OffsetDateTime>,
}

/// The default model family's group decides, since one reading stands for the whole vendor;
/// the other groups stay in `detail`. Within it, the window closest to running out wins.
fn tightest(groups: &[Group]) -> Option<&Bucket> {
    let default_family = groups
        .iter()
        .filter(|group| group.name.to_ascii_lowercase().contains("gemini"));
    lowest(default_family.flat_map(|group| &group.buckets))
        .or_else(|| lowest(groups.iter().flat_map(|group| &group.buckets)))
}

fn lowest<'a>(buckets: impl Iterator<Item = &'a Bucket>) -> Option<&'a Bucket> {
    buckets.min_by(|a, b| a.remaining.total_cmp(&b.remaining))
}

fn groups_from_data(data: &Value) -> Vec<Group> {
    data.get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|group| Group {
            name: text_field(group, "name").unwrap_or_default(),
            buckets: group
                .get("buckets")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(bucket_from_json)
                .collect(),
        })
        .filter(|group| !group.buckets.is_empty())
        .collect()
}

fn bucket_from_json(bucket: &Value) -> Option<Bucket> {
    let remaining = bucket
        .get("remaining_fraction")
        .and_then(Value::as_f64)
        .or_else(|| {
            bucket
                .get("used_fraction")
                .and_then(Value::as_f64)
                .map(|used| 1.0 - used)
        })?;
    Some(Bucket {
        window: text_field(bucket, "window")
            .or_else(|| text_field(bucket, "id"))
            .map(|window| normalize_window(&window)),
        remaining,
        resets_at: text_field(bucket, "reset_time").and_then(|t| parse_time(&t)),
    })
}

/// Lines like `Gemini Models\tFive Hour Limit Remaining\t100%\t2026-09-30T09:09:04Z`.
fn groups_from_text(text: &str) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
        let [group, name, percent, reset, ..] = fields[..] else {
            continue;
        };
        let Some(remaining) = percent
            .strip_suffix('%')
            .and_then(|p| p.trim().parse::<f64>().ok())
        else {
            continue;
        };
        let bucket = Bucket {
            window: window_from_label(name),
            remaining: remaining / 100.0,
            resets_at: parse_time(reset),
        };
        match groups.iter_mut().find(|g| g.name == group) {
            Some(existing) => existing.buckets.push(bucket),
            None => groups.push(Group {
                name: group.to_owned(),
                buckets: vec![bucket],
            }),
        }
    }
    groups
}

fn normalize_window(window: &str) -> String {
    match window.to_ascii_lowercase().as_str() {
        "5h" | "five_hour" | "five-hour" => "five_hour".to_owned(),
        "weekly" | "week" | "7d" => "weekly".to_owned(),
        "daily" | "day" | "1d" => "daily".to_owned(),
        other => other.to_owned(),
    }
}

fn window_from_label(label: &str) -> Option<String> {
    let label = label.to_ascii_lowercase();
    if label.contains("five hour") || label.contains("5-hour") || label.contains("5 hour") {
        Some("five_hour".to_owned())
    } else if label.contains("weekly") {
        Some("weekly".to_owned())
    } else if label.contains("daily") {
        Some("daily".to_owned())
    } else {
        None
    }
}

/// agy may print notices before the JSON, so the last line that parses as an object counts,
/// or the whole output when it's pretty-printed.
fn last_json_object(stdout: &str) -> Option<Value> {
    let whole = serde_json::from_str::<Value>(stdout.trim()).ok();
    whole.filter(Value::is_object).or_else(|| {
        stdout
            .lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .find(Value::is_object)
    })
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn parse_time(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text.trim(), &Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    fn payload(groups: Value) -> String {
        json!({
            "conversation_id": "",
            "status": "SUCCESS",
            "response": "",
            "command": {"name": "usage", "data": {"groups": groups}},
        })
        .to_string()
    }

    #[test]
    fn the_probe_spends_nothing_and_keeps_the_key() {
        let spec = quota_probe_command("agy", Path::new("/work"), None);
        assert_eq!(spec.args, ["-p", "/usage", "--output-format", "json"]);
        assert_eq!(spec.cwd, Path::new("/work"));
        assert!(spec.stdin.is_none() && spec.files.is_empty());
        assert!(!spec.env_remove.iter().any(|v| v == API_KEY_ENV));
        assert!(spec.env_remove.iter().any(|v| v == "ANTHROPIC_API_KEY"));
        assert_eq!(spec.env_set, [(AUTO_UPDATE_ENV.into(), "true".into())]);
    }

    #[test]
    fn the_probe_names_the_sessions_model() {
        let spec = quota_probe_command("agy", Path::new("/work"), Some("gemini-3.6-flash-low"));
        assert_eq!(
            spec.args,
            [
                "-p",
                "/usage",
                "--output-format",
                "json",
                "--model",
                "gemini-3.6-flash-low"
            ]
        );
        let odd = quota_probe_command("agy", Path::new("/work"), Some("--yolo"));
        assert_eq!(odd.args, ["-p", "/usage", "--output-format", "json"]);
    }

    #[test]
    fn the_gemini_group_decides_and_its_tightest_window_wins() {
        let stdout = payload(json!([
            {"name": "Claude and GPT models", "buckets": [
                {"window": "5h", "remaining_fraction": 0.0, "reset_time": "2026-09-30T09:00:00Z"},
            ]},
            {"name": "Gemini Models", "buckets": [
                {"window": "weekly", "remaining_fraction": 0.15, "reset_time": "2026-10-06T02:52:44Z"},
                {"window": "5h", "remaining_fraction": 0.5, "reset_time": "2026-09-30T09:09:04Z"},
            ]},
        ]));
        let reading = parse_usage("antigravity-cli", &stdout).unwrap();
        assert_eq!(reading.vendor, VendorId("antigravity-cli".into()));
        assert_eq!(reading.status, QuotaStatus::Warning);
        assert_eq!(reading.window.as_deref(), Some("weekly"));
        assert!((reading.used.unwrap() - 0.85).abs() < 1e-9);
        assert_eq!(reading.resets_at, Some(datetime!(2026-10-06 02:52:44 UTC)));
        assert_eq!(reading.source, QuotaSource::UsageProbe);
        assert_eq!(
            reading.detail.unwrap()["groups"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn an_empty_window_is_rejected_and_a_fresh_one_allowed() {
        let empty = payload(json!([{"name": "Gemini Models", "buckets": [
            {"window": "5h", "remaining_fraction": 0, "reset_time": "2026-09-30T09:09:04Z"},
        ]}]));
        let reading = parse_usage("antigravity-cli", &empty).unwrap();
        assert_eq!(reading.status, QuotaStatus::Rejected);
        assert_eq!(reading.window.as_deref(), Some("five_hour"));
        assert_eq!(reading.used, Some(1.0));

        let fresh = payload(json!([{"name": "Gemini Models", "buckets": [
            {"window": "weekly", "remaining_fraction": 1},
        ]}]));
        let reading = parse_usage("antigravity-cli", &fresh).unwrap();
        assert_eq!(reading.status, QuotaStatus::Allowed);
        assert_eq!(reading.used, Some(0.0));
        assert!(reading.resets_at.is_none());
    }

    #[test]
    fn without_a_gemini_group_the_tightest_bucket_counts() {
        let stdout = payload(json!([
            {"name": "Team pool", "buckets": [{"id": "pool-daily", "remaining_fraction": 0.4}]},
            {"name": "Other", "buckets": [{"window": "daily", "remaining_fraction": 0.9}]},
        ]));
        let reading = parse_usage("antigravity-cli", &stdout).unwrap();
        assert_eq!(reading.window.as_deref(), Some("pool-daily"));
        assert!((reading.used.unwrap() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn the_response_text_is_the_fallback() {
        let stdout = json!({
            "status": "SUCCESS",
            "response": "Gemini Models\tWeekly Limit Remaining\t40%\t2026-10-06T02:52:44Z\n\
                         Gemini Models\tFive Hour Limit Remaining\t100%\t2026-09-30T09:09:04Z\n\
                         Claude and GPT models\tWeekly Limit Remaining\t0%\t2026-10-07T04:09:04Z\n",
        })
        .to_string();
        let reading = parse_usage("antigravity-cli", &stdout).unwrap();
        assert_eq!(reading.window.as_deref(), Some("weekly"));
        assert!((reading.used.unwrap() - 0.6).abs() < 1e-9);
        assert_eq!(reading.status, QuotaStatus::Allowed);
    }

    #[test]
    fn notices_before_the_json_are_skipped() {
        let body = payload(json!([{"name": "Gemini Models", "buckets": [
            {"window": "5h", "remaining_fraction": 0.1},
        ]}]));
        let stdout = format!("Update available\n{body}\n");
        let reading = parse_usage("antigravity-cli", &stdout).unwrap();
        assert_eq!(reading.status, QuotaStatus::Warning);
    }

    #[test]
    fn garbage_errors_and_empty_payloads_are_unknown() {
        for stdout in [
            "",
            "not json",
            "[1,2]",
            r#"{"status":"ERROR","error":"authentication required"}"#,
            r#"{"status":"SUCCESS","response":"no quota here"}"#,
            r#"{"status":"SUCCESS","command":{"data":{"groups":[{"name":"Gemini","buckets":[{"window":"5h"}]}]}}}"#,
        ] {
            assert!(parse_usage("antigravity-cli", stdout).is_none(), "{stdout}");
        }
    }
}
