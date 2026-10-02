//! For the run-side tests: the controller's context with the fake conductor and approvals,
//! driving the runs part's handlers directly and applying what their stream tasks send, as the
//! loop does. It reaches `start`, which the harness only reaches through a chat turn.

// Shared by many tests, and not every test reads every part.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use argonauts_core::api::{Approvals, StartRun};
use argonauts_core::app::AppEvent;
use argonauts_core::clock::SystemClock;
use argonauts_core::events::checkpoint::{
    CheckpointEvent, CheckpointKind, CheckpointOpened, CheckpointPacket, FreeText, QuestionPacket,
};
use argonauts_core::events::run::RunMode;
use argonauts_core::events::run::{ParkReason, ResumeCause, RunLifecycle, RunOutcome, RunResumed};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{CheckpointId, WorkerRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::steps::SchemaStepRunner;
use argonauts_core::{CardId, Paths, RunId};
use argonauts_testkit::{FakeDirectory, FakeWorker, TempRepo};
use time::OffsetDateTime;
use tokio::sync::mpsc;

use super::{RunsState, on_update};
use crate::app_settings::AppSettings;
use crate::controller::test_support::{FakeApprovals, FakeConductor, WAIT};
use crate::controller::{AppServices, Ctx, Effect, Internal, Outbox};
use crate::env::AppEnv;
use crate::trace::Trace;

pub(crate) struct Rig {
    pub repo: TempRepo,
    pub ctx: Ctx,
    pub runs: RunsState,
    pub conductor: Arc<FakeConductor>,
    pub approvals: Arc<FakeApprovals>,
    pub agent: Arc<FakeWorker>,
    pub effects: Vec<Effect>,
    events: mpsc::UnboundedReceiver<AppEvent>,
    internal: mpsc::UnboundedReceiver<Internal>,
    seen: Vec<AppEvent>,
}

impl Rig {
    pub(crate) fn new() -> Self {
        Self::with(true, |_| {})
    }

    /// `prepare` runs before the settings load, such as to write the global config.
    pub(crate) fn with(interactive: bool, prepare: impl FnOnce(&Path)) -> Self {
        let repo = TempRepo::new().expect("a temp repo");
        let paths = Paths::new(repo.home());
        prepare(&paths.global_config());
        let mut env =
            AppEnv::for_repo(paths.clone(), repo.root(), interactive).expect("the app environment");
        env.agent_session = false;
        env.user_home = Some(repo.user_home().to_owned());
        let env = Arc::new(env);
        let settings =
            AppSettings::load(&env, Vec::new(), interactive).expect("settings that resolve");
        let directory = Arc::new(FakeDirectory::new());
        let agent = directory.add(FakeWorker::harness("claude-code", "claude"));
        let conductor = Arc::new(FakeConductor::default());
        let approvals = Arc::new(FakeApprovals::default());
        let (quota, health) = argonauts_conductor::boards(&paths);
        let registry =
            crate::controller::test_support::quiet_registry(settings.settings(), repo.root());
        let services = AppServices {
            conductor: conductor.clone(),
            approvals: interactive.then(|| approvals.clone() as Arc<dyn Approvals>),
            workers: directory,
            steps: Arc::new(SchemaStepRunner),
            registry,
            quota,
            health,
            clock: Arc::new(SystemClock),
        };
        let (events_sender, events) = mpsc::unbounded_channel();
        let out = Outbox::new(events_sender, Arc::new(Trace::off()));
        let (internal_sender, internal) = mpsc::unbounded_channel();
        let ctx = Ctx::new(env, settings, services, out, internal_sender);
        let runs = RunsState::new(&ctx);
        Self {
            repo,
            ctx,
            runs,
            conductor,
            approvals,
            agent,
            effects: Vec::new(),
            events,
            internal,
            seen: Vec::new(),
        }
    }

    pub(crate) async fn start(&mut self, request: &str) -> RunId {
        let effects = super::start(&mut self.ctx, &mut self.runs, start_run(request))
            .await
            .expect("the run starts");
        self.effects.extend(effects);
        self.until(|event| matches!(event, AppEvent::Attached { .. }))
            .await;
        super::attached(&self.runs)
            .cloned()
            .expect("an attached run")
    }

    /// Applies stream updates until an event matches, and returns it.
    pub(crate) async fn until(&mut self, what: impl Fn(&AppEvent) -> bool) -> AppEvent {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            while let Ok(event) = self.events.try_recv() {
                self.seen.push(event.clone());
                if what(&event) {
                    return event;
                }
            }
            tokio::select! {
                update = self.internal.recv() => {
                    let Some(Internal::Run(update)) = update else { continue };
                    let effects = on_update(&mut self.ctx, &mut self.runs, update)
                        .await
                        .expect("the update applies");
                    self.effects.extend(effects);
                }
                event = self.events.recv() => {
                    let event = event.expect("the outbox is open");
                    self.seen.push(event.clone());
                    if what(&event) {
                        return event;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    panic!("no match within {WAIT:?}; seen so far: {:#?}", self.seen)
                }
            }
        }
    }

    /// Applies every update the stream tasks send until they go quiet, and returns the events.
    pub(crate) async fn settle(&mut self) -> Vec<AppEvent> {
        let quiet = std::time::Duration::from_millis(150);
        while let Ok(Some(update)) = tokio::time::timeout(quiet, self.internal.recv()).await {
            if let Internal::Run(update) = update {
                let effects = on_update(&mut self.ctx, &mut self.runs, update)
                    .await
                    .expect("the update applies");
                self.effects.extend(effects);
            }
        }
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event.clone());
            events.push(event);
        }
        events
    }

    pub(crate) fn seen(&self) -> &[AppEvent] {
        &self.seen
    }
}

pub(crate) fn start_run(request: &str) -> StartRun {
    StartRun {
        request: request.to_owned(),
        mode: RunMode::Solo,
        forced: true,
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        overrides: Vec::new(),
        include_uncommitted: false,
        default_agent: None,
        approval_mode: None,
        front_end: None,
    }
}

pub(crate) fn run_id(name: &str) -> RunId {
    RunId::new(name).expect("a run id")
}

pub(crate) fn event(run: &RunId, seq: u64, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts: OffsetDateTime::UNIX_EPOCH,
        run_id: run.clone(),
        card_id: None,
        session: None,
        source: "conductor".to_owned(),
        kind,
    }
}

pub(crate) fn resumed(run: &RunId, seq: u64) -> RunEvent {
    let resumed = RunResumed {
        cause: ResumeCause::User,
        settings_changed: false,
        harness_versions_changed: Vec::new(),
        main_moved_by: None,
        sessions: Vec::new(),
    };
    event(run, seq, RunEventKind::Run(RunLifecycle::Resumed(resumed)))
}

pub(crate) fn parked(run: &RunId, seq: u64, waiting_on: Vec<CheckpointId>) -> RunEvent {
    let parked = RunLifecycle::Parked {
        reason: ParkReason::Checkpoint,
        waiting_on,
    };
    event(run, seq, RunEventKind::Run(parked))
}

pub(crate) fn finished(run: &RunId, seq: u64, outcome: RunOutcome) -> RunEvent {
    let finished = RunLifecycle::Finished {
        outcome,
        detail: None,
    };
    event(run, seq, RunEventKind::Run(finished))
}

/// A question from an agent that waits for you.
pub(crate) fn question(run: &RunId, seq: u64, id: &str) -> RunEvent {
    let card = CardId::new("card-001").expect("a card id");
    let opened = CheckpointOpened {
        id: CheckpointId::parse(id).expect("a checkpoint id"),
        kind: CheckpointKind::Question,
        title: "Question · card-001".to_owned(),
        card: Some(card.clone()),
        packet: CheckpointPacket::Question(QuestionPacket {
            from: WorkerRef::new("codex", None),
            card,
            text: "Which store?".to_owned(),
            options: Vec::new(),
        }),
        evidence_sha256: "e".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::Answer,
        needs_you: true,
    };
    let kind = RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened)));
    event(run, seq, kind)
}
