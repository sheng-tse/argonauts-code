//! Each CLI's plan usage as the CLI itself reads it live, tied to the account it was signed in
//! as. A reading names its account, so one account's numbers never stand for another's.

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::limits::{QuotaBoard, QuotaReading, QuotaSource, QuotaStatus, binding_reading};
use crate::names::VendorId;

/// From this share of a window used, the CLI's own screens warn, and so does its reading.
pub const WARNING_FROM: f64 = 0.8;

/// A reading whose CLI didn't say who was signed in is shown only this long: past it, the
/// account may have changed with nothing to tell.
pub const UNNAMED_SHOWN_FOR: Duration = Duration::minutes(10);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveUsage {
    pub vendor: VendorId,
    /// Who the CLI was signed in as when it answered: an email, or the CLI's own account id.
    /// `None` when the CLI doesn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The plan as the CLI names it, such as `Max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// In the order the CLI's own usage screen lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<LiveWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<ExtraUsage>,
    /// What made up most of the usage, where the CLI says (Claude Code does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behaviors: Option<Behaviors>,
    /// The CLI refuses ordinary work whatever its windows say.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked: bool,
    pub source: QuotaSource,
    #[serde(with = "time::serde::rfc3339")]
    pub read_at: OffsetDateTime,
}

/// One plan window, named twice: short on a card, and in full as the CLI's own screen titles
/// it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveWindow {
    /// What stays the same from reading to reading, such as `five_hour` or `model:Fable`.
    pub key: String,
    /// Such as `session`, `week` or `Fable week`.
    pub label: String,
    /// Such as `Current session` or `Current week (all models)`.
    pub title: String,
    /// 0 to 1, where the CLI says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub resets_at: Option<OffsetDateTime>,
    /// Said before the reset, such as `5-hour` when the label leaves the window's length out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Shown on the CLI's own page but not on its card, which keeps one row for a group.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub page_only: bool,
}

/// Paid usage past the plan's limits, such as Claude Code's usage credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraUsage {
    pub enabled: bool,
    /// In the currency's own units, such as dollars.
    pub spent: f64,
    /// `None` when it has no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// Such as `USD`.
    pub currency: String,
    /// 0 to 1 of the cap, where the CLI says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
}

impl ExtraUsage {
    /// The share of the cap spent: the CLI's own figure, else what the amounts come to.
    pub fn share(&self) -> Option<f64> {
        self.used.or_else(|| {
            let limit = self.limit.filter(|limit| *limit > 0.0)?;
            Some((self.spent / limit).clamp(0.0, 1.0))
        })
    }
}

/// What made up the usage over the last day and the last week, each a share of it. The
/// shares overlap, so they don't add up to the whole.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Behaviors {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub day: Vec<Behavior>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub week: Vec<Behavior>,
    /// The rest of the CLI's report on the last day and the last week, where it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_report: Option<UsageReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub week_report: Option<UsageReport>,
}

/// What a CLI's own usage report counts over a span besides the behaviors: the requests and
/// sessions, and the share of the usage each agent, skill, plugin and MCP server took.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    pub requests: u64,
    pub sessions: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<UsageShare>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<UsageShare>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<UsageShare>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<UsageShare>,
}

/// One named part of the usage and its share, 0 to 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageShare {
    pub name: String,
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Behavior {
    /// The CLI's own name for it, such as `subagent_heavy`.
    pub key: String,
    /// 0 to 1.
    pub share: f64,
}

impl LiveUsage {
    /// An empty reading of `vendor`, made `read_at`.
    pub fn new(vendor: VendorId, source: QuotaSource, read_at: OffsetDateTime) -> Self {
        Self {
            vendor,
            account: None,
            plan: None,
            windows: Vec::new(),
            extra: None,
            behaviors: None,
            blocked: false,
            source,
            read_at,
        }
    }

    /// Each window as the quota board keeps it, for what wants one number per CLI: the bottom
    /// line, the handoff and the planner.
    pub fn quota_readings(&self) -> Vec<QuotaReading> {
        let mut readings: Vec<QuotaReading> = self
            .windows
            .iter()
            .map(|window| window.reading(self))
            .collect();
        if self.blocked
            && let Some(binding) = binding_reading(&readings).cloned()
            && let Some(reading) = readings.iter_mut().find(|r| **r == binding)
        {
            reading.status = QuotaStatus::Rejected;
        }
        readings
    }

    /// The window that binds soonest, as one number per CLI shows it.
    pub fn binding(&self) -> Option<QuotaReading> {
        binding_reading(&self.quota_readings()).cloned()
    }

    /// Whether a window it read has reset since, so the reading says nothing of it now.
    pub fn reset_passed(&self, now: OffsetDateTime) -> bool {
        self.windows
            .iter()
            .filter_map(|window| window.resets_at)
            .any(|at| self.read_at < at && at <= now)
    }

    /// Whether it may be shown as the CLI's reading now: one that names its account always,
    /// since the board keeps only the account last signed in; one that doesn't, only while
    /// fresh.
    pub fn shown_at(&self, now: OffsetDateTime) -> bool {
        self.account.is_some() || now - self.read_at < UNNAMED_SHOWN_FOR
    }

    /// Due a new read before it stops showing: a reading that names no account shows only
    /// while fresh, so it's read again a minute before then.
    pub fn fading_at(&self, now: OffsetDateTime) -> bool {
        self.account.is_none() && now - self.read_at >= UNNAMED_SHOWN_FOR - Duration::minutes(1)
    }

    /// Whether it stands as the CLI's own reading at `now`: it may be shown, and it was read
    /// after anything that cast doubt on it (see [`QuotaBoard::doubted_at`]).
    pub fn current_at(&self, now: OffsetDateTime, doubted_at: Option<OffsetDateTime>) -> bool {
        self.shown_at(now) && doubted_at.is_none_or(|at| self.read_at > at)
    }
}

/// The CLI's live reading while it stands as its own, as the Usage tab and the bottom line
/// both show it.
pub fn current_live(
    board: &dyn QuotaBoard,
    vendor: &VendorId,
    now: OffsetDateTime,
) -> Option<LiveUsage> {
    board
        .live(vendor)
        .filter(|live| live.current_at(now, board.doubted_at(vendor)))
}

impl LiveWindow {
    pub fn new(key: impl Into<String>, label: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            title: title.into(),
            used: None,
            resets_at: None,
            note: None,
            page_only: false,
        }
    }

    pub fn used(mut self, used: Option<f64>) -> Self {
        self.used = used.map(|used| used.clamp(0.0, 1.0));
        self
    }

    pub fn resets_at(mut self, at: Option<OffsetDateTime>) -> Self {
        self.resets_at = at;
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn page_only(mut self) -> Self {
        self.page_only = true;
        self
    }

    fn reading(&self, of: &LiveUsage) -> QuotaReading {
        let status = match self.used {
            Some(used) if used >= 1.0 => QuotaStatus::Rejected,
            Some(used) if used >= WARNING_FROM => QuotaStatus::Warning,
            Some(_) => QuotaStatus::Allowed,
            None => QuotaStatus::Unknown,
        };
        QuotaReading {
            vendor: of.vendor.clone(),
            status,
            used: self.used,
            resets_at: self.resets_at,
            window: Some(self.key.clone()),
            source: of.source,
            observed_at: of.read_at,
            detail: None,
            wrap_up: false,
        }
    }
}

/// `max` as `Max`: the plan as a screen names it.
pub fn plan_name(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("Claude ").unwrap_or(raw);
    let mut chars = raw.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const READ: OffsetDateTime = datetime!(2026-10-09 03:12 UTC);

    fn reading(windows: Vec<LiveWindow>) -> LiveUsage {
        LiveUsage {
            windows,
            ..LiveUsage::new(VendorId("codex".into()), QuotaSource::AppServer, READ)
        }
    }

    #[test]
    fn windows_become_quota_readings_and_the_fullest_binds() {
        let live = reading(vec![
            LiveWindow::new("five_hour", "5-hour", "5h limit").used(Some(0.2)),
            LiveWindow::new("weekly", "weekly", "Weekly limit").used(Some(0.85)),
        ]);
        let readings = live.quota_readings();
        assert_eq!(readings.len(), 2);
        assert_eq!(readings[1].status, QuotaStatus::Warning);
        assert_eq!(readings[0].observed_at, READ);
        assert_eq!(live.binding().unwrap().window.as_deref(), Some("weekly"));
    }

    #[test]
    fn a_blocked_reading_refuses_in_its_binding_window() {
        let mut live = reading(vec![
            LiveWindow::new("five_hour", "5-hour", "5h limit").used(Some(0.3)),
        ]);
        live.blocked = true;
        assert_eq!(live.binding().unwrap().status, QuotaStatus::Rejected);
    }

    #[test]
    fn a_reset_since_the_reading_is_noticed() {
        let live = reading(vec![
            LiveWindow::new("five_hour", "5-hour", "5h limit")
                .resets_at(Some(datetime!(2026-10-09 05:00 UTC))),
        ]);
        assert!(!live.reset_passed(datetime!(2026-10-09 04:59 UTC)));
        assert!(live.reset_passed(datetime!(2026-10-09 05:00 UTC)));
    }

    #[test]
    fn a_reading_with_no_account_is_shown_only_while_fresh() {
        let mut live = reading(Vec::new());
        assert!(live.shown_at(READ + Duration::minutes(9)));
        assert!(!live.shown_at(READ + Duration::minutes(10)));
        live.account = Some("someone@example.com".into());
        assert!(live.shown_at(READ + Duration::days(3)));
    }

    #[test]
    fn plan_names_read_as_a_screen_shows_them() {
        assert_eq!(plan_name("max").as_deref(), Some("Max"));
        assert_eq!(plan_name("Claude Max").as_deref(), Some("Max"));
        assert_eq!(plan_name("plus").as_deref(), Some("Plus"));
        assert_eq!(plan_name("  "), None);
    }

    #[test]
    fn extra_usage_shares_its_own_figure_or_the_amounts() {
        let extra = ExtraUsage {
            enabled: true,
            spent: 25.0,
            limit: Some(100.0),
            currency: "USD".into(),
            used: None,
        };
        assert_eq!(extra.share(), Some(0.25));
        let uncapped = ExtraUsage {
            limit: None,
            ..extra
        };
        assert_eq!(uncapped.share(), None);
    }
}
