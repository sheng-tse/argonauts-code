//! Test helpers: a shell-script adapter that prints agent events as JSON lines, a fake session
//! driver, specs, and a worker with no sandbox under a temp home.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::adapter::Transport;
use argonauts_core::driver::{
    DriverControl, DriverEnd, DriverIo, DriverStart, DriverStop, RawFrame, SessionDriver,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::session::{
    Delivery, InputPartKind, QueuedMessage, SessionEnd, SessionEvent,
};
use argonauts_core::future::BoxFuture;
use argonauts_core::input::MentionTarget;
use argonauts_core::limits::{HealthState, VendorHealth};
use argonauts_core::names::{MessageId, RoleName, SessionKey, VendorId, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::settings::sessions::{Compaction, NativeFanout, SessionLifetime};
use argonauts_core::worker::{
    Access, SandboxProfile, SessionHandle, SessionLimits, SessionSpec, WorkerEvent,
};
use argonauts_core::{
    Adapter, AgentEvent, Capabilities, CommandSpec, Paths, SessionRequest, SessionRole, StopPlan,
    StopSignal, StopStep, StreamParser, extract_version,
};
use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};
use serde_json::json;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::HarnessWorker;
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
    parser: Option<fn() -> Box<dyn StreamParser>>,
    resume: bool,
    takeover: bool,
    build: bool,
    driver: Option<Arc<dyn SessionDriver>>,
    stop_plan: StopPlan,
    removed_env: Vec<String>,
    card_home: bool,
}

impl TestAdapter {
    pub(crate) fn new(script: &str) -> Self {
        Self {
            id: "fake",
            binary: "sh",
            script: script.to_owned(),
            input_lines: false,
            input_joins_turn: false,
            parser: None,
            resume: true,
            takeover: false,
            build: true,
            driver: None,
            stop_plan: fast_stop_plan(),
            removed_env: Vec::new(),
            card_home: false,
        }
    }

    /// Points `FAKE_HOME` at the folder the worker passes for a home kept for the card, as an
    /// ACP adapter with `keep = "card"` does.
    pub(crate) fn keeping_its_home_for_the_card(mut self) -> Self {
        self.card_home = true;
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

    /// Reads the output with another adapter's parser, for replaying its recordings.
    pub(crate) fn with_parser(mut self, parser: fn() -> Box<dyn StreamParser>) -> Self {
        self.parser = Some(parser);
        self
    }

    pub(crate) fn without_resume(mut self) -> Self {
        self.resume = false;
        self
    }

    pub(crate) fn with_takeover(mut self) -> Self {
        self.takeover = true;
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
        };
        spec.env_set.push(("FAKE_ROLE".to_owned(), role.to_owned()));
        if let Some(home) = req.project.as_ref().filter(|_| self.card_home) {
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
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            reports_cost: true,
            input_joins_running_turn: self.input_joins_turn,
            ..Capabilities::default()
        }
    }
    fn input_line(&self, message: &str) -> Option<String> {
        self.input_lines.then(|| input_line(message))
    }
    fn interactive_resume(&self, session_id: &str, workdir: &Path) -> Option<CommandSpec> {
        self.takeover
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
    let mut vendor_session = None;
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
            vendor_session,
            stop: DriverStop::Disconnected,
        };
    }
    let mut lines = BufReader::new(io.stdout).lines();
    let mut control_open = true;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else {
                    return DriverEnd { vendor_session, stop: DriverStop::Disconnected };
                };
                let _ = io.frames.send(RawFrame { outgoing: false, line: line.clone() });
                for event in parse_events(&line) {
                    if let AgentEvent::Init { session_id: Some(id), .. } = &event {
                        vendor_session = Some(id.clone());
                    }
                    let ended = matches!(event, AgentEvent::Result { .. });
                    let _ = io.events.send(event);
                    if ended {
                        return DriverEnd { vendor_session, stop: DriverStop::EndTurn };
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
                    return DriverEnd { vendor_session, stop: DriverStop::Cancelled };
                }
                Some(DriverControl::Cancel) => {
                    controls.lock().unwrap().push("cancel".into());
                    return DriverEnd { vendor_session, stop: DriverStop::Cancelled };
                }
                None => control_open = false,
            },
        }
    }
}

pub(crate) fn healthy(vendor: &VendorId) -> VendorHealth {
    VendorHealth {
        vendor: vendor.clone(),
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
        card: None,
        key: unique_key(),
        role: RoleName::Default,
        step: None,
        worker: WorkerRef::new("fake", None),
        effort: None,
        access: Access::ReadOnly,
        workdir: workdir.to_path_buf(),
        session_dir: session_dir.to_path_buf(),
        prompt: vec![part(InputPartKind::Prompt, "prompt", prompt, None)],
        json_schema: None,
        resume: None,
        lifetime: SessionLifetime::Card,
        keep_input_open: false,
        limits: SessionLimits::default(),
        goal_sha256: None,
        sandbox: SandboxProfile::default(),
        compaction: Compaction::Inherit,
        native_fanout: NativeFanout::default(),
        isolated: false,
        status_file: None,
        env: Vec::new(),
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

/// A worker for `adapter` with no sandbox (`sandbox.mode = "off"`) under its own temp home.
pub(crate) struct Harness {
    pub(crate) worker: HarnessWorker,
    pub(crate) home: tempfile::TempDir,
    pub(crate) health: Arc<MemoryHealthBoard>,
    pub(crate) quota: Arc<MemoryQuotaBoard>,
}

impl Harness {
    pub(crate) fn new(adapter: TestAdapter) -> Self {
        Self::with_settings(adapter, Settings::default())
    }

    pub(crate) fn with_settings(adapter: TestAdapter, mut settings: Settings) -> Self {
        settings.sandbox.mode = SandboxChoice::Tool;
        let home = tempfile::tempdir().unwrap();
        let health = Arc::new(MemoryHealthBoard::default());
        let quota = Arc::new(MemoryQuotaBoard::default());
        let worker = HarnessWorker::new(
            Arc::new(adapter),
            WorkerRef::new("fake", None),
            "fake".into(),
            Arc::new(settings),
            quota.clone(),
            health.clone(),
        )
        .with_sandbox(Arc::new(Unsandboxed))
        .with_home(Paths::new(home.path()));
        Self {
            worker,
            home,
            health,
            quota,
        }
    }

    pub(crate) fn agents_dir(&self) -> std::path::PathBuf {
        self.home
            .path()
            .join(argonauts_core::layout::user_files::AGENTS)
    }
}

/// Reads every event, then the end, giving up after 30 seconds.
pub(crate) async fn collect(handle: SessionHandle) -> (Vec<WorkerEvent>, SessionEnd) {
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

pub(crate) fn session_events(events: &[WorkerEvent]) -> Vec<&SessionEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            WorkerEvent::Session(event) => Some(event),
            _ => None,
        })
        .collect()
}

pub(crate) fn agent_events(events: &[WorkerEvent]) -> Vec<&AgentEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            WorkerEvent::Agent(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// Each event's name, such as `input`, `agent:result`, or `limit:budget`.
pub(crate) fn names(events: &[WorkerEvent]) -> Vec<String> {
    events.iter().map(name).collect()
}

fn name(event: &WorkerEvent) -> String {
    match event {
        WorkerEvent::Agent(event) => format!("agent:{}", event.type_name()),
        WorkerEvent::Vendor(_) => "vendor".to_owned(),
        WorkerEvent::Session(event) => {
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
