//! Claude Code's `rate_limit_event`, sent on a subscription login whenever the limit state
//! changes. API-key sessions get their limit signal from `system/api_retry` instead.

use argonauts_core::base::names::AgentId;
use argonauts_core::models::limits::{PlanUsageReading, PlanUsageSource, PlanUsageStatus};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::parser::json::{bool_at, str_at, string_at};

/// `info` is the event's `rate_limit_info`. `None` when it has no status to normalize.
pub fn reading(info: &Value, observed_at: OffsetDateTime) -> Option<PlanUsageReading> {
    let status = status(info)?;
    // 2.1.284 sends utilization per window under `unifiedWindows` rather than at the top, and
    // `rateLimitType` names the window that changed, which isn't always the fullest. A model's
    // own bucket says how full the account is only when it's what turned a request down.
    let named = string_at(info, "rateLimitType");
    let own_bucket = named
        .as_deref()
        .is_some_and(|w| !ACCOUNT_WINDOWS.contains(&w));
    let (window, used, resets_at) = match fullest_window(info) {
        Some(fullest) if status != PlanUsageStatus::Rejected || !own_bucket => fullest,
        _ => {
            let in_windows = named
                .as_deref()
                .and_then(|w| info.get("unifiedWindows")?.get(w));
            let resets_at = info
                .get("resetsAt")
                .and_then(Value::as_i64)
                .or_else(|| in_windows?.get("resetsAt")?.as_i64());
            let used = info
                .get("utilization")
                .and_then(Value::as_f64)
                .or_else(|| in_windows?.get("utilization")?.as_f64());
            (named, used, resets_at)
        }
    };
    let used = used.map(|u| u.clamp(0.0, 1.0));
    let resets_at = resets_at.and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok());
    Some(PlanUsageReading {
        agent_id: AgentId("claude-code".to_owned()),
        status,
        used,
        resets_at,
        window,
        source: PlanUsageSource::RateLimitEvent,
        observed_at,
        detail: Some(info.clone()),
        wrap_up: false,
    })
}

fn status(info: &Value) -> Option<PlanUsageStatus> {
    Some(match str_at(info, "status")? {
        "allowed" => PlanUsageStatus::Allowed,
        "allowed_warning" => PlanUsageStatus::Warning,
        "rejected" => PlanUsageStatus::Rejected,
        _ => PlanUsageStatus::Unknown,
    })
}

/// The account's other windows, one reading each, beside the one [`reading`] gave, so every
/// window shows on its own. The window Claude Code names carries the event's status, and the
/// rest allow. A model's own bucket stays in the detail: it may pass 100% while the account
/// still allows, and would read as the fullest.
pub fn other_windows(info: &Value, headline: &PlanUsageReading) -> Vec<PlanUsageReading> {
    let Some(windows) = info.get("unifiedWindows").and_then(Value::as_object) else {
        return Vec::new();
    };
    let named = str_at(info, "rateLimitType");
    windows
        .iter()
        .filter(|(name, _)| ACCOUNT_WINDOWS.contains(&name.as_str()))
        .filter(|(name, _)| headline.window.as_deref() != Some(name.as_str()))
        .filter_map(|(name, window)| {
            let used = window.get("utilization")?.as_f64()?.clamp(0.0, 1.0);
            let resets_at = window
                .get("resetsAt")
                .and_then(Value::as_i64)
                .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok());
            let status = if named == Some(name.as_str()) {
                status(info).unwrap_or(PlanUsageStatus::Unknown)
            } else {
                PlanUsageStatus::Allowed
            };
            Some(PlanUsageReading {
                agent_id: AgentId("claude-code".to_owned()),
                status,
                used: Some(used),
                resets_at,
                window: Some(name.clone()),
                source: PlanUsageSource::RateLimitEvent,
                observed_at: headline.observed_at,
                detail: None,
                wrap_up: false,
            })
        })
        .collect()
}

/// The account's own limits. The other windows are a model's own weekly bucket, such as
/// `seven_day_overage_included` (the "Fable limit"), which may pass 100% while the account
/// still allows.
const ACCOUNT_WINDOWS: &[&str] = &["five_hour", "seven_day"];

/// The account's `unifiedWindows` entry with the highest utilization, the later reset on a tie,
/// as its name, utilization, and reset in Unix seconds.
fn fullest_window(info: &Value) -> Option<(Option<String>, Option<f64>, Option<i64>)> {
    let windows = info.get("unifiedWindows")?.as_object()?;
    let (name, used, resets_at) = windows
        .iter()
        .filter(|(name, _)| ACCOUNT_WINDOWS.contains(&name.as_str()))
        .filter_map(|(name, window)| {
            let used = window.get("utilization")?.as_f64()?;
            let resets_at = window.get("resetsAt").and_then(Value::as_i64);
            Some((name, used, resets_at))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)))?;
    Some((Some(name.clone()), Some(used), resets_at))
}

/// The window Claude Code names as the limit is one of the account's, and it's used up.
fn limiting_window_full(info: &Value) -> bool {
    let Some(window) = str_at(info, "rateLimitType").filter(|w| ACCOUNT_WINDOWS.contains(w)) else {
        return false;
    };
    let used = info.get("utilization").and_then(Value::as_f64).or_else(|| {
        info.get("unifiedWindows")?
            .get(window)?
            .get("utilization")?
            .as_f64()
    });
    used.is_some_and(|used| used >= 1.0)
}

/// The start of the notice Claude Code shows while it finishes on its allowance.
pub const WRAP_UP_NOTICE: &str = "Usage limit reached · wrapping up";

/// Claude Code's wrap-up allowance. At a limit mid-task it tells Claude to
/// finish the current step and keeps going on a small allowance, so a `rejected` reading no
/// longer ends the turn by itself. Any of three signals means wrapping up: a `rejected` reading
/// followed by more of the same turn, a window at 100% while the status still allows, or Claude
/// Code's own notice. The usage-limit error waits for the turn's end, and comes only if the turn
/// fails or nothing followed the reading.
#[derive(Debug, Default)]
pub struct LimitTracker {
    pending: Option<Pending>,
}

#[derive(Debug)]
struct Pending {
    reading: PlanUsageReading,
    wrapping_up: bool,
}

impl LimitTracker {
    /// `reading` as it should be reported: a full window that still allows becomes a wrap-up,
    /// out for new work while this turn finishes.
    pub fn observe(&mut self, mut reading: PlanUsageReading) -> PlanUsageReading {
        let overage = reading
            .detail
            .as_ref()
            .is_some_and(|detail| bool_at(detail, "isUsingOverage"));
        let full = reading.detail.as_ref().is_some_and(limiting_window_full);
        let wrapping_up = self.pending.as_ref().is_some_and(|p| p.wrapping_up);
        match reading.status {
            _ if overage => self.pending = None,
            PlanUsageStatus::Rejected => {
                reading.wrap_up = wrapping_up;
                self.hold(&reading, wrapping_up);
            }
            PlanUsageStatus::Allowed | PlanUsageStatus::Warning if full => {
                reading.status = PlanUsageStatus::Rejected;
                reading.wrap_up = true;
                self.hold(&reading, true);
            }
            PlanUsageStatus::Allowed | PlanUsageStatus::Warning => self.pending = None,
            PlanUsageStatus::Unknown => {}
        }
        reading
    }

    /// More of the turn came: a limit read before it was a wrap-up. Reported once.
    pub fn went_on(&mut self, now: OffsetDateTime) -> Option<PlanUsageReading> {
        let pending = self.pending.as_mut().filter(|p| !p.wrapping_up)?;
        pending.wrapping_up = true;
        pending.reading.wrap_up = true;
        pending.reading.observed_at = now;
        Some(pending.reading.clone())
    }

    /// Claude Code said it's wrapping up, with or without a reading before. `line` is the
    /// notice, kept as the reading's detail.
    pub fn notice(&mut self, line: &Value, now: OffsetDateTime) -> Option<PlanUsageReading> {
        if self.pending.is_some() {
            return self.went_on(now);
        }
        let reading = PlanUsageReading {
            agent_id: AgentId("claude-code".to_owned()),
            status: PlanUsageStatus::Rejected,
            used: Some(1.0),
            resets_at: None,
            window: None,
            source: PlanUsageSource::RateLimitEvent,
            observed_at: now,
            detail: Some(line.clone()),
            wrap_up: true,
        };
        self.hold(&reading, true);
        Some(reading)
    }

    /// The turn ended. A wrap-up's reading comes back as plain `rejected`, out until the reset,
    /// and the usage-limit message comes when the turn stopped on the limit instead.
    pub fn turn_ended(
        &mut self,
        failed: bool,
        now: OffsetDateTime,
    ) -> (Option<PlanUsageReading>, Option<String>) {
        let Some(Pending {
            mut reading,
            wrapping_up,
        }) = self.pending.take()
        else {
            return (None, None);
        };
        let message = if failed || !wrapping_up {
            usage_limit_message(&reading)
        } else {
            None
        };
        let out = wrapping_up.then(|| {
            reading.wrap_up = false;
            reading.observed_at = now;
            reading
        });
        (out, message)
    }

    /// The usage-limit message, when a limit is waiting at a stream that ended without a result.
    pub fn unfinished(&self) -> Option<String> {
        usage_limit_message(&self.pending.as_ref()?.reading)
    }

    fn hold(&mut self, reading: &PlanUsageReading, wrapping_up: bool) {
        self.pending = Some(Pending {
            reading: reading.clone(),
            wrapping_up,
        });
    }
}

/// The usage-limit message for a rejected reading, unless paid overage is carrying the session.
pub fn usage_limit_message(reading: &PlanUsageReading) -> Option<String> {
    let detail = reading.detail.as_ref()?;
    if reading.status != PlanUsageStatus::Rejected || bool_at(detail, "isUsingOverage") {
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
        assert_eq!(warning.status, PlanUsageStatus::Warning);
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

    /// 2.1.289 also lists a model's own weekly bucket, such as the "Fable limit", which may
    /// pass 100% while the account allows. It isn't how full Claude Code is, and it's no
    /// wrap-up.
    #[test]
    fn a_models_own_bucket_past_full_is_no_limit_of_the_account() {
        for named in [None, Some("five_hour"), Some("seven_day_overage_included")] {
            let mut info = json!({"status": "allowed",
                "unifiedWindows": {"five_hour": {"utilization": 0.3, "resetsAt": 1790751000},
                                   "seven_day": {"utilization": 0.4, "resetsAt": 1790820000},
                                   "seven_day_overage_included": {"utilization": 1.2,
                                                                  "resetsAt": 1791000000}}});
            if let Some(named) = named {
                info["rateLimitType"] = json!(named);
            }
            let read = reading(&info, NOW).unwrap();
            assert_eq!(read.window.as_deref(), Some("seven_day"), "{named:?}");
            assert_eq!(read.used, Some(0.4), "{named:?}");
            let seen = LimitTracker::default().observe(read);
            assert_eq!(seen.status, PlanUsageStatus::Allowed, "{named:?}");
            assert!(!seen.wrap_up, "{named:?}");
        }
        // When that bucket is what turned a request down, its reset is the one that counts.
        let fable_out = json!({"status": "rejected", "rateLimitType": "seven_day_overage_included",
            "resetsAt": 1791000000,
            "unifiedWindows": {"five_hour": {"utilization": 0.3, "resetsAt": 1790751000},
                               "seven_day_overage_included": {"utilization": 1.0,
                                                              "resetsAt": 1791000000}}});
        let out = reading(&fable_out, NOW).unwrap();
        assert_eq!(out.window.as_deref(), Some("seven_day_overage_included"));
        assert_eq!(out.resets_at.unwrap().unix_timestamp(), 1791000000);
    }

    /// The account's window Claude Code names, full while it still allows: a wrap-up, read at
    /// the top level or in its window.
    #[test]
    fn the_named_account_window_full_while_allowed_is_a_wrap_up() {
        for info in [
            json!({"status": "allowed", "rateLimitType": "seven_day", "utilization": 1.0}),
            json!({"status": "allowed_warning", "rateLimitType": "five_hour",
                   "unifiedWindows": {"five_hour": {"utilization": 1.04, "resetsAt": 1790751000}}}),
        ] {
            let seen = LimitTracker::default().observe(reading(&info, NOW).unwrap());
            assert_eq!(seen.status, PlanUsageStatus::Rejected, "{info}");
            assert!(seen.wrap_up, "{info}");
        }
    }

    #[test]
    fn rejected_means_a_usage_limit_unless_overage_pays() {
        let rejected = json!({"status": "rejected", "resetsAt": 1790678400,
                              "rateLimitType": "five_hour"});
        let limited = reading(&rejected, NOW).unwrap();
        assert_eq!(limited.status, PlanUsageStatus::Rejected);
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
        assert_eq!(future.status, PlanUsageStatus::Unknown);
        assert_eq!(future.used, Some(1.0));
        assert!(future.window.is_none() && future.resets_at.is_none());
    }
}
