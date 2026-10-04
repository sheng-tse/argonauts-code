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
use crate::settings::handoff::{FallbackEntry, HandoffStrategy};
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
    fn observe(&self, reading: QuotaReading);
    fn get(&self, vendor: &VendorId) -> Option<QuotaReading>;
    fn all(&self) -> Vec<QuotaReading>;
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
    /// The first that passes its gates and review wins.
    Race {
        to: Vec<WorkerRef>,
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
    /// Apply `budget.on_hit`.
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
            candidates: self.candidates(query),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shortlist {
    pub strategy: HandoffStrategy,
    /// Attempts at once for a race.
    pub race_count: u32,
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
