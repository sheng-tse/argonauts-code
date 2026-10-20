//! For the run-side tests: the controller's context with the fake conductor and approvals,
//! driving the runs part's handlers directly and applying what their stream tasks send, as the
//! loop does. It reaches `start`, which the CLI only reaches through a conversation turn.

// Shared by many tests, and not every test reads every part.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use argonauts_core::api::{Approvals, StartRun};
use argonauts_core::app::AppEvent;
use argonauts_core::clock::SystemClock;
use argonauts_core::events::approval::{
    ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText, QuestionPacket,
};
use argonauts_core::events::run::RunMode;
use argonauts_core::events::run::{ResumeCause, RunLifecycle, RunOutcome, RunResumed, WaitReason};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ApprovalId, ModelRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::steps::SchemaStepRunner;
use argonauts_core::{Paths, RunId, TaskId};
use argonauts_testkit::{FakeDirectory, FakeRunner, TempRepo};
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
    pub agent: Arc<FakeRunner>,
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
        let flags = crate::controller::test_support::quiet_local_servers(Vec::new());
        let settings = AppSettings::load(&env, flags, interactive).expect("settings that resolve");
        let directory = Arc::new(FakeDirectory::new());
        let agent = directory.add(FakeRunner::cli("claude-code", "anthropic"));
        let conductor = Arc::new(FakeConductor::default());
        let approvals = Arc::new(FakeApprovals::default());
        let (plan_usage, health) = argonauts_conductor::boards(&paths);
        let registry =
            crate::controller::test_support::quiet_registry(settings.settings(), repo.root());
        let services = AppServices {
            conductor: conductor.clone(),
            approvals: interactive.then(|| approvals.clone() as Arc<dyn Approvals>),
            workers: directory,
            steps: Arc::new(SchemaStepRunner),
            registry,
            plan_usage,
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
        mode: RunMode::Captain,
        forced: true,
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        overrides: Vec::new(),
        include_uncommitted: false,
        main_agent: None,
        approval_mode: None,
        outside_agent: None,
        instructions: Vec::new(),
        request_settings: Vec::new(),
        required_steps: Vec::new(),
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
        task_id: None,
        session: None,
        source: "argonauts-code".to_owned(),
        kind,
    }
}

pub(crate) fn resumed(run: &RunId, seq: u64) -> RunEvent {
    let resumed = RunResumed {
        cause: ResumeCause::User,
        settings_changed: false,
        cli_versions_changed: Vec::new(),
        main_moved_by: None,
        sessions: Vec::new(),
    };
    event(run, seq, RunEventKind::Run(RunLifecycle::Resumed(resumed)))
}

pub(crate) fn waiting(run: &RunId, seq: u64, waiting_on: Vec<ApprovalId>) -> RunEvent {
    let waiting = RunLifecycle::Waiting {
        reason: WaitReason::Approval,
        waiting_on,
    };
    event(run, seq, RunEventKind::Run(waiting))
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
    let task = TaskId::new("task-001").expect("a task id");
    let opened = ApprovalOpened {
        id: ApprovalId::parse(id).expect("an approval id"),
        kind: ApprovalKind::Question,
        title: "Question · task-001".to_owned(),
        task: Some(task.clone()),
        packet: ApprovalPacket::Question(QuestionPacket {
            from: ModelRef::new("codex", None),
            task,
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
    let kind = RunEventKind::Approval(ApprovalEvent::Opened(Box::new(opened)));
    event(run, seq, kind)
}
