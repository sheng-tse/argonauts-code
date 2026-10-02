//! Claude Code's `rate_limit_event`, sent on a subscription login whenever the limit state
//! changes. API-key sessions get their limit signal from `system/api_retry` instead.

use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::VendorId;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::json::{bool_at, str_at, string_at};

/// `info` is the event's `rate_limit_info`. `None` when it has no status to normalize.
pub fn reading(info: &Value, observed_at: OffsetDateTime) -> Option<QuotaReading> {
    let status = match str_at(info, "status")? {
        "allowed" => QuotaStatus::Allowed,
        "allowed_warning" => QuotaStatus::Warning,
        "rejected" => QuotaStatus::Rejected,
        _ => QuotaStatus::Unknown,
    };
    // 2.1.284 sends utilization per window under `unifiedWindows` rather than at the top, and
    // `rateLimitType` names the window that changed, which isn't always the fullest.
    let (window, used, resets_at) = fullest_window(info).unwrap_or_else(|| {
        let window = string_at(info, "rateLimitType");
        let named = window
            .as_deref()
            .and_then(|w| info.get("unifiedWindows")?.get(w));
        let resets_at = info
            .get("resetsAt")
            .and_then(Value::as_i64)
            .or_else(|| named?.get("resetsAt")?.as_i64());
        let used = info.get("utilization").and_then(Value::as_f64);
        (window, used, resets_at)
    });
    let used = used.map(|u| u.clamp(0.0, 1.0));
    let resets_at = resets_at.and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok());
    Some(QuotaReading {
        vendor: VendorId("claude-code".to_owned()),
        status,
        used,
        resets_at,
        window,
        source: QuotaSource::RateLimitEvent,
        observed_at,
        detail: Some(info.clone()),
    })
}

/// The `unifiedWindows` entry with the highest utilization, the later reset on a tie, as its
/// name, utilization, and reset in Unix seconds.
fn fullest_window(info: &Value) -> Option<(Option<String>, Option<f64>, Option<i64>)> {
    let windows = info.get("unifiedWindows")?.as_object()?;
    let (name, used, resets_at) = windows
        .iter()
        .filter_map(|(name, window)| {
            let used = window.get("utilization")?.as_f64()?;
            let resets_at = window.get("resetsAt").and_then(Value::as_i64);
            Some((name, used, resets_at))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)))?;
    Some((Some(name.clone()), Some(used), resets_at))
}

/// The usage-limit message for a rejected reading, unless paid overage is carrying the session.
pub fn usage_limit_message(reading: &QuotaReading) -> Option<String> {
    let detail = reading.detail.as_ref()?;
    if reading.status != QuotaStatus::Rejected || bool_at(detail, "isUsingOverage") {
        return None;
    }
    let window = reading
        .window
        .as_deref()
        .map(|w| format!(" ({w})"))
        .unwrap_or_default();
    let reset = reading
        .resets_at
        .and_then(|at| at.format(&Rfc3339).ok())
        .map(|at| format!(", resets at {at}"))
        .unwrap_or_default();
    Some(format!("Claude usage limit reached{window}{reset}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-30 12:00 UTC);

    #[test]
    fn top_level_and_per_window_utilization() {
        let flat = json!({"status": "allowed_warning", "utilization": 0.91,
                          "resetsAt": 1790678400, "rateLimitType": "seven_day"});
        let warning = reading(&flat, NOW).unwrap();
        assert_eq!(warning.status, QuotaStatus::Warning);
        assert_eq!(warning.used, Some(0.91));
        assert_eq!(warning.window.as_deref(), Some("seven_day"));
        assert_eq!(warning.resets_at.unwrap().unix_timestamp(), 1790678400);
        assert_eq!(warning.observed_at, NOW);
        assert_eq!(warning.detail.as_ref(), Some(&flat));
        assert!(usage_limit_message(&warning).is_none());

        let windows = json!({"status": "allowed", "rateLimitType": "five_hour",
                             "unifiedWindows": {"five_hour": {"utilization": 0.18, "resetsAt": 1790678400},
                                                "seven_day": {"utilization": 0.05, "resetsAt": 1790820000}}});
        let allowed = reading(&windows, NOW).unwrap();
        assert_eq!(allowed.used, Some(0.18));
        assert_eq!(allowed.window.as_deref(), Some("five_hour"));
        assert_eq!(allowed.resets_at.unwrap().unix_timestamp(), 1790678400);
    }

    #[test]
    fn the_fullest_window_counts() {
        let fuller_week = json!({"status": "allowed", "rateLimitType": "five_hour",
                                 "resetsAt": 1790751000, "utilization": 0.55,
                                 "unifiedWindows": {"five_hour": {"utilization": 0.55, "resetsAt": 1790751000},
                                                    "seven_day": {"utilization": 0.62, "resetsAt": 1790820000}}});
        let week = reading(&fuller_week, NOW).unwrap();
        assert_eq!(week.window.as_deref(), Some("seven_day"));
        assert_eq!(week.used, Some(0.62));
        assert_eq!(week.resets_at.unwrap().unix_timestamp(), 1790820000);
        assert_eq!(week.detail.as_ref(), Some(&fuller_week));

        // Both full: nothing runs until the later reset.
        let both_full = json!({"status": "rejected", "rateLimitType": "five_hour",
                               "unifiedWindows": {"five_hour": {"utilization": 1.0, "resetsAt": 1790751000},
                                                  "seven_day": {"utilization": 1.0, "resetsAt": 1790820000},
                                                  "opus": {"resetsAt": 1790900000}}});
        let full = reading(&both_full, NOW).unwrap();
        assert_eq!(full.window.as_deref(), Some("seven_day"));
        assert_eq!(full.resets_at.unwrap().unix_timestamp(), 1790820000);

        // Windows without a utilization leave the top-level fields in charge.
        let bare = json!({"status": "allowed", "rateLimitType": "five_hour", "utilization": 0.3,
                          "unifiedWindows": {"five_hour": {"resetsAt": 1790751000}}});
        let top = reading(&bare, NOW).unwrap();
        assert_eq!(top.window.as_deref(), Some("five_hour"));
        assert_eq!(top.used, Some(0.3));
        assert_eq!(top.resets_at.unwrap().unix_timestamp(), 1790751000);
    }

    #[test]
    fn rejected_means_a_usage_limit_unless_overage_pays() {
        let rejected = json!({"status": "rejected", "resetsAt": 1790678400,
                              "rateLimitType": "five_hour"});
        let limited = reading(&rejected, NOW).unwrap();
        assert_eq!(limited.status, QuotaStatus::Rejected);
        assert_eq!(
            usage_limit_message(&limited).as_deref(),
            Some("Claude usage limit reached (five_hour), resets at 2026-09-29T10:40:00Z")
        );
        let overage = json!({"status": "rejected", "isUsingOverage": true});
        assert!(usage_limit_message(&reading(&overage, NOW).unwrap()).is_none());
    }

    #[test]
    fn odd_payloads() {
        assert!(reading(&json!({"utilization": 0.5}), NOW).is_none());
        let future = reading(&json!({"status": "throttled", "utilization": 7}), NOW).unwrap();
        assert_eq!(future.status, QuotaStatus::Unknown);
        assert_eq!(future.used, Some(1.0));
        assert!(future.window.is_none() && future.resets_at.is_none());
    }
}
