//! In-memory, scripted fakes of the engine services, so the crew engine can be tested without
//! the real ones.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use argonauts_core::clock::Clock;
use argonauts_core::engine::{EngineCommand, EngineContext, EventSink, RepoContext, Services};
use argonauts_core::events::card::{CardSpec, GateKind, GateOutcome};
use argonauts_core::events::run::ParkReason;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::events::{RunEvent, RunEventKind, VendorEvent};
use argonauts_core::future::BoxFuture;
use argonauts_core::journal::JournalError;
use argonauts_core::limits::{
    Candidate, CandidateQuery, CostEstimate, GateRequest, GateRunner, HandoffPacket,
    HandoffPlanner, HealthBoard, HealthSignal, HealthState, LimitsService, QuotaBoard,
    QuotaReading, RecoveryAction, StopContext, TrackRecord, TrackRecords, VendorHealth, WorkSoFar,
};
use argonauts_core::names::{RoleName, SessionKey, VendorId, WorkerRef};
use argonauts_core::policy::{
    ApprovalPolicy, ApprovalsView, DecisionPoint, EvidencePacket, HardLimits, PolicyOutcome,
    UserDecision, Violation,
};
use argonauts_core::registry::{CostUnit, ModelEntry, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::settings::budget::OnHit;
use argonauts_core::settings::roles::HandoffMode;
use argonauts_core::steps::SchemaStepRunner;
use argonauts_core::worker::{SessionLimits, SessionSpec};
use argonauts_core::{CardId, DiffStat, ErrorCategory, RepoId, RunId};
use time::OffsetDateTime;
use time::macros::datetime;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::worker::FakeDirectory;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every fake service, with typed handles for scripting and inspecting them.
pub struct FakeServices {
    pub workers: Arc<FakeDirectory>,
    /// The real core step runner, over the fake workers.
    pub steps: Arc<SchemaStepRunner>,
    pub policy: Arc<AskingPolicy>,
    pub limits: Arc<TableLimits>,
    pub handoff: Arc<FakeHandoff>,
    pub quota: Arc<MemoryQuotaBoard>,
    pub health: Arc<MemoryHealthBoard>,
    pub gates: Arc<ScriptedGates>,
    pub registry: Arc<StaticRegistry>,
    pub track_records: Arc<StaticTrackRecords>,
    pub clock: Arc<FakeClock>,
}

impl Default for FakeServices {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeServices {
    pub fn new() -> Self {
        Self {
            workers: Arc::new(FakeDirectory::new()),
            steps: Arc::new(SchemaStepRunner),
            policy: Arc::new(AskingPolicy::default()),
            limits: Arc::new(TableLimits::default()),
            handoff: Arc::new(FakeHandoff::default()),
            quota: Arc::new(MemoryQuotaBoard::default()),
            health: Arc::new(MemoryHealthBoard::default()),
            gates: Arc::new(ScriptedGates::default()),
            registry: Arc::new(StaticRegistry::default()),
            track_records: Arc::new(StaticTrackRecords::default()),
            clock: Arc::new(FakeClock::default()),
        }
    }

    pub fn services(&self) -> Services {
        Services {
            workers: self.workers.clone(),
            steps: self.steps.clone(),
            policy: self.policy.clone(),
            limits: self.limits.clone(),
            handoff: self.handoff.clone(),
            quota: self.quota.clone(),
            health: self.health.clone(),
            gates: self.gates.clone(),
            registry: self.registry.clone(),
            track_records: self.track_records.clone(),
            clock: self.clock.clone(),
        }
    }
}

/// A clock that stands still until a test moves it.
pub struct FakeClock {
    now: Mutex<OffsetDateTime>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self {
            now: Mutex::new(datetime!(2026-09-29 10:00 UTC)),
        }
    }
}

impl FakeClock {
    pub fn set(&self, now: OffsetDateTime) {
        *lock(&self.now) = now;
    }

    pub fn advance(&self, by: Duration) {
        let mut now = lock(&self.now);
        *now += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> OffsetDateTime {
        *lock(&self.now)
    }
}

pub const TEST_RUN: &str = "run-01";

/// An engine context over the fakes, with placeholder paths and no history. Returns the sink to
/// read events from and the command sender. Clone `context.shutdown` first to test a shutdown.
pub fn engine_context(
    services: &FakeServices,
    settings: Settings,
) -> (EngineContext, Arc<MemorySink>, mpsc::Sender<EngineCommand>) {
    let base = PathBuf::from("/nonexistent/argonauts-test");
    let repo = RepoContext {
        id: RepoId::from_canonical_path(&base.join("repo")),
        root: base.join("repo"),
        run_dir: base.join("state/runs").join(TEST_RUN),
        worktrees: base.join("worktrees").join(TEST_RUN),
        base_worktree: base.join("worktrees").join(TEST_RUN).join("base"),
    };
    engine_context_in(services, settings, repo, Vec::new())
}

/// An engine context for a real repository layout and a log to rebuild from.
pub fn engine_context_in(
    services: &FakeServices,
    settings: Settings,
    repo: RepoContext,
    history: Vec<RunEvent>,
) -> (EngineContext, Arc<MemorySink>, mpsc::Sender<EngineCommand>) {
    let run = RunId::new(TEST_RUN).unwrap_or_else(|_| RunId::generate());
    let sink = Arc::new(MemorySink::new(run.clone()));
    for event in &history {
        sink.push(event.clone());
    }
    let (commands, inbox) = mpsc::channel(64);
    let settings_json = serde_json::to_value(&settings).unwrap_or_default();
    let context = EngineContext {
        run,
        repo,
        settings: Arc::new(settings),
        settings_sha256: argonauts_core::sha256_hex(
            argonauts_core::canonical_json(&settings_json).as_bytes(),
        ),
        history,
        sink: sink.clone(),
        inbox,
        vendor_events: services.health.subscribe(),
        services: services.services(),
        shutdown: CancellationToken::new(),
        reopen: false,
    };
    (context, sink, commands)
}

/// Records events in memory, numbering them like the journal does.
pub struct MemorySink {
    run: RunId,
    events: Mutex<Vec<RunEvent>>,
}

impl MemorySink {
    pub fn new(run: RunId) -> Self {
        Self {
            run,
            events: Mutex::new(Vec::new()),
        }
    }

    fn push(&self, event: RunEvent) {
        lock(&self.events).push(event);
    }

    pub fn events(&self) -> Vec<RunEvent> {
        lock(&self.events).clone()
    }

    pub fn kinds(&self) -> Vec<RunEventKind> {
        lock(&self.events).iter().map(|e| e.kind.clone()).collect()
    }
}

impl EventSink for MemorySink {
    fn record(
        &self,
        card: Option<&CardId>,
        session: Option<&SessionKey>,
        source: &str,
        kind: RunEventKind,
    ) -> Result<RunEvent, JournalError> {
        let mut events = lock(&self.events);
        let event = RunEvent {
            seq: events.last().map_or(0, |last| last.seq + 1),
            ts: OffsetDateTime::now_utc(),
            run_id: self.run.clone(),
            card_id: card.cloned(),
            session: session.cloned(),
            source: source.to_owned(),
            kind,
        };
        events.push(event.clone());
        Ok(event)
    }
}

/// Asks you at every decision point, like supervised mode, unless a test scripts the next
/// outcomes. Hard limits report whatever a test adds, and nothing by default.
#[derive(Default)]
pub struct AskingPolicy {
    limits: ScriptedHardLimits,
    next: Mutex<VecDeque<(PolicyOutcome, Duration)>>,
    decisions: Mutex<Vec<UserDecision>>,
    spot_checks: Mutex<Vec<(DecisionPoint, bool)>>,
    evaluated: Mutex<Vec<(DecisionPoint, EvidencePacket)>>,
}

impl AskingPolicy {
    /// The next `evaluate` returns `outcome` instead of asking, such as an auto approval.
    pub fn decide_next(&self, outcome: PolicyOutcome) {
        self.decide_next_after(outcome, Duration::ZERO);
    }

    /// The same, answering only after `delay`, like a slow approver model.
    pub fn decide_next_after(&self, outcome: PolicyOutcome, delay: Duration) {
        lock(&self.next).push_back((outcome, delay));
    }

    pub fn set_point_violations(&self, violations: Vec<Violation>) {
        *lock(&self.limits.point) = violations;
    }

    /// Makes `check_card_diff` report these.
    pub fn set_diff_violations(&self, violations: Vec<Violation>) {
        *lock(&self.limits.diff) = violations;
    }

    pub fn set_session_violations(&self, violations: Vec<Violation>) {
        *lock(&self.limits.session) = violations;
    }

    pub fn evaluated(&self) -> Vec<(DecisionPoint, EvidencePacket)> {
        lock(&self.evaluated).clone()
    }

    pub fn decisions(&self) -> Vec<UserDecision> {
        lock(&self.decisions).clone()
    }

    pub fn spot_checks(&self) -> Vec<(DecisionPoint, bool)> {
        lock(&self.spot_checks).clone()
    }
}

impl ApprovalPolicy for AskingPolicy {
    fn evaluate<'a>(
        &'a self,
        point: &'a DecisionPoint,
        evidence: &'a EvidencePacket,
    ) -> BoxFuture<'a, PolicyOutcome> {
        lock(&self.evaluated).push((point.clone(), evidence.clone()));
        let violations = self.limits.check_point(point, evidence);
        let scripted = lock(&self.next).pop_front();
        Box::pin(async move {
            // Hard limits come first, as the real policy checks them.
            match scripted {
                Some((outcome, delay)) if violations.is_empty() => {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    outcome
                }
                _ => PolicyOutcome::AskUser {
                    violations,
                    shadow: None,
                },
            }
        })
    }

    fn user_decided(&self, decision: &UserDecision) {
        lock(&self.decisions).push(decision.clone());
    }

    fn spot_checked(&self, point: &DecisionPoint, correct: bool) {
        lock(&self.spot_checks).push((point.clone(), correct));
    }

    fn auto_off(&self) {}

    fn hard_limits(&self) -> &dyn HardLimits {
        &self.limits
    }

    fn view(&self) -> ApprovalsView {
        ApprovalsView::default()
    }
}

#[derive(Default)]
struct ScriptedHardLimits {
    point: Mutex<Vec<Violation>>,
    diff: Mutex<Vec<Violation>>,
    session: Mutex<Vec<Violation>>,
}

impl HardLimits for ScriptedHardLimits {
    fn check_point(&self, _point: &DecisionPoint, _evidence: &EvidencePacket) -> Vec<Violation> {
        lock(&self.point).clone()
    }

    fn check_card_diff(&self, _card: &CardSpec, _unified_diff: &str) -> Vec<Violation> {
        lock(&self.diff).clone()
    }

    fn check_session(&self, _spec: &SessionSpec) -> Vec<Violation> {
        lock(&self.session).clone()
    }
}

/// A simple recovery table: vendor trouble asks you, a full context rotates, a stuck or crashed
/// session goes to another agent once and then escalates, and a budget hit applies
/// `on_hit = ask`. `set_next` overrides the next answer.
#[derive(Default)]
pub struct TableLimits {
    overrides: Mutex<VecDeque<RecoveryAction>>,
    all_out: Mutex<Option<ParkReason>>,
    fallback: Mutex<Option<WorkerRef>>,
    stops: Mutex<Vec<StopContext>>,
}

impl TableLimits {
    /// The next `after_stop` answers this instead of the table.
    pub fn set_next(&self, action: RecoveryAction) {
        lock(&self.overrides).push_back(action);
    }

    /// What `all_out` answers.
    pub fn set_all_out(&self, reason: Option<ParkReason>) {
        *lock(&self.all_out) = reason;
    }

    /// The worker a stuck or crashed card moves to on its first failure.
    pub fn set_fallback(&self, worker: WorkerRef) {
        *lock(&self.fallback) = Some(worker);
    }

    pub fn stops(&self) -> Vec<StopContext> {
        lock(&self.stops).clone()
    }

    fn table(&self, context: &StopContext) -> RecoveryAction {
        let packet = || Box::new(simple_packet(&context.card));
        let ask = || {
            RecoveryAction::Ask(Box::new(HandoffPlan {
                card: context.card.id.clone(),
                title: context.card.title.clone(),
                stopped: context.worker.clone(),
                reason: format!("{:?}", context.end.reason),
                resets_at: None,
                work: DiffStat::default(),
                gates_ran: false,
                candidates: Vec::new(),
                mode: HandoffMode::Ask,
                strategy: Default::default(),
                race_count: 1,
                also_moving: Vec::new(),
            }))
        };
        match &context.end.reason {
            EndReason::Completed | EndReason::Paused | EndReason::Stopped => {
                RecoveryAction::Proceed
            }
            EndReason::ContextFull => RecoveryAction::Rotate { packet: packet() },
            EndReason::BudgetHit { .. } => RecoveryAction::BudgetHit(OnHit::Ask),
            EndReason::Stuck { .. } | EndReason::Crashed { .. } | EndReason::StartFailed { .. } => {
                match lock(&self.fallback).clone() {
                    Some(to) if !context.reassigned_once => RecoveryAction::HandOff {
                        to,
                        packet: packet(),
                        checkpoint: None,
                    },
                    _ => RecoveryAction::Escalate {
                        reason: format!("{:?}", context.end.reason),
                    },
                }
            }
            EndReason::VendorError { .. } => RecoveryAction::Wait {
                vendor: context.worker.vendor(),
                until: None,
            },
            EndReason::UsageLimit { .. }
            | EndReason::RateLimited
            | EndReason::Auth { .. }
            | EndReason::Billing { .. }
            | EndReason::ModelUnavailable { .. }
            | EndReason::AdapterBroken { .. } => ask(),
        }
    }
}

impl LimitsService for TableLimits {
    fn session_limits(&self, card: Option<&CardSpec>, _role: RoleName) -> SessionLimits {
        let budget = card.and_then(|card| card.budget.clone().limit);
        SessionLimits {
            budget,
            soft_stop: 0.85,
            max_minutes: card.and_then(|card| card.budget.max_minutes),
            max_tool_calls: card.and_then(|card| card.budget.max_tool_calls),
            inactivity_min: card.and_then(|card| card.budget.inactivity_min),
            repeat_limit: Some(3),
            native_cap_usd: None,
        }
    }

    fn after_stop<'a>(&'a self, context: &'a StopContext) -> BoxFuture<'a, RecoveryAction> {
        lock(&self.stops).push(context.clone());
        let action = lock(&self.overrides)
            .pop_front()
            .unwrap_or_else(|| self.table(context));
        Box::pin(async move { action })
    }

    fn all_out(&self, _workers: &[WorkerRef]) -> Option<ParkReason> {
        lock(&self.all_out).clone()
    }
}

fn simple_packet(card: &CardSpec) -> HandoffPacket {
    HandoffPacket {
        card: card.clone(),
        goal_sha256: None,
        notes: Vec::new(),
        work: WorkSoFar {
            branch: format!("argonauts/{TEST_RUN}/{}", card.id),
            commit: None,
            stat: DiffStat::default(),
        },
        happened: Vec::new(),
        last_todo: Vec::new(),
        gates: Vec::new(),
        why_stopped: String::new(),
        last_words: None,
        last_message: None,
        budget_left: None,
        transcript: None,
        summary: None,
    }
}

/// Offers the candidates a test sets, and builds a bare packet from the card.
#[derive(Default)]
pub struct FakeHandoff {
    candidates: Mutex<Vec<Candidate>>,
    queries: Mutex<Vec<CandidateQuery>>,
}

impl FakeHandoff {
    /// What `candidates` returns.
    pub fn set_candidates(&self, candidates: Vec<Candidate>) {
        *lock(&self.candidates) = candidates;
    }

    pub fn queries(&self) -> Vec<CandidateQuery> {
        lock(&self.queries).clone()
    }
}

impl HandoffPlanner for FakeHandoff {
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate> {
        lock(&self.queries).push(query.clone());
        lock(&self.candidates).clone()
    }

    fn packet(&self, card: &CardSpec, _events: &[RunEvent]) -> HandoffPacket {
        simple_packet(card)
    }
}

#[derive(Default)]
pub struct MemoryQuotaBoard {
    readings: Mutex<BTreeMap<VendorId, QuotaReading>>,
}

impl QuotaBoard for MemoryQuotaBoard {
    fn observe(&self, reading: QuotaReading) {
        lock(&self.readings).insert(reading.vendor.clone(), reading);
    }

    fn get(&self, vendor: &VendorId) -> Option<QuotaReading> {
        lock(&self.readings).get(vendor).cloned()
    }

    fn all(&self) -> Vec<QuotaReading> {
        lock(&self.readings).values().cloned().collect()
    }

    fn probe(&self, vendor: &VendorId) -> BoxFuture<'_, Option<QuotaReading>> {
        let reading = self.get(vendor);
        Box::pin(async move { reading })
    }
}

/// Health in memory, with a breaker simple enough to predict: three overload, server, or
/// network errors in a row put a vendor down, and a success or a passing test card brings it
/// back. Every change is broadcast.
pub struct MemoryHealthBoard {
    states: Mutex<BTreeMap<VendorId, (VendorHealth, u32)>>,
    events: broadcast::Sender<VendorEvent>,
}

impl Default for MemoryHealthBoard {
    fn default() -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            states: Mutex::new(BTreeMap::new()),
            events,
        }
    }
}

const ERRORS_TO_DOWN: u32 = 3;

impl MemoryHealthBoard {
    /// Sets a vendor's state directly and broadcasts it.
    pub fn set(&self, vendor: &VendorId, state: HealthState, reason: Option<&str>) {
        let health = VendorHealth {
            vendor: vendor.clone(),
            state,
            since: OffsetDateTime::now_utc(),
            reason: reason.map(str::to_owned),
            next_test_at: None,
            disabled: None,
        };
        lock(&self.states).insert(vendor.clone(), (health.clone(), 0));
        let _ = self.events.send(VendorEvent::Health(health));
    }

    fn healthy(vendor: &VendorId) -> VendorHealth {
        VendorHealth {
            vendor: vendor.clone(),
            state: HealthState::Healthy,
            since: OffsetDateTime::UNIX_EPOCH,
            reason: None,
            next_test_at: None,
            disabled: None,
        }
    }
}

impl HealthBoard for MemoryHealthBoard {
    fn observe(&self, signal: HealthSignal) {
        let (vendor, change) = match signal {
            HealthSignal::Error {
                vendor,
                category: ErrorCategory::Overloaded | ErrorCategory::Server | ErrorCategory::Network,
                ..
            } => (vendor, Some(true)),
            HealthSignal::Success { vendor, .. } => (vendor, Some(false)),
            HealthSignal::AdapterBroken { vendor, detail } => {
                let mut health = self.state(&vendor);
                health.disabled = Some(detail);
                lock(&self.states).insert(vendor.clone(), (health.clone(), 0));
                let _ = self.events.send(VendorEvent::Health(health));
                return;
            }
            HealthSignal::Retry { vendor, .. } | HealthSignal::Error { vendor, .. } => {
                (vendor, None)
            }
        };
        let Some(failed) = change else { return };
        let (state, errors) = {
            let mut states = lock(&self.states);
            let entry = states
                .entry(vendor.clone())
                .or_insert_with(|| (Self::healthy(&vendor), 0));
            entry.1 = if failed { entry.1 + 1 } else { 0 };
            (entry.0.state, entry.1)
        };
        if failed && errors >= ERRORS_TO_DOWN && state != HealthState::Down {
            self.set(&vendor, HealthState::Down, Some("repeated errors"));
        } else if !failed && state != HealthState::Healthy {
            self.set(&vendor, HealthState::Healthy, None);
        }
    }

    fn state(&self, vendor: &VendorId) -> VendorHealth {
        lock(&self.states)
            .get(vendor)
            .map(|(health, _)| health.clone())
            .unwrap_or_else(|| Self::healthy(vendor))
    }

    fn all(&self) -> Vec<VendorHealth> {
        lock(&self.states)
            .values()
            .map(|(health, _)| health.clone())
            .collect()
    }

    fn subscribe(&self) -> broadcast::Receiver<VendorEvent> {
        self.events.subscribe()
    }

    fn test_result(&self, vendor: &VendorId, ok: bool) {
        let state = if ok {
            HealthState::Healthy
        } else {
            HealthState::Down
        };
        self.set(vendor, state, (!ok).then_some("the test card failed"));
    }
}

/// Gates that pass unless a test queues an outcome. Every request is recorded.
#[derive(Default)]
pub struct ScriptedGates {
    outcomes: Mutex<VecDeque<GateOutcome>>,
    requests: Mutex<Vec<GateRequest>>,
}

impl ScriptedGates {
    /// The next gate returns this.
    pub fn push(&self, outcome: GateOutcome) {
        lock(&self.outcomes).push_back(outcome);
    }

    pub fn fail_next(&self, summary: &str) {
        self.push(GateOutcome {
            name: "tests".to_owned(),
            kind: GateKind::Command,
            passed: false,
            command: None,
            exit_code: Some(1),
            duration_ms: 1,
            summary: summary.to_owned(),
            output_tail: summary.to_owned(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        });
    }

    pub fn requests(&self) -> Vec<GateRequest> {
        lock(&self.requests).clone()
    }
}

impl GateRunner for ScriptedGates {
    fn run(&self, request: GateRequest) -> BoxFuture<'_, GateOutcome> {
        let scripted = lock(&self.outcomes).pop_front();
        let outcome = scripted.unwrap_or_else(|| GateOutcome {
            name: request.name.clone(),
            kind: GateKind::Command,
            passed: true,
            command: Some(request.command.clone()),
            exit_code: Some(0),
            duration_ms: 1,
            summary: "passed".to_owned(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        });
        lock(&self.requests).push(request);
        Box::pin(async move { outcome })
    }
}

/// The model id prefixes that name a family.
const MODEL_FAMILIES: &[&str] = &["claude", "gpt", "gemini"];

/// The family of a built-in harness's default model, as the real registry knows it.
const HARNESS_FAMILIES: &[(&str, &str)] = &[
    ("claude-code", "claude"),
    ("codex", "gpt"),
    ("antigravity-cli", "gemini"),
];

/// A registry of fixed entries. Families come from the entry, else from the model id's
/// prefix (`claude`, `gpt`, `gemini`), else from the harness's default family as the real
/// registry falls back, else `unknown`.
#[derive(Default)]
pub struct StaticRegistry {
    entries: Mutex<Vec<ModelEntry>>,
}

impl StaticRegistry {
    pub fn add(&self, entry: ModelEntry) {
        lock(&self.entries).push(entry);
    }
}

impl ModelRegistry for StaticRegistry {
    fn get(&self, worker: &WorkerRef) -> Option<ModelEntry> {
        lock(&self.entries)
            .iter()
            .find(|entry| {
                &entry.id == worker
                    || (entry.id.worker == worker.worker
                        && worker
                            .model
                            .as_ref()
                            .is_some_and(|model| entry.aliases.contains(model)))
            })
            .cloned()
    }

    fn family(&self, worker: &WorkerRef) -> String {
        if let Some(entry) = self.get(worker) {
            return entry.family;
        }
        let model = worker.model.as_deref().unwrap_or_default();
        let by_model = MODEL_FAMILIES
            .iter()
            .find(|family| model.starts_with(*family))
            .copied();
        let by_harness = || {
            HARNESS_FAMILIES
                .iter()
                .find(|(harness, _)| *harness == worker.worker)
                .map(|&(_, family)| family)
        };
        by_model.or_else(by_harness).unwrap_or("unknown").to_owned()
    }

    fn list(&self) -> Vec<ModelEntry> {
        lock(&self.entries).clone()
    }

    fn estimate(&self, worker: &WorkerRef, input_tokens: u64, output_tokens: u64) -> CostEstimate {
        let Some(entry) = self.get(worker) else {
            return CostEstimate::Unknown;
        };
        match entry.cost.unit {
            CostUnit::None => CostEstimate::Free,
            CostUnit::Quota => CostEstimate::Unknown,
            CostUnit::Usd => {
                let price = |per_mtok: Option<f64>, tokens: u64| {
                    per_mtok.unwrap_or(0.0) * tokens as f64 / 1_000_000.0
                };
                CostEstimate::Usd(
                    price(entry.cost.input_per_mtok, input_tokens)
                        + price(entry.cost.output_per_mtok, output_tokens),
                )
            }
        }
    }

    fn discover(&self) -> BoxFuture<'_, Vec<ModelEntry>> {
        Box::pin(async { Vec::new() })
    }
}

#[derive(Default)]
pub struct StaticTrackRecords {
    records: Mutex<BTreeMap<(WorkerRef, String), TrackRecord>>,
}

impl StaticTrackRecords {
    pub fn set(&self, worker: WorkerRef, task_type: &str, record: TrackRecord) {
        lock(&self.records).insert((worker, task_type.to_owned()), record);
    }
}

impl TrackRecords for StaticTrackRecords {
    fn get(&self, worker: &WorkerRef, task_type: &str) -> Option<TrackRecord> {
        lock(&self.records)
            .get(&(worker.clone(), task_type.to_owned()))
            .copied()
    }
}
