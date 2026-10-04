//! Test doubles for the controller: a conductor and approvals that record every call, and a
//! harness that runs the real controller loop against them and testkit's fake workers.

// Shared by many tests, and not every test reads every part.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::api::{
    ApiError, Approvals, CheckpointAnswer, ConductorApi, DiffScope, DiffView, EventStream, RawView,
    RunHandle, RunSummary, StartRun, StatusSummary, SteerAck, TakeOverTicket,
};
use argonauts_core::app::{AppEvent, UiCommand};
use argonauts_core::clock::SystemClock;
use argonauts_core::events::RunEvent;
use argonauts_core::events::goal::GoalAction;
use argonauts_core::events::session::QueuedMessage;
use argonauts_core::future::BoxFuture;
use argonauts_core::input::MentionTarget;
use argonauts_core::names::{CheckpointId, MessageId, SessionKey, WorkerRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::steps::SchemaStepRunner;
use argonauts_core::{CardId, Paths, RunId};
use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};
use argonauts_testkit::{FakeDirectory, FakeWorker, TempRepo};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::{AppServices, Controller, Ctx, Internal, Opening, Outbox};
use crate::app_settings::AppSettings;
use crate::env::AppEnv;
use crate::trace::Trace;

/// How long a test waits for the next event before failing.
pub(crate) const WAIT: Duration = Duration::from_secs(5);

/// A conductor call, as the fake recorded it.
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
    Reassign {
        run: RunId,
        card: CardId,
        to: WorkerRef,
    },
    List,
    Continue {
        merged: RunId,
        request: Box<StartRun>,
    },
    Status(RunId),
    Diff {
        run: RunId,
        card: CardId,
    },
    Raw {
        run: RunId,
        session: SessionKey,
    },
    Revert {
        run: RunId,
        card: CardId,
        checkpoint: u32,
    },
    SpotCheck {
        run: RunId,
        id: CheckpointId,
        correct: bool,
    },
    Goal {
        run: RunId,
        action: GoalAction,
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
}

impl FakeConductor {
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

    /// Closes the run's stream; with `crashed`, the supervisor went away first.
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
        let (live, live_receiver) = watch::channel(true);
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
        ready(Ok(()))
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

    fn reassign(
        &self,
        run: &RunId,
        card: &CardId,
        to: WorkerRef,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Reassign {
            run: run.clone(),
            card: card.clone(),
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
        card: &CardId,
        scope: DiffScope,
    ) -> BoxFuture<'_, Result<DiffView, ApiError>> {
        let _ = scope;
        self.record(Call::Diff {
            run: run.clone(),
            card: card.clone(),
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

    fn revert(
        &self,
        run: &RunId,
        card: &CardId,
        checkpoint: u32,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(Call::Revert {
            run: run.clone(),
            card: card.clone(),
            checkpoint,
        });
        ready(Ok(()))
    }

    fn spot_check(
        &self,
        run: &RunId,
        id: &CheckpointId,
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
}

/// An approvals call, as the fake recorded it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ApprovalCall {
    Answer {
        run: RunId,
        answer: CheckpointAnswer,
    },
    Mode {
        run: RunId,
        mode: ApprovalMode,
    },
    ReassignOverriding {
        run: RunId,
        card: CardId,
        to: WorkerRef,
        rule: String,
    },
    TakeOver {
        run: RunId,
        card: CardId,
    },
    TakeOverEnded {
        run: RunId,
        card: CardId,
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
    fn answer(&self, run: &RunId, answer: CheckpointAnswer) -> BoxFuture<'_, Result<(), ApiError>> {
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
        card: &CardId,
        to: WorkerRef,
        rule: String,
    ) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(ApprovalCall::ReassignOverriding {
            run: run.clone(),
            card: card.clone(),
            to,
            rule,
        });
        ready(Ok(()))
    }

    fn take_over(
        &self,
        run: &RunId,
        card: &CardId,
    ) -> BoxFuture<'_, Result<TakeOverTicket, ApiError>> {
        self.record(ApprovalCall::TakeOver {
            run: run.clone(),
            card: card.clone(),
        });
        ready(Err(ApiError::NotNow(
            "the fake approvals can't take over".to_owned(),
        )))
    }

    fn take_over_ended(&self, run: &RunId, card: &CardId) -> BoxFuture<'_, Result<(), ApiError>> {
        self.record(ApprovalCall::TakeOverEnded {
            run: run.clone(),
            card: card.clone(),
        });
        ready(Ok(()))
    }
}

/// The real registry with discovery off, so no test runs a vendor CLI's model listing. A test
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

pub(crate) struct HarnessOptions {
    /// A person at a terminal. Without one the app holds no `Approvals`.
    pub interactive: bool,
    /// `--set` flags.
    pub flags: Vec<String>,
    pub opening: Opening,
    pub prepare: Option<Prepare>,
    /// The model registry, such as testkit's `StaticRegistry`; the real one knows no models yet.
    pub registry: Option<Arc<dyn ModelRegistry>>,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            interactive: true,
            flags: Vec::new(),
            opening: Opening::default(),
            prepare: None,
            registry: None,
        }
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
    pub agent: Arc<FakeWorker>,
    pub env: Arc<AppEnv>,
    pub quota: Arc<MemoryQuotaBoard>,
    pub health: Arc<MemoryHealthBoard>,
}

impl TestCtx {
    pub(crate) fn new(options: HarnessOptions) -> (Self, Opening) {
        let repo = TempRepo::new().expect("a temp repo");
        if let Some(prepare) = options.prepare {
            prepare(&repo);
        }
        let paths = Paths::new(repo.home());
        let mut env = AppEnv::for_repo(paths.clone(), repo.root(), options.interactive)
            .expect("the app environment");
        env.agent_session = false;
        env.user_home = Some(repo.user_home().to_owned());
        let env = Arc::new(env);
        let settings = AppSettings::load(&env, options.flags, options.interactive)
            .expect("settings that resolve");
        let directory = Arc::new(FakeDirectory::new());
        let agent = directory.add(FakeWorker::harness("claude-code", "claude"));
        let conductor = Arc::new(FakeConductor::default());
        let approvals = Arc::new(FakeApprovals::default());
        let quota = Arc::new(MemoryQuotaBoard::default());
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
            quota: quota.clone(),
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
            quota,
            health,
        };
        (parts, options.opening)
    }
}

/// The real controller loop in a temp repo and home, with a fake conductor, fake approvals,
/// and a fake default agent (`claude-code`) whose sessions a test scripts.
pub(crate) struct Harness {
    pub repo: TempRepo,
    pub conductor: Arc<FakeConductor>,
    pub approvals: Arc<FakeApprovals>,
    pub directory: Arc<FakeDirectory>,
    pub agent: Arc<FakeWorker>,
    pub env: Arc<AppEnv>,
    pub quota: Arc<MemoryQuotaBoard>,
    pub health: Arc<MemoryHealthBoard>,
    commands: mpsc::Sender<UiCommand>,
    events: mpsc::UnboundedReceiver<AppEvent>,
    seen: Vec<AppEvent>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    pub(crate) async fn start() -> Self {
        Self::start_with(HarnessOptions::default()).await
    }

    pub(crate) async fn start_with(options: HarnessOptions) -> Self {
        let (parts, opening) = TestCtx::new(options);
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
            quota: parts.quota,
            health: parts.health,
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
        })
        .await;
    }

    /// The next event. Fails the test after [`WAIT`], or when the controller stopped.
    pub(crate) async fn next(&mut self) -> AppEvent {
        let event = tokio::time::timeout(WAIT, self.events.recv())
            .await
            .unwrap_or_else(|_| panic!("no event within {WAIT:?}; seen so far: {:#?}", self.seen))
            .expect("the controller stopped sending events");
        self.seen.push(event.clone());
        event
    }

    /// Reads events until one matches, and returns it.
    pub(crate) async fn until(&mut self, what: impl Fn(&AppEvent) -> bool) -> AppEvent {
        loop {
            let event = self.next().await;
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
        let result = tokio::time::timeout(WAIT, &mut self.task)
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

    #[tokio::test]
    async fn the_harness_opens_with_a_status_line_and_exits() {
        let mut harness = Harness::start().await;
        harness
            .until(|event| matches!(event, AppEvent::Status { .. }))
            .await;
        let events = harness.exit().await;
        assert!(matches!(events.last(), Some(AppEvent::Exit)), "{events:#?}");
    }
}
