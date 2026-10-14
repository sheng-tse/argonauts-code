//! In-process fake workers that play scripted sessions and record every spec and control
//! message. They keep the worker contract: the first event is `Session(Input)`, a delivered
//! message emits `Delivered` and a fresh `Input`, dropping the control sender stops the session,
//! `Build` and `Experiment` need the goal hash, and an endpoint takes only `NoTools` sessions.
//! `lenient()` turns the refusals off.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use argonauts_core::events::AgentStatusEvent;
use argonauts_core::events::session::{
    EndReason, InputPart, InputPartKind, LaunchRecord, PromptVia, QueuedMessage, SessionEnd,
    SessionEvent, SessionInput,
};
use argonauts_core::future::BoxFuture;
use argonauts_core::names::{AgentId, ModelRef, SessionKey};
use argonauts_core::session_runner::{
    Access, RunnerCapabilities, RunnerDirectory, RunnerError, RunnerEvent, RunnerKind,
    SessionControl, SessionHandle, SessionRunner, SessionSpec, SteerSupport, SubagentVisibility,
};
use argonauts_core::{AgentEvent, CommandSpec, Spend};
use serde_json::Value;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{mpsc, oneshot};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A `SessionEnd` with placeholder values besides the reason and output.
pub fn session_end(
    reason: EndReason,
    result_text: Option<&str>,
    structured: Option<Value>,
) -> SessionEnd {
    SessionEnd {
        cli_session: Some("fake-session".to_owned()),
        exit_code: Some(0),
        signal: None,
        duration_ms: 1,
        reason,
        spend: Spend::default(),
        tool_calls: 0,
        result_text: result_text.map(str::to_owned),
        structured,
        last_message: result_text.map(str::to_owned),
        raw_log: None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FakeSession {
    /// Events after the `Session(Input)` record.
    pub events: Vec<RunnerEvent>,
    pub end: SessionEnd,
    /// A pause before each event, to let tests watch a session mid-run.
    pub event_delay: Duration,
    /// After its events, keep running until told to stop, like an agent mid-task.
    pub until_stopped: bool,
}

impl FakeSession {
    /// Streams `text`, then ends with it as the result.
    pub fn completed(text: &str) -> Self {
        Self {
            events: vec![
                RunnerEvent::Agent(AgentEvent::TextDelta {
                    text: text.to_owned(),
                    parent: None,
                }),
                RunnerEvent::Agent(AgentEvent::Result {
                    text: Some(text.to_owned()),
                    structured: None,
                    is_error: false,
                }),
            ],
            end: session_end(EndReason::Completed, Some(text), None),
            event_delay: Duration::ZERO,
            until_stopped: false,
        }
    }

    /// Ends with `value` as the structured output, as a schema-checked step expects.
    pub fn structured(value: Value) -> Self {
        let text = value.to_string();
        Self {
            events: vec![RunnerEvent::Agent(AgentEvent::Result {
                text: Some(text.clone()),
                structured: Some(value.clone()),
                is_error: false,
            })],
            end: session_end(EndReason::Completed, Some(&text), Some(value)),
            event_delay: Duration::ZERO,
            until_stopped: false,
        }
    }

    /// Ends for `reason` with no output, such as a usage limit.
    pub fn ended(reason: EndReason) -> Self {
        Self {
            events: Vec::new(),
            end: session_end(reason, None, None),
            event_delay: Duration::ZERO,
            until_stopped: false,
        }
    }

    /// Plays `events`, then keeps running until it gets `Stop` or `StopAfterTool` (or its
    /// control sender is dropped), and ends `Stopped`.
    pub fn until_stopped(events: Vec<RunnerEvent>) -> Self {
        Self {
            events,
            end: session_end(EndReason::Stopped, None, None),
            event_delay: Duration::ZERO,
            until_stopped: true,
        }
    }

    /// Adds an event before the `Result`, if there is one.
    pub fn with_event(mut self, event: RunnerEvent) -> Self {
        let at = self
            .events
            .iter()
            .position(|e| matches!(e, RunnerEvent::Agent(AgentEvent::Result { .. })))
            .unwrap_or(self.events.len());
        self.events.insert(at, event);
        self
    }

    pub fn with_cli_session(mut self, id: &str) -> Self {
        self.end.cli_session = Some(id.to_owned());
        self
    }

    pub fn with_spend(mut self, spend: Spend) -> Self {
        self.end.spend = spend;
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.event_delay = delay;
        self
    }
}

type Responder = Box<dyn Fn(&SessionSpec) -> FakeSession + Send + Sync>;

enum Script {
    Queue(VecDeque<FakeSession>),
    Respond(Responder),
}

pub struct FakeRunner {
    worker: ModelRef,
    kind: RunnerKind,
    provider: String,
    capabilities: RunnerCapabilities,
    enforce_rules: bool,
    cheapest: Option<String>,
    logged_in: Option<bool>,
    wont_start: Option<String>,
    script: Mutex<Script>,
    specs: Mutex<Vec<SessionSpec>>,
    controls: Arc<Mutex<Vec<(SessionKey, SessionControl)>>>,
}

impl FakeRunner {
    /// A CLI's runner, such as `FakeRunner::cli("codex/gpt-x", "openai")`.
    pub fn cli(worker: &str, provider: &str) -> Self {
        Self::new(worker, provider, RunnerKind::Cli)
    }

    /// An endpoint's runner, such as `FakeRunner::endpoint("openrouter/m-a", "zhipu")`.
    pub fn endpoint(worker: &str, provider: &str) -> Self {
        Self::new(worker, provider, RunnerKind::Endpoint)
    }

    fn new(worker: &str, provider: &str, kind: RunnerKind) -> Self {
        let cli = kind == RunnerKind::Cli;
        Self {
            worker: worker
                .parse()
                .unwrap_or_else(|_| ModelRef::new(worker, None)),
            kind,
            provider: provider.to_owned(),
            capabilities: RunnerCapabilities {
                streams_text: true,
                steer: if cli {
                    SteerSupport::Input
                } else {
                    SteerSupport::NextTurnOnly
                },
                resume: cli,
                fork: false,
                structured_output: true,
                status_tool: cli,
                subagents: if cli {
                    SubagentVisibility::Full
                } else {
                    SubagentVisibility::None
                },
                native_limit_cap: false,
                take_over: cli,
                can_build: cli,
            },
            enforce_rules: true,
            cheapest: None,
            logged_in: None,
            wont_start: None,
            script: Mutex::new(Script::Queue(VecDeque::new())),
            specs: Mutex::new(Vec::new()),
            controls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_capabilities(mut self, capabilities: RunnerCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// The model its CLI names as its cheapest, such as `gpt-6-luna`.
    pub fn with_cheapest_model(mut self, model: &str) -> Self {
        self.cheapest = Some(model.to_owned());
        self
    }

    /// What its CLI's own login check says: `Some(false)` is logged out.
    pub fn with_login(mut self, logged_in: Option<bool>) -> Self {
        self.logged_in = logged_in;
        self
    }

    /// It refuses every session before starting one, for a reason known beforehand, as
    /// OpenCode does with no model set.
    pub fn refusing_to_start(mut self, why: &str) -> Self {
        self.wont_start = Some(why.to_owned());
        self
    }

    /// Queues a session.
    pub fn then(self, session: FakeSession) -> Self {
        self.push(session);
        self
    }

    /// Queues a session on a shared worker.
    pub fn push(&self, session: FakeSession) {
        let mut script = lock(&self.script);
        match &mut *script {
            Script::Queue(queue) => queue.push_back(session),
            Script::Respond(_) => *script = Script::Queue(VecDeque::from([session])),
        }
    }

    /// Answers every session with `respond`, instead of a queue.
    pub fn respond_with(
        self,
        respond: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static,
    ) -> Self {
        *lock(&self.script) = Script::Respond(Box::new(respond));
        self
    }

    /// Like [`FakeRunner::respond_with`], for a worker already in a directory.
    pub fn respond(&self, respond: impl Fn(&SessionSpec) -> FakeSession + Send + Sync + 'static) {
        *lock(&self.script) = Script::Respond(Box::new(respond));
    }

    /// Stops refusing sessions that break the worker rules.
    pub fn lenient(mut self) -> Self {
        self.enforce_rules = false;
        self
    }

    pub fn specs(&self) -> Vec<SessionSpec> {
        lock(&self.specs).clone()
    }

    pub fn controls(&self) -> Vec<(SessionKey, SessionControl)> {
        lock(&self.controls).clone()
    }

    fn refusal(&self, spec: &SessionSpec) -> Option<String> {
        if let Some(why) = &self.wont_start {
            return Some(why.clone());
        }
        if !self.enforce_rules {
            return None;
        }
        if spec.access.writes() && spec.goal_sha256.is_none() {
            return Some("a session that changes files needs the approved goal's hash".to_owned());
        }
        if self.kind == RunnerKind::Endpoint && spec.access != Access::NoTools {
            return Some("an endpoint takes only no-tools sessions".to_owned());
        }
        None
    }

    fn next_session(&self, spec: &SessionSpec) -> Option<FakeSession> {
        match &mut *lock(&self.script) {
            Script::Queue(queue) => queue.pop_front(),
            Script::Respond(respond) => Some(respond(spec)),
        }
    }
}

fn input_record(spec: &SessionSpec, kind: RunnerKind) -> SessionInput {
    // The worker sets ARGONAUTS_SESSION itself, over anything the spec gave.
    let mut env_set: Vec<String> = spec
        .env
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|name| name != "ARGONAUTS_SESSION")
        .collect();
    env_set.push("ARGONAUTS_SESSION".to_owned());
    SessionInput {
        launch: LaunchRecord {
            program: format!("fake-{}", spec.model.agent),
            args: vec!["<prompt>".to_owned()],
            cwd: spec.workdir.clone(),
            env_set,
            env_removed: Vec::new(),
            prompt_via: match kind {
                RunnerKind::Cli => PromptVia::Argument,
                RunnerKind::Endpoint => PromptVia::Request,
            },
            sandbox: None,
        },
        parts: spec.prompt.clone(),
        queued: Vec::new(),
    }
}

impl SessionRunner for FakeRunner {
    fn model_ref(&self) -> &ModelRef {
        &self.worker
    }

    fn kind(&self) -> RunnerKind {
        self.kind
    }

    fn agent_id(&self) -> AgentId {
        self.worker.agent_id()
    }

    fn provider(&self) -> String {
        self.provider.clone()
    }

    fn capabilities(&self) -> RunnerCapabilities {
        self.capabilities
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, RunnerError>> {
        lock(&self.specs).push(spec.clone());
        let refusal = self.refusal(&spec);
        let session = self.next_session(&spec);
        let worker = self.worker.clone();
        let kind = self.kind;
        let controls = self.controls.clone();
        Box::pin(async move {
            if let Some(reason) = refusal {
                return Err(RunnerError::Refused(reason));
            }
            let Some(session) = session else {
                return Err(RunnerError::Refused(format!(
                    "fake model {worker} has no scripted session left"
                )));
            };
            let (events_tx, events) = mpsc::channel(64);
            let (control, mut control_rx) = mpsc::channel::<SessionControl>(16);
            let (ended_tx, ended) = oneshot::channel();
            let key = spec.key.clone();
            let record = input_record(&spec, kind);
            let input = RunnerEvent::Session(SessionEvent::Input(Box::new(record.clone())));
            tokio::spawn(async move {
                let mut end = session.end;
                let mut stopped = false;
                let mut deliveries = Vec::new();
                // The input record always comes first, whatever arrives on the control channel.
                let _ = events_tx.send(input).await;
                for event in session.events {
                    if !session.event_delay.is_zero() {
                        tokio::time::sleep(session.event_delay).await;
                    }
                    loop {
                        match control_rx.try_recv() {
                            Ok(message) => {
                                stopped |= is_stop(&message);
                                deliveries.extend(delivered(&message, &record));
                                lock(&controls).push((spec.key.clone(), message));
                            }
                            // Nobody holds the control sender any more: that means stop.
                            Err(TryRecvError::Disconnected) => {
                                stopped = true;
                                break;
                            }
                            Err(TryRecvError::Empty) => break,
                        }
                    }
                    for delivery in deliveries.drain(..) {
                        let _ = events_tx.send(delivery).await;
                    }
                    if stopped {
                        end = session_end(EndReason::Stopped, None, None);
                        break;
                    }
                    if events_tx.send(event).await.is_err() {
                        break;
                    }
                }
                if session.until_stopped && !stopped {
                    while let Some(message) = control_rx.recv().await {
                        let stop = is_stop(&message);
                        for delivery in delivered(&message, &record) {
                            let _ = events_tx.send(delivery).await;
                        }
                        lock(&controls).push((spec.key.clone(), message));
                        if stop {
                            break;
                        }
                    }
                }
                while let Ok(message) = control_rx.try_recv() {
                    lock(&controls).push((spec.key.clone(), message));
                }
                // The end goes out only after the last event, as the contract says.
                drop(events_tx);
                let _ = ended_tx.send(end);
            });
            Ok(SessionHandle {
                key,
                events,
                control,
                ended,
            })
        })
    }

    fn take_over_command(&self, cli_session: &str, workdir: &Path) -> Option<CommandSpec> {
        (self.kind == RunnerKind::Cli).then(|| {
            CommandSpec::new(format!("fake-{}", self.worker.agent), workdir)
                .args(["resume", cli_session])
        })
    }

    fn cheapest_model(&self) -> Option<String> {
        self.cheapest.clone()
    }

    fn start_refusal(&self) -> Option<String> {
        self.wont_start.clone()
    }

    fn logged_in(&self) -> BoxFuture<'_, Option<bool>> {
        let logged_in = self.logged_in;
        Box::pin(async move { logged_in })
    }
}

fn is_stop(message: &SessionControl) -> bool {
    matches!(
        message,
        SessionControl::Stop | SessionControl::StopAfterTool
    )
}

/// What a delivered message adds to the record: `Delivered`, then an `Input` holding only it.
fn delivered(message: &SessionControl, record: &SessionInput) -> Vec<RunnerEvent> {
    let SessionControl::Deliver(message) = message else {
        return Vec::new();
    };
    vec![
        RunnerEvent::Session(SessionEvent::Delivered {
            message: message.id.clone(),
            via: message.delivery,
        }),
        RunnerEvent::Session(SessionEvent::Input(Box::new(SessionInput {
            launch: record.launch.clone(),
            parts: vec![message_part(message)],
            queued: Vec::new(),
        }))),
    ]
}

fn message_part(message: &QueuedMessage) -> InputPart {
    InputPart {
        kind: InputPartKind::Message,
        label: format!("message {}", message.id.as_str()),
        text: message.text.clone(),
        approx_tokens: (message.text.len() / 4) as u32,
        source: None,
    }
}

#[derive(Default)]
pub struct FakeDirectory {
    workers: Mutex<Vec<Arc<dyn SessionRunner>>>,
    fakes: Mutex<Vec<Arc<FakeRunner>>>,
    /// Worker ids whose CLI is gone from this machine.
    uninstalled: Mutex<Vec<String>>,
    /// Worker ids whose CLI keeps your plugins out of an isolated session.
    isolating: Mutex<Vec<String>>,
    taking_tools: Mutex<Vec<String>>,
}

impl FakeDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the shared handle, for inspecting it later.
    pub fn add(&self, worker: FakeRunner) -> Arc<FakeRunner> {
        let worker = Arc::new(worker);
        lock(&self.fakes).push(worker.clone());
        lock(&self.workers).push(worker.clone());
        worker
    }

    pub fn add_worker(&self, worker: Arc<dyn SessionRunner>) {
        lock(&self.workers).push(worker);
    }

    /// `worker`'s CLI is gone: `get` still builds it, as the real directory does for any
    /// built-in adapter, but `available` no longer lists it.
    pub fn uninstall(&self, worker: &str) {
        lock(&self.uninstalled).push(worker.to_owned());
    }

    /// `worker`'s CLI keeps your plugins out of an isolated session, as Claude Code's does.
    pub fn isolating(&self, worker: &str) {
        lock(&self.isolating).push(worker.to_owned());
    }

    /// `worker`'s CLI takes Argonauts Code's tools when driven, as Claude Code's does, so a
    /// conversation with it is the CLI itself.
    pub fn taking_tools(&self, worker: &str) {
        lock(&self.taking_tools).push(worker.to_owned());
    }

    pub fn fake(&self, worker: &str) -> Option<Arc<FakeRunner>> {
        let wanted: ModelRef = worker.parse().ok()?;
        lock(&self.fakes)
            .iter()
            .find(|fake| fake.worker == wanted)
            .cloned()
    }
}

impl RunnerDirectory for FakeDirectory {
    /// An exact match first; else one with the same worker id where either side leaves the
    /// model open.
    fn get(&self, worker: &ModelRef) -> Result<Arc<dyn SessionRunner>, RunnerError> {
        let workers = lock(&self.workers);
        let exact = workers.iter().find(|w| w.model_ref() == worker);
        let loose = || {
            workers.iter().find(|w| {
                let known = w.model_ref();
                known.agent == worker.agent && (known.model.is_none() || worker.model.is_none())
            })
        };
        exact
            .or_else(loose)
            .cloned()
            .ok_or_else(|| RunnerError::Unknown(worker.clone()))
    }

    fn available(&self) -> Vec<ModelRef> {
        let uninstalled = lock(&self.uninstalled);
        lock(&self.workers)
            .iter()
            .map(|w| w.model_ref().clone())
            .filter(|worker| !uninstalled.contains(&worker.agent))
            .collect()
    }

    fn takes_session_tools(&self, worker: &ModelRef) -> bool {
        lock(&self.taking_tools).contains(&worker.agent)
    }

    /// As the real directory: a CLI marked [`FakeDirectory::isolating`], or an endpoint, which
    /// answers with no tools, so nothing of yours reaches it.
    fn isolates(&self, worker: &ModelRef) -> bool {
        lock(&self.isolating).contains(&worker.agent)
            || self
                .get(worker)
                .is_ok_and(|found| found.kind() == RunnerKind::Endpoint)
    }
}

/// A `SessionSpec` with safe defaults, for tests that start sessions by hand.
pub fn session_spec(worker: &str, access: Access, workdir: &Path) -> SessionSpec {
    use argonauts_core::names::RoleName;
    use argonauts_core::session_runner::{SandboxProfile, SessionLimits};
    use argonauts_core::settings::sessions::{Compaction, NativeFanout, SessionLifetime};
    SessionSpec {
        run: None,
        task: None,
        key: SessionKey::numbered(1),
        role: RoleName::Main,
        step: None,
        model: worker
            .parse()
            .unwrap_or_else(|_| ModelRef::new(worker, None)),
        effort: None,
        access,
        workdir: workdir.to_owned(),
        session_dir: workdir.join(".session"),
        prompt: Vec::new(),
        json_schema: None,
        resume: None,
        fork: false,
        lifetime: SessionLifetime::Task,
        keep_input_open: false,
        limits: SessionLimits::default(),
        goal_sha256: None,
        sandbox: SandboxProfile::default(),
        compaction: Compaction::Inherit,
        native_fanout: NativeFanout::default(),
        isolated: false,
        autopilot: false,
        status_file: None,
        env: Vec::new(),
        asking: None,
        cli_workflow: argonauts_core::session_runner::CliWorkflow::default(),
        shell: argonauts_core::session_runner::SessionShell::None,
    }
}

pub async fn drain(mut handle: SessionHandle) -> (Vec<RunnerEvent>, SessionEnd) {
    let mut events = Vec::new();
    while let Some(event) = handle.events.recv().await {
        events.push(event);
    }
    let end = handle.ended.await.unwrap_or_else(|_| {
        session_end(
            EndReason::Crashed {
                message: "the fake session dropped its end".to_owned(),
            },
            None,
            None,
        )
    });
    (events, end)
}

pub fn agent_event(event: AgentStatusEvent) -> RunnerEvent {
    RunnerEvent::AgentStatus(event)
}

/// A workdir for tests that don't need a real one.
pub fn nowhere() -> PathBuf {
    PathBuf::from("/nonexistent/argonauts-test")
}
