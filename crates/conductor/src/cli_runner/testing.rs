//! Test helpers: a shell-script adapter that prints agent events as JSON lines, a fake session
//! driver, specs, and a runner with no sandbox under a temp home.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::adapter::Transport;
use argonauts_core::driver::{
    DriverControl, DriverEnd, DriverIo, DriverStart, DriverStop, RawFrame, SessionDriver,
};
use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::run::Party;
use argonauts_core::events::session::{
    Delivery, InputPartKind, QueuedMessage, SessionEnd, SessionEvent,
};
use argonauts_core::future::BoxFuture;
use argonauts_core::input::MentionTarget;
use argonauts_core::limits::{AgentHealth, HealthState};
use argonauts_core::names::{AgentId, MessageId, ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::{
    Access, RunnerEvent, SandboxProfile, SessionHandle, SessionLimits, SessionSpec,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::settings::sessions::{Compaction, NativeFanout, SessionLifetime};
use argonauts_core::{
    Adapter, AgentEvent, Capabilities, CommandSpec, Paths, SessionRequest, SessionRole, StopPlan,
    StopSignal, StopStep, StreamParser, extract_version,
};
use argonauts_testkit::services::{MemoryHealthBoard, MemoryPlanUsageBoard};
use serde_json::json;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::CliRunner;
use super::launch::part;
use crate::sandbox::Unsandboxed;

/// A fake agent: `/bin/sh -c <script> sh <prompt> <resume>`, with `FAKE_ROLE` set to
/// `read-only` or `build`. With input lines, the prompt goes on stdin as `{"text": ...}` and `$1`
/// is empty, and each steered message is one more line. Its output is agent events as JSON
/// unless another parser is set.
#[derive(Clone)]
pub(crate) struct TestAdapter {
    id: &'static str,
    binary: &'static str,
    script: String,
    input_lines: bool,
    input_joins_turn: bool,
    /// Messages carry a uuid, and send now is a line of its own, as Claude Code 2.1.289's.
    send_now: bool,
    parser: Option<fn() -> Box<dyn StreamParser>>,
    resume: bool,
    /// Carries on a copy of a session when asked, as a CLI with a fork of its own: `$3` is
    /// then `fork`.
    fork: bool,
    take_over: bool,
    build: bool,
    driver: Option<Arc<dyn SessionDriver>>,
    stop_plan: StopPlan,
    removed_env: Vec<String>,
    task_home: bool,
    heartbeat: Option<Duration>,
    isolates: bool,
    /// Asks for the context by category with a line of its own, as Claude Code's
    /// `get_context_usage`.
    context_asks: bool,
}

impl TestAdapter {
    pub(crate) fn new(script: &str) -> Self {
        Self {
            id: "fake",
            binary: "sh",
            script: script.to_owned(),
            input_lines: false,
            input_joins_turn: false,
            send_now: false,
            parser: None,
            resume: true,
            fork: false,
            take_over: false,
            build: true,
            driver: None,
            stop_plan: fast_stop_plan(),
            removed_env: Vec::new(),
            task_home: false,
            heartbeat: None,
            isolates: false,
            context_asks: false,
        }
    }

    pub(crate) fn with_context_asks(mut self) -> Self {
        self.context_asks = true;
        self
    }

    /// Keeps your plugins out of an isolated session and reports any that load, as Claude
    /// Code's does.
    pub(crate) fn isolating(mut self) -> Self {
        self.isolates = true;
        self
    }

    /// Claims a heartbeat during tool calls, as Claude Code's.
    pub(crate) fn with_heartbeat(mut self, every: Duration) -> Self {
        self.heartbeat = Some(every);
        self
    }

    /// Points `FAKE_HOME` at the folder the runner passes for a home kept for the task, as an
    /// ACP adapter with `keep = "task"` does.
    pub(crate) fn keeping_its_home_for_the_task(mut self) -> Self {
        self.task_home = true;
        self
    }

    /// Removes `names` and never sets them, as an adapter does with the keys its auth mode
    /// doesn't use.
    pub(crate) fn removing_env(mut self, names: &[&str]) -> Self {
        self.removed_env = names.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    pub(crate) fn with_id(mut self, id: &'static str) -> Self {
        self.id = id;
        self
    }

    /// Names another binary, though the script still runs under `sh`.
    pub(crate) fn with_binary(mut self, binary: &'static str) -> Self {
        self.binary = binary;
        self
    }

    pub(crate) fn with_input_lines(mut self) -> Self {
        self.input_lines = true;
        self
    }

    /// Input lines where a message sent mid-turn joins the running turn, as Claude Code's do.
    pub(crate) fn with_input_joining_the_turn(mut self) -> Self {
        self.input_lines = true;
        self.input_joins_turn = true;
        self
    }

    /// Joining input where a message carries a uuid and send now is a line of its own.
    pub(crate) fn with_send_now(mut self) -> Self {
        self = self.with_input_joining_the_turn();
        self.send_now = true;
        self
    }

    /// Reads the output with another adapter's parser, for replaying its recordings.
    pub(crate) fn with_parser(mut self, parser: fn() -> Box<dyn StreamParser>) -> Self {
        self.parser = Some(parser);
        self
    }

    pub(crate) fn without_resume(mut self) -> Self {
        self.resume = false;
        self
    }

    pub(crate) fn with_fork(mut self) -> Self {
        self.fork = true;
        self
    }

    pub(crate) fn with_take_over(mut self) -> Self {
        self.take_over = true;
        self
    }

    pub(crate) fn read_only(mut self) -> Self {
        self.build = false;
        self
    }

    pub(crate) fn with_driver(mut self, driver: impl SessionDriver + 'static) -> Self {
        self.driver = Some(Arc::new(driver));
        self
    }
}

/// Interrupt, terminate, and kill, each a fifth of a second apart.
fn fast_stop_plan() -> StopPlan {
    let step = |signal| StopStep {
        signal,
        wait: Duration::from_millis(200),
    };
    StopPlan {
        steps: vec![
            step(StopSignal::Interrupt),
            step(StopSignal::Terminate),
            step(StopSignal::Kill),
        ],
    }
}

impl Adapter for TestAdapter {
    fn id(&self) -> &'static str {
        self.id
    }
    fn display_name(&self) -> &'static str {
        "Fake Agent"
    }
    fn binary(&self) -> &'static str {
        self.binary
    }
    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        anyhow::ensure!(
            self.build || req.role == SessionRole::ReadOnly,
            "this agent only reads"
        );
        let mut spec = CommandSpec::new("/bin/sh", &req.workdir).args(["-c", &self.script, "sh"]);
        if self.input_lines {
            spec = spec.arg("");
            spec.stdin = Some(input_line(&req.prompt));
            spec.keep_stdin_open = req.keep_input_open;
        } else {
            spec = spec.arg(&req.prompt);
        }
        if self.resume {
            spec = spec.arg(req.resume_session.as_deref().unwrap_or(""));
        }
        if self.fork && req.fork_session {
            spec = spec.arg("fork");
        }
        spec.env_set = req
            .env_set
            .iter()
            .filter(|(name, _)| !self.removed_env.contains(name))
            .cloned()
            .collect();
        spec.env_remove = self.removed_env.clone();
        let role = match req.role {
            SessionRole::ReadOnly => "read-only",
            SessionRole::Build => "build",
            SessionRole::Direct => "direct",
        };
        spec.env_set.push(("FAKE_ROLE".to_owned(), role.to_owned()));
        spec.env_set
            .push(("FAKE_BUILTINS_OFF".to_owned(), req.builtins_off.join(",")));
        if let Some(home) = req.project.as_ref().filter(|_| self.task_home) {
            spec.env_set.push(("FAKE_HOME".to_owned(), home.clone()));
        }
        Ok(spec)
    }
    fn parser(&self) -> Box<dyn StreamParser> {
        match self.parser {
            Some(parser) => parser(),
            None => Box::new(JsonEventParser),
        }
    }
    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }
    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }
    fn tested_versions(&self) -> &'static [&'static str] {
        &["1.0.0"]
    }
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("tiny", Some("low"))
    }
    fn stop_plan(&self) -> StopPlan {
        self.stop_plan.clone()
    }
    fn heartbeat(&self) -> Option<Duration> {
        self.heartbeat
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            reports_cost: true,
            input_joins_running_turn: self.input_joins_turn,
            send_now: self.send_now,
            isolates: self.isolates,
            ..Capabilities::default()
        }
    }
    fn input_line(&self, message: &str) -> Option<String> {
        self.input_lines.then(|| input_line(message))
    }
    fn message_line(&self, message: &str, uuid: &str) -> Option<String> {
        self.input_lines
            .then(|| format!("{}\n", json!({ "text": message, "uuid": uuid })))
    }
    fn send_now_line(&self, uuid: &str) -> Option<String> {
        self.send_now
            .then(|| format!("{}\n", json!({ "send_now": uuid })))
    }
    fn context_usage_line(&self, detail: ContextDetail) -> Option<String> {
        self.context_asks
            .then(|| format!("{}\n", json!({ "context": detail.as_str() })))
    }
    fn interactive_resume(&self, session_id: &str, workdir: &Path) -> Option<CommandSpec> {
        self.take_over
            .then(|| CommandSpec::new("/bin/sh", workdir).arg(session_id))
    }
    fn session_driver(&self) -> Option<Arc<dyn SessionDriver>> {
        self.driver.clone()
    }
    fn transport(&self) -> Transport {
        if self.driver.is_some() {
            Transport::Acp
        } else {
            Transport::Lines
        }
    }
}

fn input_line(text: &str) -> String {
    format!("{}\n", json!({ "text": text }))
}

/// Each stdout line is an [`AgentEvent`] or a JSON array of them.
struct JsonEventParser;

impl StreamParser for JsonEventParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        parse_events(line)
    }
    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        Vec::new()
    }
}

fn parse_events(line: &str) -> Vec<AgentEvent> {
    if let Ok(events) = serde_json::from_str::<Vec<AgentEvent>>(line) {
        return events;
    }
    vec![serde_json::from_str(line).unwrap_or_else(|_| AgentEvent::unknown_line(line))]
}

/// A driver that sends `{"prompt": ...}` and `{"message": ...}` frames, reads agent events
/// back, and ends the turn at the first result.
#[derive(Clone, Default)]
pub(crate) struct FakeDriver {
    pub(crate) starts: Arc<Mutex<Vec<DriverStart>>>,
    pub(crate) controls: Arc<Mutex<Vec<String>>>,
}

impl SessionDriver for FakeDriver {
    fn run(&self, io: DriverIo, start: DriverStart) -> BoxFuture<'static, DriverEnd> {
        self.starts.lock().unwrap().push(start.clone());
        Box::pin(drive(io, start, self.controls.clone()))
    }
}

async fn drive(
    mut io: DriverIo,
    start: DriverStart,
    controls: Arc<Mutex<Vec<String>>>,
) -> DriverEnd {
    let mut cli_session = None;
    let mut stdin = io.stdin;
    let send = |line: String| {
        let _ = io.frames.send(RawFrame {
            outgoing: true,
            line: line.clone(),
        });
        format!("{line}\n")
    };
    let first = send(json!({ "prompt": start.prompt }).to_string());
    if stdin.write_all(first.as_bytes()).await.is_err() {
        return DriverEnd {
            cli_session,
            stop: DriverStop::Disconnected,
        };
    }
    let mut lines = BufReader::new(io.stdout).lines();
    let mut control_open = true;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else {
                    return DriverEnd { cli_session, stop: DriverStop::Disconnected };
                };
                let _ = io.frames.send(RawFrame { outgoing: false, line: line.clone() });
                for event in parse_events(&line) {
                    if let AgentEvent::Init { session_id: Some(id), .. } = &event {
                        cli_session = Some(id.clone());
                    }
                    let ended = matches!(event, AgentEvent::Result { .. });
                    let _ = io.events.send(event);
                    if ended {
                        return DriverEnd { cli_session, stop: DriverStop::EndTurn };
                    }
                }
            }
            control = io.control.recv(), if control_open => match control {
                Some(DriverControl::Deliver { text, now, done }) => {
                    controls.lock().unwrap().push(format!("deliver now={now}"));
                    let line = send(json!({ "message": text }).to_string());
                    let _ = stdin.write_all(line.as_bytes()).await;
                    let _ = done.send(if now { Delivery::Steer } else { Delivery::NextTurn });
                }
                Some(DriverControl::StopAfterTool) => {
                    controls.lock().unwrap().push("stop_after_tool".into());
                    return DriverEnd { cli_session, stop: DriverStop::Cancelled };
                }
                Some(DriverControl::Cancel) => {
                    controls.lock().unwrap().push("cancel".into());
                    return DriverEnd { cli_session, stop: DriverStop::Cancelled };
                }
                None => control_open = false,
            },
        }
    }
}

pub(crate) fn healthy(agent_id: &AgentId) -> AgentHealth {
    AgentHealth {
        agent_id: agent_id.clone(),
        state: HealthState::Healthy,
        since: OffsetDateTime::UNIX_EPOCH,
        reason: None,
        next_test_at: None,
        disabled: None,
    }
}

/// Unique across the test process, since the leftover sweep kills by session key.
pub(crate) fn unique_key() -> SessionKey {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    SessionKey::parse(&format!("s-t{}-{n}", std::process::id())).unwrap()
}

pub(crate) fn spec_in(workdir: &Path, session_dir: &Path, prompt: &str) -> SessionSpec {
    SessionSpec {
        run: None,
        task: None,
        key: unique_key(),
        role: RoleName::Main,
        step: None,
        model: ModelRef::new("fake", None),
        effort: None,
        access: Access::ReadOnly,
        workdir: workdir.to_path_buf(),
        session_dir: session_dir.to_path_buf(),
        prompt: vec![part(InputPartKind::Prompt, "prompt", prompt, None)],
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

pub(crate) fn queued(n: u32, text: &str, delivery: Delivery) -> QueuedMessage {
    QueuedMessage {
        id: MessageId::numbered(n),
        from: Party::User,
        to: MentionTarget::Number(1),
        text: text.to_owned(),
        queued_at: OffsetDateTime::UNIX_EPOCH,
        delivery,
    }
}

/// A runner for `adapter` with no sandbox (`sandbox.mode = "off"`) under its own temp home.
pub(crate) struct TestDriver {
    pub(crate) worker: CliRunner,
    pub(crate) home: tempfile::TempDir,
    pub(crate) health: Arc<MemoryHealthBoard>,
    pub(crate) plan_usage: Arc<MemoryPlanUsageBoard>,
}

impl TestDriver {
    pub(crate) fn new(adapter: TestAdapter) -> Self {
        Self::with_settings(adapter, Settings::default())
    }

    pub(crate) fn with_settings(adapter: TestAdapter, mut settings: Settings) -> Self {
        settings.sandbox.mode = SandboxChoice::Tool;
        let home = tempfile::tempdir().unwrap();
        let health = Arc::new(MemoryHealthBoard::default());
        let plan_usage = Arc::new(MemoryPlanUsageBoard::default());
        let worker = CliRunner::new(
            Arc::new(adapter),
            ModelRef::new("fake", None),
            "fake".into(),
            Arc::new(settings),
            plan_usage.clone(),
            health.clone(),
        )
        .with_sandbox(Arc::new(Unsandboxed))
        .with_home(Paths::new(home.path()));
        Self {
            worker,
            home,
            health,
            plan_usage,
        }
    }

    pub(crate) fn agents_dir(&self) -> std::path::PathBuf {
        self.home
            .path()
            .join(argonauts_core::layout::user_files::AGENTS)
    }
}

/// Reads every event, then the end, giving up after 30 seconds.
pub(crate) async fn collect(handle: SessionHandle) -> (Vec<RunnerEvent>, SessionEnd) {
    let SessionHandle {
        mut events,
        ended,
        control,
        ..
    } = handle;
    tokio::time::timeout(Duration::from_secs(30), async move {
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            seen.push(event);
        }
        let end = ended.await.unwrap();
        drop(control);
        (seen, end)
    })
    .await
    .expect("the session didn't end within 30 seconds")
}

pub(crate) fn session_events(events: &[RunnerEvent]) -> Vec<&SessionEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            RunnerEvent::Session(event) => Some(event),
            _ => None,
        })
        .collect()
}

pub(crate) fn agent_events(events: &[RunnerEvent]) -> Vec<&AgentEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            RunnerEvent::Agent(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// Each event's name, such as `input`, `agent:result`, or `limit:limit`.
pub(crate) fn names(events: &[RunnerEvent]) -> Vec<String> {
    events.iter().map(name).collect()
}

fn name(event: &RunnerEvent) -> String {
    match event {
        RunnerEvent::Agent(event) => format!("agent:{}", event.type_name()),
        RunnerEvent::AgentStatus(_) => "agent_status".to_owned(),
        RunnerEvent::Session(event) => {
            let json = serde_json::to_value(event).unwrap();
            let kind = json["event"].as_str().unwrap_or("?").to_owned();
            match json.get("kind").and_then(|kind| kind.as_str()) {
                Some(limit) => format!("{kind}:{limit}"),
                None => kind,
            }
        }
    }
}

pub(crate) fn event_line(event: &AgentEvent) -> String {
    serde_json::to_string(event).unwrap()
}
