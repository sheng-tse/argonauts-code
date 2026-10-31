//! In-memory, scripted fakes of the engine services, so the crew engine can be tested without
//! the real ones.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use argonauts_core::agents::session_runner::{SessionLimits, SessionSpec};
use argonauts_core::base::clock::Clock;
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::{AgentId, ModelRef, RoleName, SessionKey};
use argonauts_core::models::limits::{
    AgentHealth, Candidate, CandidateQuery, CheckRequest, CheckRunner, CostEstimate, FallbackNote,
    FallbackPlanner, HealthBoard, HealthSignal, HealthState, LimitsService, ModelHealth,
    ModelState, NetState, PlanUsageBoard, PlanUsageReading, PlanWindows, RecoveryAction,
    StopContext, TrackRecord, TrackRecords, Trouble, WorkSoFar,
};
use argonauts_core::models::registry::{CostUnit, ModelEntry, ModelRegistry};
use argonauts_core::runs::engine::{
    EngineCommand, EngineContext, EventSink, RepoContext, Services,
};
use argonauts_core::runs::events::agent_status::FallbackPlan;
use argonauts_core::runs::events::run::WaitReason;
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::runs::events::task::{CheckKind, CheckOutcome, TaskSpec};
use argonauts_core::runs::events::{AgentStatusEvent, RunEvent, RunEventKind};
use argonauts_core::runs::journal::JournalError;
use argonauts_core::safety::policy::{
    ApprovalPolicy, ApprovalsView, DecisionPoint, EvidencePacket, HardLimits, PolicyOutcome,
    UserDecision, Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::limit::OnHit;
use argonauts_core::settings::roles::FallbackMode;
use argonauts_core::workflows::steps::SchemaStepRunner;
use argonauts_core::{DiffStat, ErrorCategory, RepoId, RunId, TaskId};
use time::OffsetDateTime;
use time::macros::datetime;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::engine::runner::FakeDirectory;

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
    pub fallback: Arc<FakeFallback>,
    pub plan_usage: Arc<MemoryPlanUsageBoard>,
    pub health: Arc<MemoryHealthBoard>,
    pub checks: Arc<ScriptedChecks>,
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
        argonauts_conductor::boards::network::turn_off_in_this_process();
        Self {
            workers: Arc::new(FakeDirectory::new()),
            steps: Arc::new(SchemaStepRunner),
            policy: Arc::new(AskingPolicy::default()),
            limits: Arc::new(TableLimits::default()),
            fallback: Arc::new(FakeFallback::default()),
            plan_usage: Arc::new(MemoryPlanUsageBoard::default()),
            health: Arc::new(MemoryHealthBoard::default()),
            checks: Arc::new(ScriptedChecks::default()),
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
            fallback: self.fallback.clone(),
            plan_usage: self.plan_usage.clone(),
            health: self.health.clone(),
            checks: self.checks.clone(),
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
        agent_events: services.health.subscribe(),
        services: services.services(),
        shutdown: CancellationToken::new(),
        reopen: false,
        starts_supervised: false,
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
        task: Option<&TaskId>,
        session: Option<&SessionKey>,
        source: &str,
        kind: RunEventKind,
    ) -> Result<RunEvent, JournalError> {
        let mut events = lock(&self.events);
        let event = RunEvent {
            seq: events.last().map_or(0, |last| last.seq + 1),
            ts: OffsetDateTime::now_utc(),
            run_id: self.run.clone(),
            task_id: task.cloned(),
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
    refused_landings: Mutex<Vec<DecisionPoint>>,
}

impl AskingPolicy {
    /// The next `evaluate` returns `outcome` instead of asking, such as an auto approval.
    pub fn decide_next(&self, outcome: PolicyOutcome) {
        self.decide_next_after(outcome, Duration::ZERO);
    }

    /// The same, answering only after `delay`, like a slow judge.
    pub fn decide_next_after(&self, outcome: PolicyOutcome, delay: Duration) {
        lock(&self.next).push_back((outcome, delay));
    }

    pub fn set_point_violations(&self, violations: Vec<Violation>) {
        *lock(&self.limits.point) = violations;
    }

    /// Makes `check_task_diff` report these.
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

    /// The points whose auto-approved landing was refused, in order.
    pub fn refused_landings(&self) -> Vec<DecisionPoint> {
        lock(&self.refused_landings).clone()
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
                    auto_preview: None,
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

    fn landing_refused(&self, point: &DecisionPoint) {
        lock(&self.refused_landings).push(point.clone());
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

    fn check_task_diff(&self, _task: &TaskSpec, _unified_diff: &str) -> Vec<Violation> {
        lock(&self.diff).clone()
    }

    fn check_session(&self, _spec: &SessionSpec) -> Vec<Violation> {
        lock(&self.session).clone()
    }
}

/// A simple recovery table: agent trouble asks you, a full context rotates, a stuck or crashed
/// session goes to another agent once and then needs you, and a reached limit applies
/// `when_reached = fallback`. `set_next` overrides the next answer.
#[derive(Default)]
pub struct TableLimits {
    overrides: Mutex<VecDeque<RecoveryAction>>,
    all_out: Mutex<Option<WaitReason>>,
    fallback: Mutex<Option<ModelRef>>,
    stops: Mutex<Vec<StopContext>>,
}

impl TableLimits {
    /// The next `after_stop` answers this instead of the table.
    pub fn set_next(&self, action: RecoveryAction) {
        lock(&self.overrides).push_back(action);
    }

    /// What `all_out` answers.
    pub fn set_all_out(&self, reason: Option<WaitReason>) {
        *lock(&self.all_out) = reason;
    }

    /// The worker a stuck or crashed task moves to on its first failure.
    pub fn set_fallback(&self, worker: ModelRef) {
        *lock(&self.fallback) = Some(worker);
    }

    pub fn stops(&self) -> Vec<StopContext> {
        lock(&self.stops).clone()
    }

    fn table(&self, context: &StopContext) -> RecoveryAction {
        let note = || Box::new(simple_note(&context.task));
        let ask = || {
            RecoveryAction::Ask(Box::new(FallbackPlan {
                task: context.task.id.clone(),
                title: context.task.title.clone(),
                stopped: context.worker.clone(),
                reason: format!("{:?}", context.end.reason),
                resets_at: None,
                work: DiffStat::default(),
                checks_ran: false,
                candidates: Vec::new(),
                mode: FallbackMode::Ask,
                strategy: Default::default(),
                race_count: 1,
                also_moving: Vec::new(),
            }))
        };
        match &context.end.reason {
            EndReason::Completed | EndReason::Paused | EndReason::Stopped => {
                RecoveryAction::Proceed
            }
            EndReason::ContextFull => RecoveryAction::Rotate { note: note() },
            EndReason::LimitHit { .. } => RecoveryAction::LimitHit(OnHit::Fallback),
            EndReason::Stuck { .. } | EndReason::Crashed { .. } | EndReason::StartFailed { .. } => {
                match lock(&self.fallback).clone() {
                    Some(to) if !context.reassigned_once => RecoveryAction::Fallback {
                        to,
                        note: note(),
                        checkpoint: None,
                    },
                    _ => RecoveryAction::AskYou {
                        reason: format!("{:?}", context.end.reason),
                    },
                }
            }
            EndReason::CliError { .. } => RecoveryAction::Wait {
                agent_id: context.worker.agent_id(),
                until: None,
                kept_off: None,
            },
            EndReason::UsageLimit { .. }
            | EndReason::RateLimited
            | EndReason::Auth { .. }
            | EndReason::Billing { .. }
            | EndReason::ModelUnavailable { .. }
            | EndReason::AdapterBroken { .. }
            | EndReason::IsolationBroken { .. } => ask(),
        }
    }
}

impl LimitsService for TableLimits {
    fn session_limits(&self, task: Option<&TaskSpec>, _role: RoleName) -> SessionLimits {
        let limit = task.and_then(|task| task.limit.clone().limit);
        SessionLimits {
            limit,
            soft_stop: 0.85,
            max_minutes: task.and_then(|task| task.limit.max_minutes),
            max_tool_calls: task.and_then(|task| task.limit.max_tool_calls),
            inactivity_min: task.and_then(|task| task.limit.inactivity_min),
            repeat_limit: Some(3),
            no_progress_calls: None,
            native_cap_usd: None,
            run_left: None,
            spends_unseen: false,
        }
    }

    fn after_stop<'a>(&'a self, context: &'a StopContext) -> BoxFuture<'a, RecoveryAction> {
        lock(&self.stops).push(context.clone());
        let action = lock(&self.overrides)
            .pop_front()
            .unwrap_or_else(|| self.table(context));
        Box::pin(async move { action })
    }

    fn all_out(&self, _workers: &[ModelRef]) -> Option<WaitReason> {
        lock(&self.all_out).clone()
    }
}

fn simple_note(task: &TaskSpec) -> FallbackNote {
    FallbackNote {
        task: task.clone(),
        goal_sha256: None,
        notes: Vec::new(),
        work: WorkSoFar {
            branch: format!("argonauts/{TEST_RUN}/{}", task.id),
            commit: None,
            stat: DiffStat::default(),
        },
        happened: Vec::new(),
        last_todo: Vec::new(),
        checks: Vec::new(),
        why_stopped: String::new(),
        last_words: None,
        last_message: None,
        limit_left: None,
        transcript: None,
        summary: None,
    }
}

/// Offers the candidates a test sets, and builds a bare note from the task.
#[derive(Default)]
pub struct FakeFallback {
    candidates: Mutex<Vec<Candidate>>,
    queries: Mutex<Vec<CandidateQuery>>,
}

impl FakeFallback {
    /// What `candidates` returns.
    pub fn set_candidates(&self, candidates: Vec<Candidate>) {
        *lock(&self.candidates) = candidates;
    }

    pub fn queries(&self) -> Vec<CandidateQuery> {
        lock(&self.queries).clone()
    }
}

impl FallbackPlanner for FakeFallback {
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate> {
        lock(&self.queries).push(query.clone());
        lock(&self.candidates).clone()
    }

    fn note(&self, task: &TaskSpec, _events: &[RunEvent]) -> FallbackNote {
        simple_note(task)
    }
}

/// The last reading of each agent's window wins, whatever its age, so a test sets what it
/// wants.
#[derive(Default)]
pub struct MemoryPlanUsageBoard {
    readings: Mutex<BTreeMap<AgentId, PlanWindows>>,
    plans: Mutex<BTreeMap<AgentId, String>>,
    live: Mutex<BTreeMap<AgentId, argonauts_core::models::plan_usage::LiveUsage>>,
    doubted: Mutex<BTreeMap<AgentId, time::OffsetDateTime>>,
    probed: Mutex<Vec<AgentId>>,
}

impl MemoryPlanUsageBoard {
    /// The CLIs probed so far, in order.
    pub fn probed(&self) -> Vec<AgentId> {
        lock(&self.probed).clone()
    }
}

impl PlanUsageBoard for MemoryPlanUsageBoard {
    fn observe(&self, reading: PlanUsageReading) {
        lock(&self.readings)
            .entry(reading.agent_id.clone())
            .or_default()
            .put(reading);
    }

    fn get(&self, agent_id: &AgentId) -> Option<PlanUsageReading> {
        lock(&self.readings).get(agent_id)?.binding().cloned()
    }

    fn all(&self) -> Vec<PlanUsageReading> {
        lock(&self.readings)
            .values()
            .filter_map(|windows| windows.binding().cloned())
            .collect()
    }

    fn windows(&self, agent_id: &AgentId) -> Vec<PlanUsageReading> {
        lock(&self.readings)
            .get(agent_id)
            .map(|windows| windows.readings().to_vec())
            .unwrap_or_default()
    }

    fn plan(&self, agent_id: &AgentId) -> Option<String> {
        lock(&self.plans).get(agent_id).cloned()
    }

    fn observe_plan(&self, agent_id: &AgentId, plan: String) {
        lock(&self.plans).insert(agent_id.clone(), plan);
    }

    fn observe_live(&self, reading: argonauts_core::models::plan_usage::LiveUsage) {
        lock(&self.live).insert(reading.agent_id.clone(), reading);
    }

    fn live(&self, agent_id: &AgentId) -> Option<argonauts_core::models::plan_usage::LiveUsage> {
        lock(&self.live).get(agent_id).cloned()
    }

    fn doubted_at(&self, agent_id: &AgentId) -> Option<time::OffsetDateTime> {
        lock(&self.doubted).get(agent_id).copied()
    }

    fn doubt(&self, agent_id: &AgentId) {
        lock(&self.doubted).insert(agent_id.clone(), time::OffsetDateTime::now_utc());
    }

    fn probe(&self, agent_id: &AgentId) -> BoxFuture<'_, Option<PlanUsageReading>> {
        lock(&self.probed).push(agent_id.clone());
        let reading = self.get(agent_id);
        Box::pin(async move { reading })
    }
}

/// Health in memory, with a breaker simple enough to predict. Two overload or server errors in
/// a row take the model they came from out, and two models out take their agent down. Three
/// network errors in a row put an agent down as well. A success or a passing test task brings
/// either back. Every change is broadcast. The internet is up until a test says otherwise.
pub struct MemoryHealthBoard {
    states: Mutex<BTreeMap<AgentId, (AgentHealth, u32)>>,
    models: Mutex<BTreeMap<ModelRef, (ModelHealth, u32)>>,
    events: broadcast::Sender<AgentStatusEvent>,
    network: watch::Sender<NetState>,
}

impl Default for MemoryHealthBoard {
    fn default() -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            states: Mutex::new(BTreeMap::new()),
            models: Mutex::new(BTreeMap::new()),
            events,
            network: watch::Sender::new(NetState::Up),
        }
    }
}

const ERRORS_TO_DOWN: u32 = 3;
const ERRORS_TO_OUT: u32 = 2;
const MODELS_OUT_TO_DOWN: usize = 2;

impl MemoryHealthBoard {
    /// Sets an agent's state directly and broadcasts it.
    pub fn set(&self, agent_id: &AgentId, state: HealthState, reason: Option<&str>) {
        let health = AgentHealth {
            agent_id: agent_id.clone(),
            state,
            since: OffsetDateTime::now_utc(),
            reason: reason.map(str::to_owned),
            next_test_at: None,
            disabled: None,
        };
        lock(&self.states).insert(agent_id.clone(), (health.clone(), 0));
        let _ = self.events.send(AgentStatusEvent::Health(health));
    }

    /// Sets one model's state directly and broadcasts it. `trouble` is what took it out.
    pub fn set_model(&self, model: &ModelRef, state: ModelState, trouble: Option<Trouble>) {
        let health = ModelHealth {
            state,
            since: OffsetDateTime::now_utc(),
            reason: trouble.map(|trouble| trouble.words().to_owned()),
            trouble,
            ..ModelHealth::healthy(model)
        };
        lock(&self.models).insert(model.clone(), (health.clone(), 0));
        let _ = self.events.send(AgentStatusEvent::Model(health));
    }

    /// Says the internet went or came back, and tells everyone waiting on it.
    pub fn set_network(&self, state: NetState) {
        self.network.send_replace(state);
    }

    fn healthy(agent_id: &AgentId) -> AgentHealth {
        AgentHealth {
            agent_id: agent_id.clone(),
            state: HealthState::Healthy,
            since: OffsetDateTime::UNIX_EPOCH,
            reason: None,
            next_test_at: None,
            disabled: None,
        }
    }

    /// Counts an overload or server error against `model`. Two in a row take it out, and a
    /// second model out takes the agent down.
    fn model_failed(&self, model: &ModelRef, trouble: Trouble) {
        let errors = {
            let mut models = lock(&self.models);
            let entry = models
                .entry(model.clone())
                .or_insert_with(|| (ModelHealth::healthy(model), 0));
            entry.1 += 1;
            (entry.0.state != ModelState::Out).then_some(entry.1)
        };
        if errors.is_none_or(|errors| errors < ERRORS_TO_OUT) {
            return;
        }
        self.set_model(model, ModelState::Out, Some(trouble));
        let agent_id = model.agent_id();
        let out = self
            .models(&agent_id)
            .iter()
            .filter(|health| health.state == ModelState::Out)
            .count();
        if out >= MODELS_OUT_TO_DOWN && self.state(&agent_id).state != HealthState::Down {
            self.set(
                &agent_id,
                HealthState::Down,
                Some("two of its models are out"),
            );
        }
    }

    fn model_worked(&self, model: &ModelRef) {
        let was = lock(&self.models)
            .get(model)
            .map(|(health, _)| health.state);
        match was {
            Some(ModelState::Healthy) => {
                lock(&self.models).remove(model);
            }
            Some(_) => self.set_model(model, ModelState::Healthy, None),
            None => {}
        }
    }
}

impl HealthBoard for MemoryHealthBoard {
    fn observe(&self, signal: HealthSignal) {
        let (agent_id, change) = match signal {
            HealthSignal::Error {
                agent_id,
                model,
                category,
                ..
            } if Trouble::of(category).is_some() => {
                let model = ModelRef::new(agent_id.0, model);
                if let Some(trouble) = Trouble::of(category) {
                    self.model_failed(&model, trouble);
                }
                return;
            }
            HealthSignal::Error {
                agent_id,
                category: ErrorCategory::Network,
                ..
            } => (agent_id, Some(true)),
            HealthSignal::Success {
                agent_id, model, ..
            } => {
                self.model_worked(&ModelRef::new(agent_id.0.clone(), model));
                (agent_id, Some(false))
            }
            HealthSignal::AdapterBroken { agent_id, detail } => {
                let mut health = self.state(&agent_id);
                health.disabled = Some(detail);
                lock(&self.states).insert(agent_id.clone(), (health.clone(), 0));
                let _ = self.events.send(AgentStatusEvent::Health(health));
                return;
            }
            HealthSignal::Retry { agent_id, .. } | HealthSignal::Error { agent_id, .. } => {
                (agent_id, None)
            }
        };
        let Some(failed) = change else { return };
        let (state, errors) = {
            let mut states = lock(&self.states);
            let entry = states
                .entry(agent_id.clone())
                .or_insert_with(|| (Self::healthy(&agent_id), 0));
            entry.1 = if failed { entry.1 + 1 } else { 0 };
            (entry.0.state, entry.1)
        };
        if failed && errors >= ERRORS_TO_DOWN && state != HealthState::Down {
            self.set(&agent_id, HealthState::Down, Some("repeated errors"));
        } else if !failed && state != HealthState::Healthy {
            self.set(&agent_id, HealthState::Healthy, None);
        }
    }

    fn state(&self, agent_id: &AgentId) -> AgentHealth {
        lock(&self.states)
            .get(agent_id)
            .map(|(health, _)| health.clone())
            .unwrap_or_else(|| Self::healthy(agent_id))
    }

    fn all(&self) -> Vec<AgentHealth> {
        lock(&self.states)
            .values()
            .map(|(health, _)| health.clone())
            .collect()
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentStatusEvent> {
        self.events.subscribe()
    }

    fn test_result(&self, agent_id: &AgentId, ok: bool) {
        let state = if ok {
            HealthState::Healthy
        } else {
            HealthState::Down
        };
        self.set(agent_id, state, (!ok).then_some("the test task failed"));
    }

    fn model(&self, model: &ModelRef) -> ModelHealth {
        lock(&self.models)
            .get(model)
            .map(|(health, _)| health.clone())
            .unwrap_or_else(|| ModelHealth::healthy(model))
    }

    fn models(&self, agent_id: &AgentId) -> Vec<ModelHealth> {
        lock(&self.models)
            .values()
            .map(|(health, _)| health.clone())
            .filter(|health| &health.model.agent_id() == agent_id)
            .collect()
    }

    fn network(&self) -> NetState {
        *self.network.borrow()
    }

    fn network_changes(&self) -> watch::Receiver<NetState> {
        self.network.subscribe()
    }
}

/// Checks that pass unless a test queues an outcome. Every request is recorded. The checks a
/// run makes on its starting code are apart: they pass, take nothing from the queue and aren't
/// among the requests, unless a test queues what the starting code does.
#[derive(Default)]
pub struct ScriptedChecks {
    outcomes: Mutex<VecDeque<CheckOutcome>>,
    requests: Mutex<Vec<CheckRequest>>,
    at_start: Mutex<VecDeque<CheckOutcome>>,
    start_requests: Mutex<Vec<CheckRequest>>,
}

impl ScriptedChecks {
    /// The next check returns this.
    pub fn push(&self, outcome: CheckOutcome) {
        lock(&self.outcomes).push_back(outcome);
    }

    /// The next check of the starting code returns this.
    pub fn push_at_start(&self, outcome: CheckOutcome) {
        lock(&self.at_start).push_back(outcome);
    }

    /// The checks asked of the starting code.
    pub fn start_requests(&self) -> Vec<CheckRequest> {
        lock(&self.start_requests).clone()
    }

    pub fn fail_next(&self, summary: &str) {
        self.push(CheckOutcome {
            name: "tests".to_owned(),
            kind: CheckKind::Command,
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

    pub fn requests(&self) -> Vec<CheckRequest> {
        lock(&self.requests).clone()
    }
}

impl CheckRunner for ScriptedChecks {
    fn run(&self, request: CheckRequest) -> BoxFuture<'_, CheckOutcome> {
        let at_start = request.task.as_str() == argonauts_core::models::limits::START_CHECKS;
        let scripted = if at_start {
            lock(&self.at_start).pop_front()
        } else {
            lock(&self.outcomes).pop_front()
        };
        let outcome = scripted.unwrap_or_else(|| CheckOutcome {
            name: request.name.clone(),
            kind: CheckKind::Command,
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
        if at_start {
            lock(&self.start_requests).push(request);
        } else {
            lock(&self.requests).push(request);
        }
        Box::pin(async move { outcome })
    }
}

/// Model id prefixes and the provider each names.
const MODEL_PROVIDERS: &[(&str, &str)] = &[
    ("claude", "anthropic"),
    ("gpt", "openai"),
    ("gemini", "google"),
    ("qwen", "alibaba"),
];

/// The provider of a built-in CLI's default model, as the real registry knows it.
const CLI_PROVIDERS: &[(&str, &str)] = &[
    ("claude-code", "anthropic"),
    ("codex", "openai"),
    ("antigravity-cli", "google"),
];

/// A registry of fixed entries. Providers come from the entry, else from the model id's
/// prefix (`claude`, `gpt`, `gemini`, `qwen`), else from the CLI's default provider as the real
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
    fn get(&self, worker: &ModelRef) -> Option<ModelEntry> {
        lock(&self.entries)
            .iter()
            .find(|entry| {
                &entry.id == worker
                    || (entry.id.agent == worker.agent
                        && worker
                            .model
                            .as_ref()
                            .is_some_and(|model| entry.aliases.contains(model)))
            })
            .cloned()
    }

    fn provider(&self, worker: &ModelRef) -> String {
        if let Some(entry) = self.get(worker) {
            return entry.provider;
        }
        let model = worker.model.as_deref().unwrap_or_default();
        let by_model = MODEL_PROVIDERS
            .iter()
            .find(|(prefix, _)| model.starts_with(prefix))
            .map(|&(_, provider)| provider);
        let by_cli = || {
            CLI_PROVIDERS
                .iter()
                .find(|(cli, _)| *cli == worker.agent)
                .map(|&(_, provider)| provider)
        };
        by_model.or_else(by_cli).unwrap_or("unknown").to_owned()
    }

    fn list(&self) -> Vec<ModelEntry> {
        lock(&self.entries).clone()
    }

    fn estimate(&self, worker: &ModelRef, input_tokens: u64, output_tokens: u64) -> CostEstimate {
        let Some(entry) = self.get(worker) else {
            return CostEstimate::Unknown;
        };
        match entry.cost.unit {
            CostUnit::None => CostEstimate::Free,
            CostUnit::PlanUsage => CostEstimate::Unknown,
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
    records: Mutex<BTreeMap<(ModelRef, String), TrackRecord>>,
}

impl StaticTrackRecords {
    pub fn set(&self, worker: ModelRef, task_type: &str, record: TrackRecord) {
        lock(&self.records).insert((worker, task_type.to_owned()), record);
    }
}

impl TrackRecords for StaticTrackRecords {
    fn get(&self, worker: &ModelRef, task_type: &str) -> Option<TrackRecord> {
        lock(&self.records)
            .get(&(worker.clone(), task_type.to_owned()))
            .copied()
    }
}
