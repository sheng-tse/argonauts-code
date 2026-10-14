//! Quota, vendor health, recovery after a stop, handoffs, and gates, all decided in code.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::broadcast;

use crate::events::card::{CardSpec, CardState, GateOutcome};
use crate::events::run::ParkReason;
use crate::events::session::SessionEnd;
use crate::events::{RunEvent, VendorEvent};
use crate::future::BoxFuture;
use crate::handoff::CheckpointTag;
use crate::names::{RoleName, SessionKey, VendorId, WorkerRef};
use crate::settings::budget::{BudgetAmount, OnHit};
use crate::settings::handoff::{FallbackEntry, HandoffStrategy, Pick};
use crate::steps::card::CardReport;
use crate::worker::{SandboxProfile, SessionLimits};
use crate::{CardId, ErrorCategory, RetryReason, RunId, TodoItem};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaReading {
    pub vendor: VendorId,
    pub status: QuotaStatus,
    /// Share of the current window used, 0 to 1, where the vendor reports it.
    pub used: Option<f64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resets_at: Option<OffsetDateTime>,
    /// Such as `five_hour` or `weekly`.
    pub window: Option<String>,
    pub source: QuotaSource,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    /// The vendor's payload, for the raw view.
    pub detail: Option<Value>,
    /// Out of quota, but the running session finishes on a small allowance, as Claude Code
    /// does at a five-hour limit. Nothing hands its work off, and new work goes elsewhere.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrap_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Allowed,
    Warning,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaSource {
    /// Claude Code's `rate_limit_event`.
    RateLimitEvent,
    /// `agy -p "/usage" --output-format json`, which spends nothing.
    UsageProbe,
    /// A usage-limit error that says when to retry (Codex exec).
    ErrorMessage,
    /// The Codex app server's rate-limit read.
    AppServer,
    /// An endpoint's HTTP headers or errors.
    Http,
    /// Claude Code's `get_usage` control request, asked before any message.
    GetUsage,
}

/// What a quota probe read: every window the vendor reports, and its plan when it names one.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaAnswer {
    pub windows: Vec<QuotaReading>,
    pub plan: Option<String>,
    /// The whole reading with the account it was read for, where the CLI names one.
    pub live: Option<crate::plan_usage::LiveUsage>,
    /// The CLI's own words when it answered with an error instead of its usage.
    pub failed: Option<String>,
}

impl QuotaAnswer {
    /// An error in place of a reading. It names no account, so it leaves every reading kept.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            windows: Vec::new(),
            plan: None,
            live: None,
            failed: Some(message.into()),
        }
    }

    /// The window that binds soonest, which is what one number per vendor shows.
    pub fn binding(&self) -> Option<&QuotaReading> {
        binding_reading(&self.windows)
    }
}

/// A vendor's readings, the newest of each window. A reading that names no window speaks for
/// the vendor as a whole, so it replaces every window, and the next windowed one replaces it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QuotaWindows(Vec<QuotaReading>);

impl QuotaWindows {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn readings(&self) -> &[QuotaReading] {
        &self.0
    }

    /// Keeps `reading` unless its window has a newer one. Says whether it was kept.
    pub fn observe(&mut self, reading: QuotaReading) -> bool {
        let newer = self
            .0
            .iter()
            .all(|known| !same_window(known, &reading) || known.observed_at <= reading.observed_at);
        if newer {
            self.put(reading);
        }
        newer
    }

    /// Keeps `reading` whatever its age.
    pub fn put(&mut self, reading: QuotaReading) {
        if reading.window.is_none() {
            self.0.clear();
        } else {
            self.0
                .retain(|known| known.window.is_some() && known.window != reading.window);
        }
        self.0.push(reading);
    }

    /// The window that binds soonest, as [`binding_reading`] picks it.
    pub fn binding(&self) -> Option<&QuotaReading> {
        binding_reading(&self.0)
    }
}

fn same_window(a: &QuotaReading, b: &QuotaReading) -> bool {
    a.window.is_none() || b.window.is_none() || a.window == b.window
}

/// The reading that binds soonest: one that refused work, else the fullest. Between two as
/// full, the later reset is the one you'd wait for, then the newer reading.
pub fn binding_reading(readings: &[QuotaReading]) -> Option<&QuotaReading> {
    readings.iter().max_by(|a, b| {
        let refused = |r: &QuotaReading| r.status == QuotaStatus::Rejected;
        let used = |r: &QuotaReading| r.used.unwrap_or(-1.0);
        refused(a)
            .cmp(&refused(b))
            .then(used(a).total_cmp(&used(b)))
            .then(a.resets_at.cmp(&b.resets_at))
            .then(a.observed_at.cmp(&b.observed_at))
    })
}

/// Circuit-breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Healthy,
    /// New cards go elsewhere when another agent fits.
    Degraded,
    /// Its cards pause with work committed; new cards go elsewhere.
    Down,
    /// One test card goes to it.
    Testing,
}

/// Why a vendor reads healthy again after its test task succeeded: the one way back from an
/// outage that a check stands behind, where errors that stopped coming back say less.
pub const TEST_TASK_SUCCEEDED: &str = "the test task succeeded";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorHealth {
    pub vendor: VendorId,
    pub state: HealthState,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_test_at: Option<OffsetDateTime>,
    /// Why it's disabled for new cards, such as an untested CLI version.
    pub disabled: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HealthSignal {
    Retry {
        vendor: VendorId,
        session: SessionKey,
        reason: RetryReason,
    },
    Error {
        vendor: VendorId,
        session: SessionKey,
        category: ErrorCategory,
    },
    Success {
        vendor: VendorId,
        session: SessionKey,
    },
    /// Unknown events piled up or the startup check failed.
    AdapterBroken { vendor: VendorId, detail: String },
}

/// Quota across every run and the app, persisted in `~/.argonauts/vendors.json`.
pub trait QuotaBoard: Send + Sync {
    /// Keeps the reading as its vendor's for its window.
    fn observe(&self, reading: QuotaReading);
    /// The vendor's window that binds soonest, for what wants one number per vendor: the
    /// handoff, the planner, and the mode line's chip.
    fn get(&self, vendor: &VendorId) -> Option<QuotaReading>;
    /// One reading per vendor, as `get` gives it.
    fn all(&self) -> Vec<QuotaReading>;
    /// Every window the vendor reported, one reading each.
    fn windows(&self, vendor: &VendorId) -> Vec<QuotaReading> {
        self.get(vendor).into_iter().collect()
    }
    /// The account's plan, as the CLI names it.
    fn plan(&self, vendor: &VendorId) -> Option<String> {
        let _ = vendor;
        None
    }
    fn observe_plan(&self, vendor: &VendorId, plan: String) {
        let _ = (vendor, plan);
    }
    /// Keeps a live reading as its account's. A reading of another account than the last
    /// one's means the CLI was signed in again, and only the new account's numbers show.
    fn observe_live(&self, reading: crate::plan_usage::LiveUsage) {
        let _ = reading;
    }
    /// The CLI's newest live reading, for the account it was last signed in as. What shows
    /// it checks [`LiveUsage::shown_at`](crate::plan_usage::LiveUsage::shown_at) first.
    fn live(&self, vendor: &VendorId) -> Option<crate::plan_usage::LiveUsage> {
        let _ = vendor;
        None
    }
    /// Why this process's last live read of the CLI got no reading, in words for the screen.
    /// `None` once a reading lands.
    fn failed(&self, vendor: &VendorId) -> Option<String> {
        let _ = vendor;
        None
    }
    /// When this process last had cause to doubt the CLI's live reading: a read that failed,
    /// or a sign-in or sign-out since. A reading from before then may be another account's,
    /// so it no longer stands as the CLI's own.
    fn doubted_at(&self, vendor: &VendorId) -> Option<OffsetDateTime> {
        let _ = vendor;
        None
    }
    /// The CLI was signed in or out, so its live reading may be another account's until it
    /// is read again.
    fn doubt(&self, vendor: &VendorId) {
        let _ = vendor;
    }
    /// Reads quota without spending any, where the vendor allows it (agy `/usage`).
    fn probe(&self, vendor: &VendorId) -> BoxFuture<'_, Option<QuotaReading>>;
}

/// Vendor health and the circuit breaker, shared like [`QuotaBoard`].
pub trait HealthBoard: Send + Sync {
    fn observe(&self, signal: HealthSignal);
    fn state(&self, vendor: &VendorId) -> VendorHealth;
    fn all(&self) -> Vec<VendorHealth>;
    fn subscribe(&self) -> broadcast::Receiver<VendorEvent>;
    fn test_result(&self, vendor: &VendorId, ok: bool);
}

#[derive(Debug, Clone)]
pub struct StopContext {
    pub run: RunId,
    pub card: CardSpec,
    pub state: CardState,
    pub role: RoleName,
    pub worker: WorkerRef,
    pub end: SessionEnd,
    /// Oldest first, one line each.
    pub attempts: Vec<String>,
    pub reassigned_once: bool,
    pub events: PathBuf,
    /// What the stopped worker's CLI says of its login, asked after a sign-in error: `Some(false)`
    /// when it says it's logged out, the only time a login is the fix.
    pub logged_in: Option<bool>,
}

/// What the engine does after a session stops for any reason but a clean finish.
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryAction {
    /// Treat the result as final.
    Proceed,
    ResumeSame {
        /// Such as the gate output or a nudge.
        message: String,
    },
    /// Same worker, fresh session, with a handoff note (context full).
    Rotate { packet: Box<HandoffPacket> },
    /// Another worker chosen by the rules (handoff mode `auto`, retry ladder).
    HandOff {
        to: WorkerRef,
        packet: Box<HandoffPacket>,
        /// Tag the committed work as this before the new worker starts.
        checkpoint: Option<CheckpointTag>,
    },
    /// Several workers at once, each in its own worktree, sharing the card's remaining budget.
    /// `pick` says how their results combine.
    Race {
        to: Vec<WorkerRef>,
        pick: Pick,
        packet: Box<HandoffPacket>,
        checkpoint: Option<CheckpointTag>,
    },
    /// Open a handoff checkpoint.
    Ask(Box<crate::events::vendor::HandoffPlan>),
    /// Keep the card in `NeedsDecision` until the vendor is back.
    Wait {
        vendor: VendorId,
        until: Option<OffsetDateTime>,
    },
    /// Apply `limits.when_reached`.
    BudgetHit(OnHit),
    /// Out of attempts.
    Escalate { reason: String },
    /// Nothing can continue anywhere.
    Park(ParkReason),
}

/// The vendor-neutral packet a new agent gets. Built by code from the log, at no token cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffPacket {
    pub card: CardSpec,
    pub goal_sha256: Option<String>,
    pub notes: Vec<String>,
    pub work: WorkSoFar,
    /// Files read and edited, commands run and their results, one line each.
    pub happened: Vec<String>,
    pub last_todo: Vec<TodoItem>,
    pub gates: Vec<GateOutcome>,
    pub why_stopped: String,
    /// The previous agent's own report, if it wrote one.
    pub last_words: Option<CardReport>,
    /// What the stopped session said last. Without a report, it is often the only word on
    /// where the work stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    pub budget_left: Option<BudgetAmount>,
    /// The old session as Markdown, written for `note+transcript` and `note+summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptPaths>,
    /// The summarizer's summary of that transcript, for `note+summary`. It goes into the note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Where a handoff's transcript was written. Code builds it from the card's events in the
/// run's log, so it reads the same for every vendor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPaths {
    /// `<card worktree>/.argonauts/handoff/<n>/transcript.md`, the copy the note names. Every
    /// harness can read it without extra flags, and the worktree's git exclude file keeps it
    /// out of commits.
    pub worktree: PathBuf,
    /// `runs/<run>/cards/<card>/handoff-<n>/transcript.md`, the run's own copy, kept for you.
    pub run: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkSoFar {
    /// `argonauts/<run>/<card>`.
    pub branch: String,
    pub commit: Option<String>,
    /// Against the card's base.
    pub stat: crate::DiffStat,
}

pub trait HandoffPlanner: Send + Sync {
    /// Valid candidates, best first: the role's rules (family, blindness), vendor health, and
    /// quota left. Each carries its quota, track record, and cost estimate.
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate>;
    /// Built from that card's own events (the engine filters them), so its cost stays small
    /// however long the run is.
    fn packet(&self, card: &CardSpec, events: &[RunEvent]) -> HandoffPacket;

    /// The candidates from the most specific replacement list for the card's role and the
    /// stopped vendor, with the strategy and race count that apply, both from
    /// [`HandoffSettings::applied`](crate::settings::handoff::HandoffSettings::applied).
    fn shortlist(&self, query: &CandidateQuery) -> Shortlist {
        Shortlist {
            strategy: HandoffStrategy::First,
            race_count: 1,
            pick: Pick::default_for(query.role),
            candidates: self.candidates(query),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shortlist {
    pub strategy: HandoffStrategy,
    /// How many fallbacks take the task at once (`together`).
    pub race_count: u32,
    /// How their results combine.
    pub pick: Pick,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateQuery {
    pub card: CardSpec,
    pub role: RoleName,
    pub stopped: WorkerRef,
    /// Such as the owner's for a reviewer.
    pub exclude_families: Vec<String>,
    pub fallback: Vec<FallbackEntry>,
    /// List every valid worker, not only the fallback list.
    pub all_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub worker: WorkerRef,
    /// 0 to 1, where known.
    pub quota_left: Option<f64>,
    /// Such as "62%", or "API key".
    pub quota_text: String,
    pub track_record: Option<TrackRecord>,
    pub cost: CostEstimate,
    /// Moving to a paid API needs your OK.
    pub needs_confirm: bool,
    pub may_run_out: bool,
}

/// A track record on similar cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackRecord {
    pub passed: u32,
    pub of: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "unit", content = "amount", rename_all = "snake_case")]
pub enum CostEstimate {
    Usd(f64),
    /// Share of a subscription window.
    Quota(f64),
    Free,
    Unknown,
}

/// Derived from the outcome log.
pub trait TrackRecords: Send + Sync {
    fn get(&self, worker: &WorkerRef, task_type: &str) -> Option<TrackRecord>;
}

pub trait LimitsService: Send + Sync {
    fn session_limits(&self, card: Option<&CardSpec>, role: RoleName) -> SessionLimits;
    fn after_stop<'a>(&'a self, context: &'a StopContext) -> BoxFuture<'a, RecoveryAction>;
    /// Whether every candidate for the remaining work is down or out of quota.
    fn all_out(&self, workers: &[WorkerRef]) -> Option<ParkReason>;
}

/// The environment is an allowlist: `PATH`, `LANG`, `TERM`, `TMPDIR`, `HOME` set to a temp
/// directory, and `CI=1`. No vendor key, and nothing named `*_KEY`, `*_TOKEN`, `*_SECRET`, or
/// `*_PASSWORD`, reaches a gate, since tests an agent edited can print their environment.
#[derive(Debug, Clone, PartialEq)]
pub struct GateRequest {
    pub run: RunId,
    pub card: CardId,
    pub name: String,
    /// An argument vector from protected config.
    pub command: Vec<String>,
    pub workdir: PathBuf,
    pub sandbox: SandboxProfile,
    pub timeout: Duration,
    /// A `gates.setup` command: runs once per worktree before the first gate, outside the
    /// agent's sandbox, with network for installs from the lockfile and install scripts off
    /// where the tool allows it.
    pub setup: bool,
}

/// Fails closed when the sandbox is missing.
pub trait GateRunner: Send + Sync {
    fn run(&self, request: GateRequest) -> BoxFuture<'_, GateOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn reading(window: Option<&str>, used: f64, minute: u8) -> QuotaReading {
        QuotaReading {
            vendor: VendorId("codex".into()),
            status: QuotaStatus::Allowed,
            used: Some(used),
            resets_at: None,
            window: window.map(str::to_owned),
            source: QuotaSource::AppServer,
            observed_at: datetime!(2026-10-04 12:00 UTC) + time::Duration::minutes(minute.into()),
            detail: None,
            wrap_up: false,
        }
    }

    fn windows_of(windows: &QuotaWindows) -> Vec<(Option<&str>, Option<f64>)> {
        windows
            .readings()
            .iter()
            .map(|r| (r.window.as_deref(), r.used))
            .collect()
    }

    #[test]
    fn each_window_keeps_its_newest_reading() {
        let mut windows = QuotaWindows::default();
        assert!(windows.observe(reading(Some("five_hour"), 0.5, 1)));
        assert!(windows.observe(reading(Some("weekly"), 0.4, 1)));
        assert!(windows.observe(reading(Some("five_hour"), 0.6, 2)));
        assert!(!windows.observe(reading(Some("five_hour"), 0.1, 0)));
        assert_eq!(
            windows_of(&windows),
            [(Some("weekly"), Some(0.4)), (Some("five_hour"), Some(0.6))]
        );
        assert_eq!(windows.binding().unwrap().used, Some(0.6));
    }

    #[test]
    fn a_reading_with_no_window_speaks_for_the_whole_vendor() {
        let mut windows = QuotaWindows::default();
        windows.observe(reading(Some("five_hour"), 0.5, 1));
        windows.observe(reading(Some("weekly"), 0.4, 1));
        windows.observe(reading(None, 1.0, 2));
        assert_eq!(windows_of(&windows), [(None, Some(1.0))]);
        windows.observe(reading(Some("weekly"), 0.2, 3));
        assert_eq!(windows_of(&windows), [(Some("weekly"), Some(0.2))]);
    }

    #[test]
    fn a_refused_window_binds_before_a_fuller_one() {
        let mut refused = reading(Some("weekly"), 0.3, 1);
        refused.status = QuotaStatus::Rejected;
        let readings = [reading(Some("five_hour"), 0.9, 1), refused];
        assert_eq!(
            binding_reading(&readings).unwrap().window.as_deref(),
            Some("weekly")
        );
    }
}
