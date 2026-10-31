//! Test doubles for the controller: Argonauts Code and approvals that record every call, and a
//! test driver that runs the real controller loop against them and testkit's fake workers.

// Shared by many tests, and not every test reads every part.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::app::{AppEvent, UiCommand};
use argonauts_core::base::clock::SystemClock;
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::{ApprovalId, MessageId, ModelRef, SessionKey};
use argonauts_core::input::MentionTarget;
use argonauts_core::models::registry::ModelRegistry;
use argonauts_core::runs::api::{
    ApiError, ApprovalAnswer, Approvals, ConductorApi, DiffScope, DiffView, EventStream, RawView,
    RunHandle, RunSummary, StartRun, StatusSummary, SteerAck, TakeOverTicket,
};
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::council::CouncilAction;
use argonauts_core::runs::events::goal::GoalAction;
use argonauts_core::runs::events::session::QueuedMessage;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::workflows::steps::SchemaStepRunner;
use argonauts_core::{Paths, RunId, TaskId};
use argonauts_testkit::engine::services::{MemoryHealthBoard, MemoryPlanUsageBoard};
use argonauts_testkit::{FakeDirectory, FakeRunner, TempRepo};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::controller::{AppServices, Controller, Ctx, Internal, Opening, Outbox};
use crate::startup::app_settings::AppSettings;
use crate::startup::env::AppEnv;
use crate::testing::trace::Trace;

/// How long a test waits for the next event before failing: five seconds, times the deadline
/// multiplier a slower machine sets.
pub(crate) fn max_wait() -> Duration {
    argonauts_testkit::pty::deadline::scaled(Duration::from_secs(5))
}

/// Argonauts Code call, as the fake recorded it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Call {
    Start(Box<StartRun>),
    Subscribe {
        run: RunId,
        from_seq: u64,
    },
    Reattach(RunId),
    Steer {
        run: RunId,
        session: SessionKey,
        text: String,
    },
    Mention {
        run: RunId,
        target: MentionTarget,
        text: String,
    },
    Guidance {
        run: RunId,
        text: String,
    },
    TakeBack {
        run: RunId,
        messages: Vec<MessageId>,
    },
    Pause {
        run: RunId,
        now: bool,
    },
    CancelPause(RunId),
    Resume(RunId),
    Stop(RunId),
    StopSession {
        run: RunId,
        session: SessionKey,
    },
    RestartSession {
        run: RunId,
        session: SessionKey,
    },
    Reassign {
        run: RunId,
        task: TaskId,
        to: ModelRef,
    },
    List,
    Continue {
        merged: RunId,
        request: Box<StartRun>,
    },
    Status(RunId),
    Diff {
        run: RunId,
        task: TaskId,
    },
    Raw {
        run: RunId,
        session: SessionKey,
    },
    Rewind {
        run: RunId,
        task: TaskId,
        checkpoint: u32,
    },
    SpotCheck {
        run: RunId,
        id: ApprovalId,
        correct: bool,
    },
    Goal {
        run: RunId,
        action: GoalAction,
    },
    Council {
        run: RunId,
        action: CouncilAction,
    },
}

struct Stream {
    events: mpsc::Sender<RunEvent>,
    live: watch::Sender<bool>,
}

/// Records every call. `start` names runs `run-001`, `run-002` …; a test feeds a subscribed
/// run's events with [`FakeConductor::push`] and ends its stream with [`FakeConductor::end`].
#[derive(Default)]
pub(crate) struct FakeConductor {
    calls: Mutex<Vec<Call>>,
    runs: Mutex<Vec<RunSummary>>,
    status: Mutex<StatusSummary>,
    streams: Mutex<HashMap<RunId, Stream>>,
    started: AtomicU32,
    messages: AtomicU32,
    /// Runs that already merged, which refuse what acts on them as the real ones do.
    merged: Mutex<Vec<RunId>>,
    /// Runs whose run host is gone by the time a stream opens on them.
    gone: Mutex<Vec<RunId>>,
}

impl FakeConductor {
    /// `run` already merged, so acting on it is refused.
    pub(crate) fn merged(&self, run: &RunId) {
        lock(&self.merged).push(run.clone());
    }

    /// `run`'s run host is gone before a stream opens on it, as one that couldn't start is.
    pub(crate) fn gone_before_its_stream(&self, run: &RunId) {
        lock(&self.gone).push(run.clone());
    }

    fn refuses(&self, run: &RunId) -> Option<ApiError> {
        lock(&self.merged)
            .contains(run)
            .then(|| ApiError::Merged(run.clone()))
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        lock(&self.calls).clone()
    }

    pub(crate) fn starts(&self) -> Vec<StartRun> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Start(request) => Some(*request),
                _ => None,
            })
            .collect()
    }

    /// What `list` returns.
    pub(crate) fn set_runs(&self, runs: Vec<RunSummary>) {
        *lock(&self.runs) = runs;
    }

    /// What `status` returns.
    pub(crate) fn set_status(&self, summary: StatusSummary) {
        *lock(&self.status) = summary;
    }

    /// Sends an event on the run's open stream. False when nothing subscribed.
    pub(crate) async fn push(&self, run: &RunId, event: RunEvent) -> bool {
        let sender = lock(&self.streams)
            .get(run)
            .map(|stream| stream.events.clone());
        match sender {
            Some(sender) => sender.send(event).await.is_ok(),
            None => false,
        }
    }

    /// Closes the run's stream; with `crashed`, the run host went away first.
    pub(crate) fn end(&self, run: &RunId, crashed: bool) {
        if let Some(stream) = lock(&self.streams).remove(run)
            && crashed
        {
            let _ = stream.live.send(false);
        }
    }

    fn record(&self, call: Call) {
        lock(&self.calls).push(call);
    }

    fn open_stream(&self, run: &RunId) -> EventStream {
        let (events, receiver) = mpsc::channel(256);
        let (live, live_receiver) = watch::channel(!lock(&self.gone).contains(run));
        lock(&self.streams).insert(run.clone(), Stream { events, live });
        EventStream {
            events: receiver,
            replay_until: None,
            live: live_receiver,
        }
    }

    fn new_run(&self) -> RunHandle {
        let n = self.started.fetch_add(1, Ordering::SeqCst) + 1;
        let run = RunId::new(format!("run-{n:03}")).expect("a valid run id");
        RunHandle {
            events: std::path::PathBuf::from(format!("/fake/{run}/events.jsonl")),
            run,
        }
    }

    fn message_id(&self) -> MessageId {
        MessageId::numbered(self.messages.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ready<'a, T: Send + 'a>(value: T) -> BoxFuture<'a, T> {
    Box::pin(std::future::ready(value))
}

impl ConductorApi for FakeConductor {
    fn start(&self, request: StartRun) -> BoxFuture<'_, Result<RunHandle, ApiError>> {
        self.record(Call::Start(Box::new(request)));
        ready(Ok(self.new_run()))
    }

    fn subscribe(
        &self,
        run: &RunId,
        from_seq: u64,
    ) -> BoxFuture<'_, Result<EventStream, ApiError>> {
        self.record(Call::Subscribe {
            run: run.clone(),
            from_seq,
        });
        ready(Ok(self.open_stream(run)))
    }

    fn reattach(&self, run: &RunId) -> BoxFuture<'_, Result<EventStream, ApiError>> {
        self.record(Call::Reattach(run.clone()));
        ready(Ok(self.open_stream(run)))
    }

    fn steer(
        &self,
        run: &RunId,
        session: &SessionKey,
        text: String,
        _now: bool,
    ) -> BoxFuture<'_, Result<SteerAck, ApiError>> {
        self.record(Call::Steer {
            run: run.clone(),
            session: session.clone(),
            text,
        });
        ready(Err(ApiError::NotNow(
            "the fake conductor doesn't steer".to_owned(),
        )))
    }

    fn mention(
        &self,
        run: &RunId,
        target: MentionTarget,
        text: String,
    ) -> BoxFuture<'_, Result<MessageId, ApiError>> {
        self.record(Call::Mention {
            run: run.clone(),
            target,
            text,
        });
        ready(Ok(self.message_id()))
    }

    fn guidance(&self, run: &RunId, text: String) -> BoxFuture<'_, Result<MessageId, ApiError>> {
        self.record(Call::Guidance {
            run: run.clone(),
            text,
        });
        ready(Ok(self.message_id()))
    }

    fn take_back(
        &self,
        run: &RunId,
        messages: Vec<MessageId>,
    ) -> BoxFuture<'_, Result<Vec<QueuedMessage>, ApiError>> {
        self.record(Call::TakeBack {
            run: run.clone(),
            messages,
        });
        ready(Ok(Vec::new()))
    }

    fn pause(&self, run: &RunId, now: bool) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Pause {
            run: run.clone(),
            now,
        });
        ready(Ok(()))
    }

    fn cancel_pause(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::CancelPause(run.clone()));
        ready(Ok(()))
    }

    fn resume(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Resume(run.clone()));
        ready(Ok(()))
    }

    fn stop(&self, run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Stop(run.clone()));
        ready(self.refuses(run).map_or(Ok(()), Err))
    }

    fn stop_session(
        &self,
        run: &RunId,
        session: &SessionKey,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::StopSession {
            run: run.clone(),
            session: session.clone(),
        });
        ready(Ok(()))
    }

    fn restart_session(
        &self,
        run: &RunId,
        session: &SessionKey,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::RestartSession {
            run: run.clone(),
            session: session.clone(),
        });
        ready(self.refuses(run).map_or(Ok(()), Err))
    }

    fn reassign(
        &self,
        run: &RunId,
        task: &TaskId,
        to: ModelRef,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Reassign {
            run: run.clone(),
            task: task.clone(),
            to,
        });
        ready(Ok(()))
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<RunSummary>, ApiError>> {
        self.record(Call::List);
        ready(Ok(lock(&self.runs).clone()))
    }

    fn continue_run(
        &self,
        merged: &RunId,
        request: StartRun,
    ) -> BoxFuture<'_, Result<RunHandle, ApiError>> {
        self.record(Call::Continue {
            merged: merged.clone(),
            request: Box::new(request),
        });
        ready(Ok(self.new_run()))
    }

    fn status(&self, run: &RunId) -> BoxFuture<'_, Result<StatusSummary, ApiError>> {
        self.record(Call::Status(run.clone()));
        ready(Ok(lock(&self.status).clone()))
    }

    fn diff(
        &self,
        run: &RunId,
        task: &TaskId,
        scope: DiffScope,
    ) -> BoxFuture<'_, Result<DiffView, ApiError>> {
        let _ = scope;
        self.record(Call::Diff {
            run: run.clone(),
            task: task.clone(),
        });
        ready(Err(ApiError::NotNow(
            "the fake conductor has no diffs".to_owned(),
        )))
    }

    fn raw(&self, run: &RunId, session: &SessionKey) -> BoxFuture<'_, Result<RawView, ApiError>> {
        self.record(Call::Raw {
            run: run.clone(),
            session: session.clone(),
        });
        ready(Err(ApiError::NotNow(
            "the fake conductor has no raw logs".to_owned(),
        )))
    }

    fn rewind(
        &self,
        run: &RunId,
        task: &TaskId,
        checkpoint: u32,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Rewind {
            run: run.clone(),
            task: task.clone(),
            checkpoint,
        });
        ready(Ok(()))
    }

    fn spot_check(
        &self,
        run: &RunId,
        id: &ApprovalId,
        correct: bool,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::SpotCheck {
            run: run.clone(),
            id: id.clone(),
            correct,
        });
        ready(Ok(()))
    }

    fn goal(&self, run: &RunId, action: GoalAction) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Goal {
            run: run.clone(),
            action,
        });
        ready(Ok(()))
    }

    fn council(&self, run: &RunId, action: CouncilAction) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Council {
            run: run.clone(),
            action,
        });
        ready(Ok(()))
    }
}

/// An approvals call, as the fake recorded it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ApprovalCall {
    Answer {
        run: RunId,
        answer: ApprovalAnswer,
    },
    Mode {
        run: RunId,
        mode: ApprovalMode,
    },
    ReassignOverriding {
        run: RunId,
        task: TaskId,
        to: ModelRef,
        rule: String,
    },
    TakeOver {
        run: RunId,
        task: TaskId,
    },
    TakeOverEnded {
        run: RunId,
        task: TaskId,
    },
}

#[derive(Default)]
pub(crate) struct FakeApprovals {
    calls: Mutex<Vec<ApprovalCall>>,
}

impl FakeApprovals {
    pub(crate) fn calls(&self) -> Vec<ApprovalCall> {
        lock(&self.calls).clone()
    }

    fn record(&self, call: ApprovalCall) {
        lock(&self.calls).push(call);
    }
}

impl Approvals for FakeApprovals {
    fn answer(&self, run: &RunId, answer: ApprovalAnswer) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(ApprovalCall::Answer {
            run: run.clone(),
            answer,
        });
        ready(Ok(()))
    }

    fn set_approval_mode(
        &self,
        run: &RunId,
        mode: ApprovalMode,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(ApprovalCall::Mode {
            run: run.clone(),
            mode,
        });
        ready(Ok(()))
    }

    fn reassign_overriding(
        &self,
        run: &RunId,
        task: &TaskId,
        to: ModelRef,
        rule: String,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(ApprovalCall::ReassignOverriding {
            run: run.clone(),
            task: task.clone(),
            to,
            rule,
        });
        ready(Ok(()))
    }

    fn take_over(
        &self,
        run: &RunId,
        task: &TaskId,
    ) -> BoxFuture<'_, Result<TakeOverTicket, ApiError>> {
        self.record(ApprovalCall::TakeOver {
            run: run.clone(),
            task: task.clone(),
        });
        ready(Err(ApiError::NotNow(
            "the fake approvals can't take over".to_owned(),
        )))
    }

    fn take_over_ended(&self, run: &RunId, task: &TaskId) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(ApprovalCall::TakeOverEnded {
            run: run.clone(),
            task: task.clone(),
        });
        ready(Ok(()))
    }
}

/// A test app's `--set` flags: `flags`, with asking the model servers on this computer that
/// have no table turned off unless `flags` decides it, so no test reaches a server of yours.
pub(crate) fn quiet_local_servers(mut flags: Vec<String>) -> Vec<String> {
    if !flags
        .iter()
        .any(|flag| flag.starts_with("registry.local_servers="))
    {
        flags.insert(0, "registry.local_servers=false".to_owned());
    }
    flags
}

/// The real registry with discovery off, so no test runs a CLI's model listing. A test
/// that wants discovery passes its own, on fake CLIs.
pub(crate) fn quiet_registry(
    settings: &argonauts_core::settings::Settings,
    repo: &std::path::Path,
) -> Arc<dyn ModelRegistry> {
    let mut settings = settings.clone();
    settings.registry.discover = false;
    argonauts_registry::registry(&settings, Some(repo))
}

/// Runs before the controller starts, such as writing a settings file.
pub(crate) type Prepare = Box<dyn FnOnce(&TempRepo)>;

pub(crate) struct CliOptions {
    /// A person at a terminal. Without one the app holds no `Approvals`.
    pub interactive: bool,
    /// `--set` flags.
    pub flags: Vec<String>,
    pub opening: Opening,
    pub prepare: Option<Prepare>,
    /// The model registry, such as testkit's `StaticRegistry`; the real one knows no models yet.
    pub registry: Option<Arc<dyn ModelRegistry>>,
    /// This repo's runs as Argonauts Code lists them when the app opens.
    pub runs: Vec<RunSummary>,
}

impl Default for CliOptions {
    fn default() -> Self {
        Self {
            interactive: true,
            flags: Vec::new(),
            opening: Opening::default(),
            prepare: None,
            registry: None,
            runs: Vec::new(),
        }
    }
}

/// What a global file holds that a test didn't write, as after a setup that picked council.
pub(crate) const SET_UP_GLOBAL: &str = "workflow = \"council\"\n";

/// The line that keeps a test's global file on the built-in default workflow, which nobody chose.
const ON_THE_DEFAULT: &str = "# The workflow is the built-in default.\n";

/// Names council in the global file unless it names a workflow already, as after a setup: the
/// crew most of these tests drive, chosen as you would choose it. A file that says it stays on
/// the default names none.
pub(crate) fn set_up_with_council(global: &std::path::Path) {
    let text = std::fs::read_to_string(global).unwrap_or_default();
    let names_one = text
        .parse::<toml::Table>()
        .is_ok_and(|table| table.contains_key("workflow"));
    if !names_one && !text.contains(ON_THE_DEFAULT) {
        std::fs::create_dir_all(global.parent().expect("a folder")).expect("the home folder");
        std::fs::write(global, format!("{SET_UP_GLOBAL}{text}")).expect("a global config");
    }
}

/// `options` on the built-in default workflow: council, which nobody chose, so the
/// Captain-or-crew rules may leave a small change to the main agent.
pub(crate) fn on_the_default_workflow(options: CliOptions) -> CliOptions {
    let prepare = options.prepare;
    CliOptions {
        prepare: Some(Box::new(move |repo: &TempRepo| {
            if let Some(prepare) = prepare {
                prepare(repo);
            }
            let global = repo.global_config();
            let text = std::fs::read_to_string(&global).unwrap_or_default();
            std::fs::create_dir_all(global.parent().expect("a folder")).expect("the home folder");
            std::fs::write(&global, format!("{ON_THE_DEFAULT}{text}")).expect("a global config");
        })),
        ..options
    }
}

/// A `Ctx` in a temp repo and home, with a fake conductor, fake approvals, and a fake default
/// agent (`claude-code`) whose sessions a test scripts. The receivers hold what the handlers
/// send, for tests that call them directly.
pub(crate) struct TestCtx {
    pub repo: TempRepo,
    pub ctx: Ctx,
    pub events: mpsc::UnboundedReceiver<AppEvent>,
    pub internal: mpsc::UnboundedReceiver<Internal>,
    pub conductor: Arc<FakeConductor>,
    pub approvals: Arc<FakeApprovals>,
    pub directory: Arc<FakeDirectory>,
    pub agent: Arc<FakeRunner>,
    pub env: Arc<AppEnv>,
    pub plan_usage: Arc<MemoryPlanUsageBoard>,
    pub health: Arc<MemoryHealthBoard>,
}

impl TestCtx {
    pub(crate) fn new(options: CliOptions) -> (Self, Opening) {
        let repo = TempRepo::new().expect("a temp repo");
        if let Some(prepare) = options.prepare {
            prepare(&repo);
        }
        set_up_with_council(&repo.global_config());
        let paths = Paths::new(repo.home());
        let mut env = AppEnv::for_repo(paths.clone(), repo.root(), options.interactive)
            .expect("the app environment");
        env.agent_session = false;
        env.user_home = Some(repo.user_home().to_owned());
        let env = Arc::new(env);
        let flags = quiet_local_servers(options.flags);
        let settings =
            AppSettings::load(&env, flags, options.interactive).expect("settings that resolve");
        let directory = Arc::new(FakeDirectory::new());
        let agent = directory.add(FakeRunner::cli("claude-code", "anthropic"));
        let conductor = Arc::new(FakeConductor::default());
        conductor.set_runs(options.runs);
        let approvals = Arc::new(FakeApprovals::default());
        let plan_usage = Arc::new(MemoryPlanUsageBoard::default());
        let health = Arc::new(MemoryHealthBoard::default());
        let registry = options
            .registry
            .unwrap_or_else(|| quiet_registry(settings.settings(), repo.root()));
        let services = AppServices {
            conductor: conductor.clone(),
            approvals: options
                .interactive
                .then(|| approvals.clone() as Arc<dyn Approvals>),
            workers: directory.clone(),
            steps: Arc::new(SchemaStepRunner),
            registry,
            plan_usage: plan_usage.clone(),
            health: health.clone(),
            clock: Arc::new(SystemClock),
        };
        let (events_sender, events) = mpsc::unbounded_channel();
        let out = Outbox::new(events_sender, Arc::new(Trace::off()));
        let (internal_sender, internal) = mpsc::unbounded_channel();
        let ctx = Ctx::new(env.clone(), settings, services, out, internal_sender);
        let parts = Self {
            repo,
            ctx,
            events,
            internal,
            conductor,
            approvals,
            directory,
            agent,
            env,
            plan_usage,
            health,
        };
        (parts, options.opening)
    }
}

/// The real controller loop in a temp repo and home, with a fake conductor, fake approvals,
/// and a fake main agent (`claude-code`) whose sessions a test scripts.
pub(crate) struct TestDriver {
    pub repo: TempRepo,
    pub conductor: Arc<FakeConductor>,
    pub approvals: Arc<FakeApprovals>,
    pub directory: Arc<FakeDirectory>,
    pub agent: Arc<FakeRunner>,
    pub env: Arc<AppEnv>,
    pub plan_usage: Arc<MemoryPlanUsageBoard>,
    pub health: Arc<MemoryHealthBoard>,
    /// What the controller knows of the CLIs, which the app's own start fills from Doctor.
    pub cli_facts: Arc<crate::controller::panels::status_tab::CliFacts>,
    commands: mpsc::Sender<UiCommand>,
    events: mpsc::UnboundedReceiver<AppEvent>,
    seen: Vec<AppEvent>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl TestDriver {
    pub(crate) async fn start() -> Self {
        Self::start_with(CliOptions::default()).await
    }

    pub(crate) async fn start_with(options: CliOptions) -> Self {
        let (parts, opening) = TestCtx::new(options);
        let cli_facts = Arc::clone(&parts.ctx.cli_facts);
        let controller = Controller::new(parts.ctx, parts.internal).expect("a controller");
        let (commands, commands_receiver) = mpsc::channel(64);
        let task = tokio::spawn(controller.run(commands_receiver, opening));
        Self {
            repo: parts.repo,
            conductor: parts.conductor,
            approvals: parts.approvals,
            directory: parts.directory,
            agent: parts.agent,
            env: parts.env,
            plan_usage: parts.plan_usage,
            health: parts.health,
            cli_facts,
            commands,
            events: parts.events,
            seen: Vec::new(),
            task,
        }
    }

    pub(crate) async fn send(&self, command: UiCommand) {
        self.commands
            .send(command)
            .await
            .expect("the controller is still running");
    }

    pub(crate) async fn submit(&self, text: &str) {
        self.send(UiCommand::Submit {
            text: text.to_owned(),
            workflow_requested: false,
            pictures: Vec::new(),
        })
        .await;
    }

    /// Waits until the controller has taken every command sent so far. It handles a command
    /// whole before it reads anything else, so what the command does comes before any session
    /// event a test lets go after this.
    pub(crate) async fn taken(&self) {
        let wait = max_wait();
        let deadline = tokio::time::Instant::now() + wait;
        while self.commands.capacity() < self.commands.max_capacity() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the controller took no command within {wait:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The next event. Fails the test after [`max_wait`], or when the controller stopped.
    pub(crate) async fn next(&mut self) -> AppEvent {
        self.next_within(max_wait()).await
    }

    async fn next_within(&mut self, wait: Duration) -> AppEvent {
        let event = tokio::time::timeout(wait, self.events.recv())
            .await
            .unwrap_or_else(|_| panic!("no event within {wait:?}; seen so far: {:#?}", self.seen))
            .expect("the controller stopped sending events");
        self.seen.push(event.clone());
        event
    }

    /// Reads events until one matches, and returns it.
    pub(crate) async fn until(&mut self, what: impl Fn(&AppEvent) -> bool) -> AppEvent {
        self.until_within(max_wait(), what).await
    }

    /// As [`Self::until`], giving each event up to `wait`: for a test that starts real
    /// processes, which take their time while every other test runs beside it.
    pub(crate) async fn until_within(
        &mut self,
        wait: Duration,
        what: impl Fn(&AppEvent) -> bool,
    ) -> AppEvent {
        loop {
            let event = self.next_within(wait).await;
            if what(&event) {
                return event;
            }
        }
    }

    /// Every event read so far, oldest first.
    pub(crate) fn seen(&self) -> &[AppEvent] {
        &self.seen
    }

    /// Sends `Exit`, waits for the loop to end, and returns every event it sent.
    pub(crate) async fn exit(mut self) -> Vec<AppEvent> {
        self.send(UiCommand::Exit).await;
        let result = tokio::time::timeout(max_wait(), &mut self.task)
            .await
            .expect("the controller exits")
            .expect("the controller task doesn't panic");
        result.expect("the controller ends cleanly");
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
        self.seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_app_asks_no_server_of_yours_unless_it_says_so() {
        assert_eq!(
            quiet_local_servers(vec!["ui.bell=false".to_owned()]),
            ["registry.local_servers=false", "ui.bell=false"]
        );
        let asked = vec!["registry.local_servers=true".to_owned()];
        assert_eq!(quiet_local_servers(asked.clone()), asked);
    }

    #[tokio::test]
    async fn the_cli_opens_with_a_header_and_bottom_line_and_exits() {
        let mut driver = TestDriver::start().await;
        driver
            .until(|event| matches!(event, AppEvent::Status { .. }))
            .await;
        let events = driver.exit().await;
        assert!(matches!(events.last(), Some(AppEvent::Exit)), "{events:#?}");
    }
}
