//! Whole sessions through `CliRunner::start`, with shell-script agents and a fake driver.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_core::agents::session_runner::{
    Access, RunnerError, RunnerEvent, SandboxMode, SessionControl, SessionHandle, SessionRunner,
    SessionSpec,
};
use argonauts_core::base::layout::session_files;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::models::registry::{
    CostModel, CostUnit, ModelAccess, ModelEntry, RegistryKind,
};
use argonauts_core::runs::events::context::ContextDetail;
use argonauts_core::runs::events::session::{
    CallCount, Delivery, EndReason, InputPartKind, LimitEvent, PromptVia, SessionEvent,
    SessionInput, Watchdog, WrapUpVia,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{IsolatedLeak, SandboxChoice};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::{
    Adapter, AgentEvent, ErrorCategory, LimitDimension, MessageStage, WRAP_UP_MESSAGE,
};
use argonauts_testkit::engine::services::StaticRegistry;
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;
use tokio::sync::mpsc;

use super::launch::{PROMPT_PLACEHOLDER, part, prompt_text};
use super::request::SESSION_ENV;
use super::testing::{
    FakeDriver, TestAdapter, TestDriver, agent_events, collect, event_line, names, queued,
    session_events, spec_in,
};
use crate::process::watchdogs::REPEAT_NOTE;
use crate::sandbox::Unsandboxed;
use crate::testing::{TestRepo, wait_until_gone};

/// A workdir and a session folder in one temp dir.
struct Dirs {
    _tmp: tempfile::TempDir,
    work: PathBuf,
    session: PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("work");
        let session = root.join("session");
        std::fs::create_dir_all(&work).unwrap();
        Self {
            _tmp: tmp,
            work,
            session,
        }
    }

    fn in_repo(repo: &TestRepo) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().canonicalize().unwrap().join("session");
        Self {
            _tmp: tmp,
            work: repo.root.clone(),
            session,
        }
    }

    fn spec(&self, prompt: &str) -> SessionSpec {
        spec_in(&self.work, &self.session, prompt)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.work.join(name)).unwrap_or_default()
    }
}

async fn wait_for_file(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Reads events until one matches, and returns every event read.
async fn read_until(
    events: &mut mpsc::Receiver<RunnerEvent>,
    matches: impl Fn(&RunnerEvent) -> bool,
) -> Vec<RunnerEvent> {
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = events.recv().await {
            let done = matches(&event);
            seen.push(event);
            if done {
                return;
            }
        }
        panic!("the events ended first: {:?}", names(&seen));
    })
    .await
    .expect("the event never came");
    seen
}

/// Reads the rest of a session whose events were partly read already.
async fn finish(
    mut seen: Vec<RunnerEvent>,
    handle: SessionHandle,
) -> (
    Vec<RunnerEvent>,
    argonauts_core::runs::events::session::SessionEnd,
) {
    let (rest, end) = collect(handle).await;
    seen.extend(rest);
    (seen, end)
}

fn is_agent_message(event: &RunnerEvent, text: &str) -> bool {
    matches!(event, RunnerEvent::Agent(AgentEvent::Message { text: said, .. }) if said == text)
}

fn inputs(events: &[RunnerEvent]) -> Vec<&SessionInput> {
    session_events(events)
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::Input(input) => Some(input.as_ref()),
            _ => None,
        })
        .collect()
}

/// Kills a process the test started even when an assertion fails first.
struct KillOnDrop(i32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = kill(Pid::from_raw(self.0), nix::sys::signal::Signal::SIGKILL);
    }
}

fn alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None) != Err(Errno::ESRCH)
}

fn message(text: &str) -> String {
    event_line(&AgentEvent::Message {
        text: text.into(),
        parent: None,
    })
}

fn result(text: &str) -> String {
    event_line(&AgentEvent::Result {
        text: Some(text.into()),
        structured: None,
        is_error: false,
    })
}

fn tool(id: &str, started: bool) -> String {
    event_line(&if started {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: "Bash".into(),
            input: serde_json::json!({ "command": "ls" }),
            parent: None,
        }
    } else {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: None,
            parent: None,
        }
    })
}

fn init(session: &str) -> String {
    event_line(&AgentEvent::Init {
        session_id: Some(session.into()),
        model: None,
        tools: Vec::new(),
        plugins: Vec::new(),
        capabilities: Vec::new(),
        version: None,
    })
}

/// `echo '<line>'` for a script, with single quotes escaped.
fn echo(line: &str) -> String {
    format!("echo '{}'", line.replace('\'', r"'\''"))
}

fn wait_for_go() -> &'static str {
    "while [ ! -f go ]; do sleep 0.02; done"
}

#[tokio::test]
async fn dropped_session_handle_kills_process_group() {
    let dirs = Dirs::new();
    // The subshell writes its own child's pid in the background, so `ready` waits for it.
    let script = "sleep 300 & echo $! >> pids; \
                  (sleep 300 & echo $! >> pids; : > inner; wait) & echo $! >> pids; \
                  echo $$ >> pids; while [ ! -f inner ]; do sleep 0.02; done; : > ready; wait";
    let h = TestDriver::new(TestAdapter::new(script));
    let handle = h.worker.start(dirs.spec("hi")).await.unwrap();
    wait_for_file(&dirs.work.join("ready")).await;
    let pids: Vec<i32> = dirs
        .read("pids")
        .lines()
        .map(|pid| pid.trim().parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 4);
    // The agent runs only once its group is on record.
    assert!(dirs.session.join(session_files::PROCESS).exists());
    let listed = || std::fs::read_dir(h.agents_dir()).map_or(0, |dir| dir.count());
    assert_eq!(listed(), 1, "the group is listed while it runs");

    drop(handle);
    for pid in pids {
        wait_until_gone(pid).await;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(h.agents_dir()).unwrap().count() > 0 {
        assert!(std::time::Instant::now() < deadline, "the listing stayed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn an_agent_runs_only_once_its_group_is_on_record() {
    let dirs = Dirs::new();
    // At once and with the shell's own commands, as fast as a command it starts to check its
    // own ancestry could look.
    let script = "read -r agents < agents; read -r session < session; \
                  if [ -f \"$agents/$$.json\" ] && [ -f \"$session/process.json\" ]; \
                  then echo on-record > seen; else echo unrecorded > seen; fi";
    let h = TestDriver::new(TestAdapter::new(script));
    let path = |path: PathBuf| path.display().to_string();
    std::fs::write(dirs.work.join("agents"), path(h.agents_dir())).unwrap();
    std::fs::write(dirs.work.join("session"), path(dirs.session.clone())).unwrap();
    let handle = h.worker.start(dirs.spec("hi")).await.unwrap();
    let (_, end) = collect(handle).await;
    assert_eq!(dirs.read("seen").trim(), "on-record", "{end:?}");
}

#[tokio::test]
async fn session_input_parts_equal_prompt() {
    let dirs = Dirs::new();
    let script = format!(
        "printf '%s' \"$1\" > got-prompt; printf '%s' \"${SESSION_ENV}\" > got-session; {}",
        echo(&result("ok"))
    );
    let h = TestDriver::new(TestAdapter::new(&script));
    let mut spec = dirs.spec("");
    spec.prompt = vec![
        part(InputPartKind::Prompt, "role", "You fix bugs.\n", None),
        part(
            InputPartKind::Task,
            "task",
            "Task: café ✓ \"quoted\"\n",
            None,
        ),
        part(
            InputPartKind::Goal,
            "goal",
            "Goal: 🦀",
            Some("goal.md".into()),
        ),
    ];
    spec.env = vec![(SESSION_ENV.into(), "forged".into())];
    let key = spec.key.clone();
    let prompt = prompt_text(&spec.prompt);
    let (events, end) = collect(h.worker.start(spec.clone()).await.unwrap()).await;

    let RunnerEvent::Session(SessionEvent::Input(first)) = &events[0] else {
        panic!("the first event must be the input: {:?}", names(&events));
    };
    assert_eq!(first.parts, spec.prompt);
    assert_eq!(prompt_text(&first.parts), prompt);
    assert_eq!(dirs.read("got-prompt"), prompt, "sent byte for byte");
    assert_eq!(dirs.read("got-session"), key.to_string());
    assert!(first.launch.args.contains(&PROMPT_PLACEHOLDER.to_owned()));
    assert!(!first.launch.args.iter().any(|arg| arg.contains("café")));
    assert!(first.launch.env_set.contains(&SESSION_ENV.to_owned()));
    assert_eq!(first.launch.prompt_via, PromptVia::Argument);
    assert_eq!(first.launch.sandbox.as_deref(), Some("no sandbox"));
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.result_text.as_deref(), Some("ok"));
    assert_eq!(end.exit_code, Some(0));
}

#[tokio::test]
async fn raw_log_keeps_every_line() {
    let dirs = Dirs::new();
    let script = format!(
        "{}; echo 'not json at all'; echo 'on stderr' >&2; echo ''; {}; echo 'last words' >&2",
        echo(&message("hello")),
        echo(&result("done"))
    );
    let h = TestDriver::new(TestAdapter::new(&script));
    let (events, end) = collect(h.worker.start(dirs.spec("hi")).await.unwrap()).await;

    let raw_path = end.raw_log.clone().unwrap();
    assert_eq!(raw_path, dirs.session.join(session_files::RAW_STDOUT));
    let stdout = std::fs::read_to_string(&raw_path).unwrap();
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            message("hello").as_str(),
            "not json at all",
            "",
            result("done").as_str()
        ]
    );
    let stderr = std::fs::read_to_string(dirs.session.join(session_files::RAW_STDERR)).unwrap();
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        ["on stderr", "last words"]
    );
    let unknown = agent_events(&events)
        .into_iter()
        .filter(|event| matches!(event, AgentEvent::Unknown { .. }))
        .count();
    assert_eq!(
        unknown, 2,
        "the garbage and the empty line stay as unknown events"
    );
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.last_message.as_deref(), Some("hello"));
}

/// Nulls the strict format allowed in place of a field the format left optional are gone
/// from the answer the session ends with.
#[tokio::test]
async fn the_answer_reads_as_its_format_was_written() {
    let dirs = Dirs::new();
    let answer = event_line(&AgentEvent::Result {
        text: Some("ok".into()),
        structured: Some(serde_json::json!({"accept": true, "summary": null})),
        is_error: false,
    });
    let h = TestDriver::new(TestAdapter::new(&echo(&answer)));
    let mut spec = dirs.spec("review it");
    spec.json_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {"accept": {"type": "boolean"}, "summary": {"type": "string"}},
        "required": ["accept"],
    }));
    let (_, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(end.structured, Some(serde_json::json!({"accept": true})));
}

#[tokio::test]
async fn read_only_or_no_tools_until_approved() {
    let dirs = Dirs::new();
    let script = format!(
        "printf '%s' \"$FAKE_ROLE\" > got-role; {}",
        echo(&result("ok"))
    );
    let h = TestDriver::new(TestAdapter::new(&script));
    let role = |access: Access, goal: Option<String>| {
        let mut spec = dirs.spec("look around");
        spec.access = access;
        spec.goal_sha256 = goal;
        let worker = &h.worker;
        let dirs = &dirs;
        async move {
            let _ = std::fs::remove_file(dirs.work.join("got-role"));
            let (_, end) = collect(worker.start(spec).await.unwrap()).await;
            assert_eq!(end.reason, EndReason::Completed);
            dirs.read("got-role")
        }
    };
    assert_eq!(role(Access::NoTools, None).await, "no-tools");
    assert_eq!(role(Access::ReadOnly, None).await, "read-only");
    // Only an approved goal gets the build flags.
    let mut build = dirs.spec("build it");
    build.access = Access::Build;
    assert!(matches!(
        h.worker.start(build).await,
        Err(RunnerError::Refused(_))
    ));
    assert_eq!(role(Access::Build, Some("ab".repeat(32))).await, "build");
    assert_eq!(
        role(Access::Experiment, Some("ab".repeat(32))).await,
        "build"
    );
}

#[tokio::test]
async fn a_launch_the_adapter_rejects_never_starts() {
    let dirs = Dirs::new();
    let h = TestDriver::new(TestAdapter::new("true").read_only());
    let mut spec = dirs.spec("build it");
    spec.access = Access::Build;
    spec.goal_sha256 = Some("ab".repeat(32));
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert!(events.is_empty(), "{:?}", names(&events));
    match end.reason {
        EndReason::StartFailed { message } => assert!(message.contains("only reads")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn build_without_goal_hash_is_refused() {
    let dirs = Dirs::new();
    let h = TestDriver::new(TestAdapter::new("true"));
    for access in [Access::Build, Access::Experiment] {
        let mut spec = dirs.spec("build it");
        spec.access = access;
        match h.worker.start(spec).await {
            Err(RunnerError::Refused(why)) => assert!(why.contains("goal"), "{why}"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("a {access:?} session started without a goal hash"),
        }
    }
    assert!(
        !dirs.session.exists(),
        "nothing was prepared for a refused session"
    );
}

#[tokio::test]
async fn driver_without_sandbox_is_refused() {
    let dirs = Dirs::new();
    // The settings ask for the OS sandbox, but this runner can't wrap anything.
    let mut settings = Settings::default();
    settings.sandbox.mode = SandboxChoice::Os;
    let worker = super::CliRunner::new(
        Arc::new(TestAdapter::new("true")),
        ModelRef::new("fake", None),
        "fake".into(),
        Arc::new(settings),
        Arc::new(argonauts_testkit::engine::services::MemoryPlanUsageBoard::default()),
        Arc::new(argonauts_testkit::engine::services::MemoryHealthBoard::default()),
    )
    .with_sandbox(Arc::new(Unsandboxed));
    match worker.start(dirs.spec("hi")).await {
        Err(RunnerError::Refused(why)) => assert!(why.contains("sandbox"), "{why}"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("started without a sandbox"),
    }
    // An engine asking for no sandbox is refused too, unless you turned it off.
    let h = TestDriver::new(TestAdapter::new("true"));
    let mut spec = dirs.spec("hi");
    spec.sandbox.mode = SandboxMode::None;
    assert!(h.worker.start(spec).await.is_ok());
}

/// Each role's own `sandbox.mode` decides whether its sessions need ours.
#[tokio::test]
async fn a_roles_own_mode_decides_whether_it_needs_our_sandbox() {
    let dirs = Dirs::new();
    let mut settings = Settings::default();
    settings.sandbox.mode = SandboxChoice::Os;
    settings
        .roles
        .entry(RoleName::Worker)
        .or_default()
        .sandbox
        .mode = Some(SandboxChoice::Tool);
    let model = super::CliRunner::new(
        Arc::new(TestAdapter::new("true")),
        ModelRef::new("fake", None),
        "fake".into(),
        Arc::new(settings),
        Arc::new(argonauts_testkit::engine::services::MemoryPlanUsageBoard::default()),
        Arc::new(argonauts_testkit::engine::services::MemoryHealthBoard::default()),
    )
    .with_sandbox(Arc::new(Unsandboxed));
    assert!(model.start(dirs.spec("hi")).await.is_err());
    let mut worker = dirs.spec("hi");
    worker.role = RoleName::Worker;
    assert!(model.start(worker).await.is_ok());

    // The other way round: everyone runs on their tool's own sandbox but the worker, whose
    // sessions are refused without ours.
    let mut settings = Settings::default();
    settings
        .roles
        .entry(RoleName::Worker)
        .or_default()
        .sandbox
        .mode = Some(SandboxChoice::Os);
    let h = TestDriver::with_settings(TestAdapter::new("true"), settings);
    let mut spec = dirs.spec("hi");
    spec.sandbox.mode = SandboxMode::None;
    assert!(h.worker.start(spec.clone()).await.is_ok());
    spec.role = RoleName::Worker;
    match h.worker.start(spec).await {
        Err(RunnerError::Refused(why)) => assert!(why.contains("sandbox"), "{why}"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("the session started without a sandbox"),
    }
}

#[tokio::test]
async fn disabled_agent_starts_nothing() {
    let dirs = Dirs::new();
    let h = TestDriver::new(TestAdapter::new("true"));
    use argonauts_core::models::limits::HealthBoard;
    h.health.observe(
        argonauts_core::models::limits::HealthSignal::AdapterBroken {
            agent_id: h.worker.agent_id(),
            detail: "untested version".into(),
        },
    );
    // A fallback hears it before starting anything.
    let why = h.worker.start_refusal().expect("a refusal");
    match h.worker.start(dirs.spec("hi")).await {
        Err(err @ RunnerError::Disabled { .. }) => assert_eq!(err.to_string(), why),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("a disabled agent started"),
    }
}

#[tokio::test]
async fn opencode_with_no_model_says_so_before_it_starts() {
    let opencode = TestAdapter::new("true")
        .with_id("opencode")
        .with_binary("opencode");
    let why = TestDriver::new(opencode)
        .worker
        .start_refusal()
        .expect("a refusal");
    assert!(why.contains("has no model set"), "{why}");
    assert_eq!(
        TestDriver::new(TestAdapter::new("true"))
            .worker
            .start_refusal(),
        None
    );
}

#[tokio::test]
async fn steer_delivery_per_steer_support() {
    // Open input: the message is one more input line, and a turn of its own.
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; printf '%s\\n' \"$first\" >> got-input; {}; {}; {}; \
         while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; {}; done",
        echo(&message("working")),
        wait_for_go(),
        echo(&result("turn 1")),
        echo(&result("turn 2")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_input_lines());
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| is_agent_message(e, "working")).await;
    let steer = queued(1, "use the cache", Delivery::Steer);
    handle
        .control
        .send(SessionControl::Deliver(steer.clone()))
        .await
        .unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Session(SessionEvent::Input(_)))
        })
        .await,
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;

    let delivered = session_events(&events).into_iter().any(|e| {
        *e == SessionEvent::Delivered {
            message: steer.id.clone(),
            via: Delivery::Steer,
        }
    });
    assert!(delivered, "{:?}", names(&events));
    let input = inputs(&events)[1];
    assert_eq!(input.parts.len(), 1);
    assert_eq!(input.parts[0].text, "use the cache");
    assert_eq!(input.launch.prompt_via, PromptVia::StreamInput);
    assert_eq!(inputs(&events)[0].launch.prompt_via, PromptVia::StreamInput);
    let got = dirs.read("got-input");
    assert_eq!(got.lines().count(), 2, "{got}");
    assert!(got.lines().nth(1).unwrap().contains("use the cache"));
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.result_text.as_deref(), Some("turn 2"));

    // No open input: wait for the tool call, interrupt, and resume with the message.
    let dirs = Dirs::new();
    let script = format!(
        "if [ -z \"$2\" ]; then {}; {}; {}; {}; exec sleep 30; \
         else printf '%s|%s' \"$1\" \"$2\" > got-resume; {}; fi",
        echo(&init("cli-1")),
        echo(&tool("t1", true)),
        wait_for_go(),
        echo(&tool("t1", false)),
        echo(&result("resumed")),
    );
    let h = TestDriver::new(TestAdapter::new(&script));
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    let steer = queued(2, "stop using sed", Delivery::Steer);
    handle
        .control
        .send(SessionControl::Deliver(steer.clone()))
        .await
        .unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Session(SessionEvent::Queued(_)))
        })
        .await,
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !dirs.work.join("got-resume").exists(),
        "no interrupt while the tool runs"
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;

    assert_eq!(dirs.read("got-resume"), "stop using sed|cli-1");
    let resumed = inputs(&events)[1];
    assert_eq!(resumed.parts[0].text, "stop using sed");
    assert!(resumed.launch.args.contains(&"cli-1".to_owned()));
    let order = names(&events);
    let delivered_at = order.iter().position(|n| n == "delivered").unwrap();
    let finished_at = order
        .iter()
        .position(|n| n == "agent:tool_finished")
        .unwrap();
    assert!(finished_at < delivered_at, "{order:?}");
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.cli_session.as_deref(), Some("cli-1"));
    assert_eq!(end.result_text.as_deref(), Some("resumed"));
}

/// A session started as a copy of another is the copy from its first event on: the id it is
/// known by, the one a later process resumes, and the one its spend is kept under are the
/// copy's, never the original's.
#[tokio::test]
async fn a_forked_session_carries_on_as_the_copy() {
    let dirs = Dirs::new();
    let script = format!(
        "if [ \"$3\" = fork ]; then {}; {}; {}; {}; exec sleep 30; \
         else printf '%s|%s|%s' \"$1\" \"$2\" \"$3\" > got-resume; {}; fi",
        echo(&init("copy-1")),
        echo(&tool("t1", true)),
        wait_for_go(),
        echo(&tool("t1", false)),
        echo(&result("resumed")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_fork());
    assert!(h.worker.capabilities().fork);
    let mut spec = dirs.spec("start");
    spec.resume = Some("cli-1".into());
    spec.fork = true;
    let mut handle = h.worker.start(spec).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    let steer = queued(2, "stop using sed", Delivery::Steer);
    handle
        .control
        .send(SessionControl::Deliver(steer))
        .await
        .unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Session(SessionEvent::Queued(_)))
        })
        .await,
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;

    let known_as: Vec<&str> = session_events(&events)
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::CliSession { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(known_as, ["copy-1"]);
    // The process that delivers the message resumes the copy, and copies nothing again.
    assert_eq!(dirs.read("got-resume"), "stop using sed|copy-1|");
    assert_eq!(end.cli_session.as_deref(), Some("copy-1"));
    assert_eq!(end.result_text.as_deref(), Some("resumed"));
}

/// A CLI with no fork of its own would resume the original and write to it.
#[tokio::test]
async fn a_fork_is_refused_where_the_cli_has_none() {
    let dirs = Dirs::new();
    let h = TestDriver::new(TestAdapter::new("echo should-not-run > ran"));
    let mut spec = dirs.spec("start");
    spec.resume = Some("cli-1".into());
    spec.fork = true;
    match h.worker.start(spec).await {
        Err(RunnerError::Refused(why)) => {
            assert_eq!(why, "Fake Agent can't carry on a copy of a session");
        }
        other => panic!("{:?}", other.map(|_| "started")),
    }
    assert!(!dirs.work.join("ran").exists());
}

#[tokio::test]
async fn a_steer_that_joins_the_running_turn_brings_no_result_of_its_own() {
    // Like Claude Code: the steer goes to the model inside the running turn, one result ends
    // it, and the agent exits once its input closes.
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; {}; IFS= read -r steer; printf '%s\\n' \"$steer\" >> got-input; \
         {}; while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; done; \
         : > input-closed",
        echo(&message("working")),
        wait_for_go(),
        echo(&result("done")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_input_joining_the_turn());
    let mut spec = dirs.spec("start");
    // Input left open would keep the agent waiting until this watchdog called it stuck.
    spec.limits.inactivity_min = Some(0.1);
    let mut handle = h.worker.start(spec).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| is_agent_message(e, "working")).await;
    let steer = queued(1, "use the cache", Delivery::Steer);
    handle
        .control
        .send(SessionControl::Deliver(steer.clone()))
        .await
        .unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Session(SessionEvent::Input(_)))
        })
        .await,
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;

    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.result_text.as_deref(), Some("done"));
    assert!(dirs.work.join("input-closed").exists());
    let got = dirs.read("got-input");
    assert_eq!(got.lines().count(), 1, "{got}");
    assert!(got.contains("use the cache"));
    let results = agent_events(&events)
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::Result { .. }))
        .count();
    assert_eq!(results, 1);
}

#[tokio::test]
async fn a_message_with_its_uuid_goes_in_at_once_while_a_tool_runs() {
    // Like Claude Code 2.1.289: the CLI holds the message until the model can read it, so it's
    // written at once, and the turn's one result ends the session once the model read it.
    let dirs = Dirs::new();
    let started = event_line(&AgentEvent::MessageState {
        uuid: "u-1".into(),
        state: MessageStage::Started,
    });
    let script = format!(
        "IFS= read -r first; {}; IFS= read -r joined; printf '%s\\n' \"$joined\" >> got-input; \
         {}; {}; {}; while IFS= read -r line; do :; done; : > input-closed",
        echo(&tool("t1", true)),
        echo(&tool("t1", false)),
        echo(&started),
        echo(&result("done")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_input_joining_the_turn());
    let mut spec = dirs.spec("start");
    spec.limits.inactivity_min = Some(0.1);
    let mut handle = h.worker.start(spec).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    handle
        .control
        .send(SessionControl::Send {
            text: "also check the changelog".into(),
            uuid: "u-1".into(),
            pictures: Vec::new(),
        })
        .await
        .unwrap();
    let (events, end) = finish(seen, handle).await;

    assert_eq!(end.reason, EndReason::Completed);
    assert!(dirs.read("got-input").contains("also check the changelog"));
    assert!(dirs.work.join("input-closed").exists());
    let order = names(&events);
    let finished_at = order.iter().position(|n| n == "agent:tool_finished");
    assert!(finished_at.is_some(), "{order:?}");
}

#[tokio::test]
async fn send_now_from_the_agent_view_hands_over_steering_held_for_the_tool_call() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; IFS= read -r steer; printf '%s\\n' \"$steer\" >> got-input; \
         IFS= read -r now; printf '%s\\n' \"$now\" >> got-input; {}; \
         uuid=$(printf '%s' \"$steer\" | sed 's/.*\"uuid\":\"\\([^\"]*\\)\".*/\\1/'); \
         printf '{{\"type\":\"message_state\",\"uuid\":\"%s\",\"state\":\"started\"}}\\n' \"$uuid\"; \
         {}; while IFS= read -r line; do :; done",
        echo(&tool("t1", true)),
        echo(&tool("t1", false)),
        echo(&result("done")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_send_now());
    let mut spec = dirs.spec("start");
    spec.limits.inactivity_min = Some(0.1);
    let mut handle = h.worker.start(spec).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    let steer = queued(1, "use the cache", Delivery::Steer);
    handle
        .control
        .send(SessionControl::Deliver(steer))
        .await
        .unwrap();
    handle
        .control
        .send(SessionControl::SendNow {
            uuid: String::new(),
        })
        .await
        .unwrap();
    // The tool never finishes on its own here: only send now lets the agent read on.
    let (_, end) = finish(seen, handle).await;
    let got = dirs.read("got-input");
    let lines: Vec<serde_json::Value> = got
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["text"], "use the cache", "{got}");
    assert_eq!(lines[1]["send_now"], lines[0]["uuid"], "{got}");
    assert_eq!(end.reason, EndReason::Completed);
}

/// Reads agent events as they are, and stops a subagent, or the turn alone, with a line that
/// says which.
struct StoppingParser;

impl argonauts_core::agents::adapter::StreamParser for StoppingParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        vec![serde_json::from_str(line).unwrap_or_else(|_| AgentEvent::unknown_line(line))]
    }
    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn stop_subagent_line(&self, id: &str) -> Option<String> {
        Some(format!("{}\n", serde_json::json!({ "stop_task": id })))
    }
    fn stop_turn_line(&self) -> Option<String> {
        Some(format!("{}\n", serde_json::json!({ "stop_turn": true })))
    }
}

/// What a CLI says when its turn was stopped: the error, then the turn's result.
fn interrupted() -> String {
    let error = event_line(&AgentEvent::Error {
        category: ErrorCategory::Interrupted,
        message: "the turn was interrupted".into(),
        retryable: false,
    });
    let result = event_line(&AgentEvent::Result {
        text: None,
        structured: None,
        is_error: true,
    });
    format!("{}; {}", echo(&error), echo(&result))
}

/// Esc stops the turn alone: the CLI's own stop line goes in, nothing is
/// signalled, and the session stays open for what runs in the background, whose end starts
/// the CLI's next turn as always.
#[tokio::test]
async fn stopping_the_turn_leaves_the_background_running_and_the_session_open() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; {}; IFS= read -r stop; printf '%s\\n' \"$stop\" >> got-input; \
         {}; {}; {}; {}; {}; while IFS= read -r line; do :; done; : > input-closed",
        echo(&background(&["b1"], false)),
        echo(&tool("t1", true)),
        echo(&tool("t1", false)),
        interrupted(),
        wait_for_go(),
        echo(&background(&[], true)),
        echo(&result("the agent finished")),
    );
    let adapter = TestAdapter::new(&script)
        .with_input_joining_the_turn()
        .with_parser(|| Box::new(StoppingParser));
    let h = TestDriver::new(adapter);
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    handle.control.send(SessionControl::StopTurn).await.unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(
                e,
                RunnerEvent::Agent(AgentEvent::Result { is_error: true, .. })
            )
        })
        .await,
    );
    let got: serde_json::Value = serde_json::from_str(dirs.read("got-input").trim()).unwrap();
    assert_eq!(got["stop_turn"], true);
    // The stopped turn has its result, and the input stays open for the task still running.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!dirs.work.join("input-closed").exists());
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (_, end) = finish(seen, handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.result_text.as_deref(), Some("the agent finished"));
    assert!(dirs.work.join("input-closed").exists());
}

/// A session whose last word is the turn you stopped was stopped by you: it didn't fail. And
/// a CLI with no way to stop a turn alone is stopped whole.
#[tokio::test]
async fn a_stopped_turn_with_nothing_after_it_ends_as_stopped() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; IFS= read -r stop; {}; {}; \
         while IFS= read -r line; do :; done",
        echo(&tool("t1", true)),
        echo(&tool("t1", false)),
        interrupted(),
    );
    let adapter = TestAdapter::new(&script)
        .with_input_joining_the_turn()
        .with_parser(|| Box::new(StoppingParser));
    let h = TestDriver::new(adapter);
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    handle.control.send(SessionControl::StopTurn).await.unwrap();
    let (_, end) = finish(seen, handle).await;
    assert_eq!(end.reason, EndReason::Stopped);

    let dirs = Dirs::new();
    let script = format!("IFS= read -r first; {}; sleep 30", echo(&tool("t1", true)));
    let h = TestDriver::new(TestAdapter::new(&script).with_input_joining_the_turn());
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    handle.control.send(SessionControl::StopTurn).await.unwrap();
    let (_, end) = finish(seen, handle).await;
    assert_eq!(end.reason, EndReason::Stopped);
}

/// Stopping one subagent writes the CLI's own stop line and leaves the session running. A CLI
/// with no such line says so, and nothing else stops.
#[tokio::test]
async fn stopping_one_subagent_writes_the_clis_stop_line_and_the_session_goes_on() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; IFS= read -r stop; printf '%s\\n' \"$stop\" >> got-input; \
         {}; {}; while IFS= read -r line; do :; done",
        echo(&tool("t1", true)),
        echo(&tool("t1", false)),
        echo(&result("done")),
    );
    let stop = || SessionControl::StopSubagent {
        id: "toolu_a".into(),
    };
    let adapter = TestAdapter::new(&script)
        .with_input_joining_the_turn()
        .with_parser(|| Box::new(StoppingParser));
    let h = TestDriver::new(adapter);
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    handle.control.send(stop()).await.unwrap();
    let (events, end) = finish(seen, handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    let got: serde_json::Value = serde_json::from_str(dirs.read("got-input").trim()).unwrap();
    assert_eq!(got["stop_task"], "toolu_a");
    assert!(notices(&events).is_empty(), "{:?}", notices(&events));

    // The same session with a CLI that has no stop of its own.
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; sleep 0.3; {}; {}",
        echo(&tool("t1", true)),
        echo(&tool("t1", false)),
        echo(&result("done")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_input_joining_the_turn());
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    handle.control.send(stop()).await.unwrap();
    let (events, end) = finish(seen, handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert!(
        notices(&events)
            .iter()
            .any(|notice| notice.contains("can't stop that subagent on its own")),
        "{:?}",
        notices(&events)
    );
}

#[tokio::test]
async fn a_message_the_input_cant_take_says_it_never_went() {
    let dirs = Dirs::new();
    let script = format!("IFS= read -r first; {}; sleep 1", echo(&result("done")));
    let h = TestDriver::new(TestAdapter::new(&script));
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::Result { .. }))
    })
    .await;
    // Without input lines, nothing can go in.
    handle
        .control
        .send(SessionControl::Send {
            text: "too late".into(),
            uuid: "u-2".into(),
            pictures: Vec::new(),
        })
        .await
        .unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Agent(AgentEvent::MessageState { .. }))
        })
        .await,
    );
    let (events, _) = finish(seen, handle).await;
    assert!(agent_events(&events).iter().any(|e| matches!(
        e,
        AgentEvent::MessageState {
            state: MessageStage::NotSent,
            ..
        }
    )));
}

/// A recorded Claude Code stream, quoted for a script.
fn recorded(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../adapters/claude-code/fixtures")
        .join(format!("{name}.stdout.jsonl"))
        .canonicalize()
        .unwrap();
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Reads the prompt, then marks `input-closed` once the input closes, while the script goes on.
const WATCH_INPUT: &str = "IFS= read -r first; exec 3<&0; \
    (while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; done; \
    : > input-closed) <&3 &";

#[tokio::test]
async fn a_turn_that_left_a_command_in_the_background_keeps_the_input_open_for_it() {
    // Checked live with the input left open: send now moved ./slow-test.sh to the background,
    // the turn ended on PONG, and when the command ended Claude Code told the model in a turn
    // of its own. Closing the input at PONG would have killed the command.
    let stream = recorded("send_now_resume_open");
    let script = format!(
        "{WATCH_INPUT} sed -n '1,49p' {stream}; {}; sed -n '50,$p' {stream}; wait",
        wait_for_go(),
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let dirs = Dirs::new();
    let mut spec = dirs.spec("Run ./slow-test.sh");
    spec.limits.inactivity_min = Some(0.1);
    let mut handle = h.worker.start(spec).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::Result { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !dirs.work.join("input-closed").exists(),
        "the input closed with the command still in the background"
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;

    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(
        end.result_text.as_deref(),
        Some("The script printed that 12 tests passed.")
    );
    assert!(dirs.work.join("input-closed").exists());
    let results = agent_events(&events)
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::Result { .. }))
        .count();
    assert_eq!(results, 2);
}

#[tokio::test]
async fn a_resume_waits_past_the_empty_result_the_last_process_left() {
    // The next resume closed out the command the last process killed, with an empty result
    // before your message even started. Counting it would close the input then.
    let stream = recorded("send_now_resume_stale_task");
    let script = format!(
        "{WATCH_INPUT} sed -n '1,6p' {stream}; {}; sed -n '7,$p' {stream}; wait",
        wait_for_go(),
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let dirs = Dirs::new();
    let mut spec = dirs.spec("Run ./slow-test.sh");
    spec.limits.inactivity_min = Some(0.1);
    let mut handle = h.worker.start(spec).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(
            e,
            RunnerEvent::Agent(AgentEvent::MessageState {
                state: MessageStage::Started,
                ..
            })
        )
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !dirs.work.join("input-closed").exists(),
        "the left-over result closed the input"
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;

    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(
        end.result_text.as_deref(),
        Some("The script printed that 12 tests passed.")
    );
    assert!(dirs.work.join("input-closed").exists());
    let results: Vec<_> = agent_events(&events)
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::Result { .. }))
        .collect();
    assert_eq!(results.len(), 1, "{results:?}");
}

fn background(running: &[&str], ended: bool) -> String {
    event_line(&AgentEvent::BackgroundTasks {
        running: running.iter().map(|task| (*task).to_owned()).collect(),
        ended,
    })
}

#[tokio::test]
async fn a_message_sent_while_the_background_runs_goes_in() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; {}; IFS= read -r joined; \
         printf '%s\\n' \"$joined\" >> got-input; {}; {}; {}; {}; \
         while IFS= read -r line; do :; done; : > input-closed",
        echo(&background(&["b1"], false)),
        echo(&result("PONG")),
        echo(&result("noted")),
        wait_for_go(),
        echo(&background(&[], true)),
        echo(&result("12 passed")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_send_now());
    let mut spec = dirs.spec("start");
    spec.limits.inactivity_min = Some(0.1);
    let mut handle = h.worker.start(spec).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::Result { .. }))
    })
    .await;
    handle
        .control
        .send(SessionControl::Send {
            text: "and the docs?".into(),
            uuid: "u-1".into(),
            pictures: Vec::new(),
        })
        .await
        .unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Agent(AgentEvent::Result { text: Some(text), .. }) if text == "noted")
        })
        .await,
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;

    assert!(dirs.read("got-input").contains("and the docs?"));
    assert!(!agent_events(&events).iter().any(|e| matches!(
        e,
        AgentEvent::MessageState {
            state: MessageStage::NotSent,
            ..
        }
    )));
    // The turn the CLI started when the task ended was waited for too.
    assert_eq!(end.result_text.as_deref(), Some("12 passed"));
    assert!(dirs.work.join("input-closed").exists());
}

#[tokio::test]
async fn the_wait_on_the_background_ends_with_the_sessions_wall_clock() {
    // Closing the input is how Claude Code learns to stop, and its task's end follows.
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; {}; while IFS= read -r line; do :; done; \
         : > input-closed; {}",
        echo(&background(&["b1"], false)),
        echo(&result("PONG")),
        echo(&background(&[], true)),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_send_now());
    let mut spec = dirs.spec("start");
    spec.limits.inactivity_min = Some(0.1);
    spec.limits.max_minutes = Some(0.02);
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;

    assert!(dirs.work.join("input-closed").exists());
    assert_eq!(end.result_text.as_deref(), Some("PONG"));
    assert!(
        agent_events(&events).iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. } if message.contains("when the wait for it ran out")
        )),
        "{:?}",
        names(&events)
    );
}

#[tokio::test]
async fn a_steer_on_open_input_waits_for_the_running_tool_call() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; {}; {}; IFS= read -r steer; \
         printf '%s\\n' \"$steer\" >> got-input; {}; while IFS= read -r line; do :; done",
        echo(&tool("t1", true)),
        wait_for_go(),
        echo(&tool("t1", false)),
        echo(&result("done")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_input_joining_the_turn());
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    let steer = queued(1, "use the cache", Delivery::Steer);
    handle
        .control
        .send(SessionControl::Deliver(steer.clone()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(dirs.work.join("go"), "").unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Session(SessionEvent::Delivered { .. }))
        })
        .await,
    );
    let (events, end) = finish(seen, handle).await;

    let order = names(&events);
    let finished_at = order
        .iter()
        .position(|n| n == "agent:tool_finished")
        .unwrap();
    let delivered_at = order.iter().position(|n| n == "delivered").unwrap();
    assert!(finished_at < delivered_at, "{order:?}");
    assert!(dirs.read("got-input").contains("use the cache"));
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.result_text.as_deref(), Some("done"));
}

#[tokio::test]
async fn a_rejected_mcp_config_ends_a_claude_session_as_adapter_broken() {
    // Claude Code rejected the status server at init, ran on, and exited 0 with a
    // clean result.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../adapters/claude-code/fixtures")
        .canonicalize()
        .unwrap();
    let quoted = |name: &str| {
        let path = fixtures.join(name).display().to_string();
        format!("'{}'", path.replace('\'', r"'\''"))
    };
    let script = format!(
        "cat {}; cat {} >&2",
        quoted("status-tool-error.stdout.jsonl"),
        quoted("status-tool-error.stderr.txt"),
    );
    let adapter = TestAdapter::new(&script).with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let dirs = Dirs::new();
    let (_, end) = collect(h.worker.start(dirs.spec("hi")).await.unwrap()).await;
    match &end.reason {
        EndReason::AdapterBroken { detail } => {
            assert!(detail.contains("argonauts_status"), "{detail}");
        }
        other => panic!("expected adapter broken, got {other:?}"),
    }
    assert_eq!(end.result_text.as_deref(), Some("ok"));
}

#[tokio::test]
async fn next_turn_messages_wait_for_the_result() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; {}; {}; \
         while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; {}; done",
        echo(&message("working")),
        wait_for_go(),
        echo(&result("turn 1")),
        echo(&result("turn 2")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_input_lines());
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| is_agent_message(e, "working")).await;
    let later = queued(3, "then update the docs", Delivery::NextTurn);
    handle
        .control
        .send(SessionControl::Deliver(later.clone()))
        .await
        .unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Session(SessionEvent::Queued(_)))
        })
        .await,
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;
    let order = names(&events);
    let first_result = order.iter().position(|n| n == "agent:result").unwrap();
    let delivered = order.iter().position(|n| n == "delivered").unwrap();
    assert!(first_result < delivered, "{order:?}");
    assert!(dirs.read("got-input").contains("then update the docs"));
    assert_eq!(end.result_text.as_deref(), Some("turn 2"));
}

#[tokio::test]
async fn stop_after_tool_waits_for_the_tool() {
    let dirs = Dirs::new();
    let script = format!(
        "{}; {}; {}; exec sleep 30",
        echo(&tool("t1", true)),
        wait_for_go(),
        echo(&tool("t1", false)),
    );
    let h = TestDriver::new(TestAdapter::new(&script));
    let mut handle = h.worker.start(dirs.spec("hi")).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::ToolStarted { .. }))
    })
    .await;
    handle
        .control
        .send(SessionControl::StopAfterTool)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        handle.ended.try_recv().is_err(),
        "it must not stop while the tool runs"
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;
    assert_eq!(end.reason, EndReason::Paused);
    assert!(names(&events).contains(&"agent:tool_finished".to_owned()));
}

#[tokio::test]
async fn stop_plan_kills_leftovers() {
    let dirs = Dirs::new();
    // A child that leaves the group for one of its own, as Codex's shells do on macOS.
    let script = "perl -e 'setpgrp(0, 0); \
                  open(my $f, \">>\", \"pids\"); print $f \"$$\\n\"; close $f; sleep 300' \
                  </dev/null >/dev/null 2>&1 & \
                  while [ ! -s pids ]; do sleep 0.02; done; : > ready; exec sleep 300";
    let h = TestDriver::new(TestAdapter::new(script));
    let handle = h.worker.start(dirs.spec("hi")).await.unwrap();
    wait_for_file(&dirs.work.join("ready")).await;
    let leftover: i32 = dirs.read("pids").trim().parse().unwrap();
    let _cleanup = KillOnDrop(leftover);
    assert!(alive(leftover));
    handle.control.send(SessionControl::Stop).await.unwrap();
    let (_, end) = collect(handle).await;
    assert_eq!(end.reason, EndReason::Stopped);
    wait_until_gone(leftover).await;
}

#[tokio::test]
async fn inactivity_stops_a_silent_process() {
    let dirs = Dirs::new();
    let h = TestDriver::new(TestAdapter::new("exec sleep 30"));
    let mut spec = dirs.spec("hi");
    spec.limits.inactivity_min = Some(0.01);
    let started = std::time::Instant::now();
    let (_, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(
        end.reason,
        EndReason::Stuck {
            watchdog: Watchdog::Inactivity,
            calls: None
        }
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A CLI waiting on a question of yours sends nothing and uses no CPU, so while the question is
/// open the silence isn't inactivity; once it's answered the clock runs again.
#[tokio::test]
async fn an_open_question_holds_the_inactivity_stop_until_answered() {
    for answered in [false, true] {
        let dirs = Dirs::new();
        let h = TestDriver::new(TestAdapter::new("exec sleep 2"));
        let mut spec = dirs.spec("hi");
        spec.limits.inactivity_min = Some(0.01);
        let handle = h.worker.start(spec).await.unwrap();
        handle.control.send(SessionControl::Asking).await.unwrap();
        if answered {
            handle.control.send(SessionControl::Answered).await.unwrap();
        }
        let (_, end) = collect(handle).await;
        let inactive = matches!(
            end.reason,
            EndReason::Stuck {
                watchdog: Watchdog::Inactivity,
                ..
            }
        );
        assert_eq!(inactive, answered, "{:?}", end.reason);
    }
}

#[tokio::test]
async fn repeat_gets_a_note_then_stuck() {
    let dirs = Dirs::new();
    let mut calls = String::new();
    for n in 0..30 {
        let id = format!("t{n}");
        calls.push_str(&format!(
            "{}; {}; sleep 0.05; ",
            echo(&tool(&id, true)),
            echo(&tool(&id, false))
        ));
    }
    // With job control off, dash gives a background job /dev/null for input, so the job reads
    // the session's input through a copy taken first.
    let script =
        format!("IFS= read -r first; exec 3<&0; cat <&3 >> got-input & {calls} exec sleep 30");
    let h = TestDriver::new(TestAdapter::new(&script).with_input_lines());
    let mut spec = dirs.spec("hi");
    spec.limits.repeat_limit = Some(3);
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(
        end.reason,
        EndReason::Stuck {
            watchdog: Watchdog::Repeat,
            calls: Some(CallCount {
                name: "Bash".into(),
                times: 5
            })
        }
    );
    let nudged = session_events(&events)
        .into_iter()
        .any(|e| matches!(e, SessionEvent::Limit(LimitEvent::Nudged { .. })));
    assert!(nudged, "{:?}", names(&events));
    let note = inputs(&events)
        .into_iter()
        .any(|input| input.parts.iter().any(|part| part.text == REPEAT_NOTE));
    assert!(note);
    let got = dirs.read("got-input");
    assert!(got.contains("same tool call"), "the note went in: {got}");
}

/// A later try on committed work whose calls change nothing gets its own wrap-up, then stops.
#[tokio::test]
async fn a_later_try_that_changes_nothing_is_asked_for_the_checks_then_stopped() {
    let dirs = Dirs::new();
    let mut calls = String::new();
    for n in 0..30 {
        let started = event_line(&AgentEvent::ToolStarted {
            id: format!("t{n}"),
            name: "Bash".into(),
            input: serde_json::json!({ "command": format!("cat file-{n}") }),
            parent: None,
        });
        calls.push_str(&format!(
            "{}; {}; sleep 0.05; ",
            echo(&started),
            echo(&tool(&format!("t{n}"), false))
        ));
    }
    let script =
        format!("IFS= read -r first; exec 3<&0; cat <&3 >> got-input & {calls} exec sleep 30");
    let h = TestDriver::new(TestAdapter::new(&script).with_input_lines());
    let mut spec = dirs.spec("hi");
    spec.limits.no_progress_calls = Some(4);
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(
        end.reason,
        EndReason::Stuck {
            watchdog: Watchdog::NoProgress,
            calls: Some(CallCount {
                name: "Bash".into(),
                times: 7
            })
        }
    );
    let wrap_ups: Vec<String> = inputs(&events)
        .into_iter()
        .flat_map(|input| input.parts.iter())
        .filter(|part| part.kind == InputPartKind::WrapUp)
        .map(|part| part.text.clone())
        .collect();
    assert_eq!(wrap_ups, [crate::process::watchdogs::NO_PROGRESS_WRAP_UP]);
    let got = dirs.read("got-input");
    assert!(
        got.contains("Run the checks once"),
        "the wrap-up went in: {got}"
    );
}

#[tokio::test]
async fn limit_wraps_up_then_stops() {
    let dirs = Dirs::new();
    let usage = |cost: f64| {
        event_line(&AgentEvent::Usage {
            input_tokens: 10,
            output_tokens: 10,
            cached_tokens: 0,
            cost_usd: Some(cost),
            cumulative: true,
        })
    };
    let script = format!(
        "IFS= read -r first; exec 3<&0; cat <&3 >> got-input & {}; sleep 0.3; {}; exec sleep 30",
        echo(&usage(0.9)),
        echo(&usage(1.2)),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_input_lines());
    let mut spec = dirs.spec("hi");
    spec.limits.limit = Some(LimitAmount {
        unit: LimitUnit::Usd,
        amount: 1.0,
    });
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(
        end.reason,
        EndReason::LimitHit {
            dimension: LimitDimension::Spend
        }
    );
    let wrap_ups: Vec<_> = session_events(&events)
        .into_iter()
        .filter(|e| matches!(e, SessionEvent::Limit(LimitEvent::WrapUpSent { .. })))
        .collect();
    assert_eq!(
        wrap_ups,
        [&SessionEvent::Limit(LimitEvent::WrapUpSent {
            via: WrapUpVia::Input,
            dimension: LimitDimension::Spend
        })]
    );
    assert!(
        dirs.read("got-input")
            .contains("limit for this task is almost used up")
    );
    let sent = inputs(&events)
        .into_iter()
        .any(|input| input.parts.iter().any(|part| part.text == WRAP_UP_MESSAGE));
    assert!(sent);
    assert!(end.spend.cost_usd >= 1.2);
}

/// A session that never answers is stopped once its time limit is up, so whoever waits on it
/// waits no longer than that.
#[tokio::test]
async fn a_session_that_never_answers_stops_at_its_time_limit() {
    let dirs = Dirs::new();
    let script = "IFS= read -r first; cat <&0 > /dev/null & exec sleep 30";
    let h = TestDriver::new(TestAdapter::new(script).with_input_lines());
    let mut spec = dirs.spec("hi");
    spec.limits.max_minutes = Some(0.01);
    let (_events, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(
        end.reason,
        EndReason::LimitHit {
            dimension: LimitDimension::Minutes
        }
    );
    assert!(end.duration_ms < 20_000, "{}ms", end.duration_ms);
}

fn registry_entry(context_window: u64) -> ModelEntry {
    ModelEntry {
        id: ModelRef::new("fake", None),
        provider: "fake".into(),
        kind: RegistryKind::Cli,
        access: ModelAccess::Subscription,
        tools: true,
        cost: CostModel {
            unit: CostUnit::None,
            input_per_mtok: None,
            output_per_mtok: None,
            cached_input_per_mtok: None,
        },
        strengths: Vec::new(),
        good_for: Vec::new(),
        context_window: Some(context_window),
        efforts: Vec::new(),
        aliases: Vec::new(),
        retired: false,
        successor: None,
        discovered: false,
        cli_default: false,
        default_effort: None,
        launch: None,
        tier: None,
        name: None,
        description: None,
        window: None,
        ram_bytes: None,
        thinking: None,
        variants: None,
        listed_cost: false,
    }
}

#[tokio::test]
async fn context_cap_rotates() {
    let dirs = Dirs::new();
    let fill = event_line(&AgentEvent::ContextFill {
        used: 150_000,
        size: None,
        sent: None,
    });
    let script = format!("{}; exec sleep 30", echo(&fill));
    let mut settings = Settings::default();
    settings.sessions.rotate_at = RotateAt::Tokens(100_000);
    let registry = Arc::new(StaticRegistry::default());
    registry.add(registry_entry(200_000));
    let h = TestDriver::with_settings(TestAdapter::new(&script), settings);
    let worker = h.worker.with_registry(registry);
    let (events, end) = collect(worker.start(dirs.spec("hi")).await.unwrap()).await;
    assert_eq!(end.reason, EndReason::ContextFull);
    let filled = agent_events(&events).into_iter().any(|e| {
        matches!(
            e,
            AgentEvent::ContextFill {
                used: 150_000,
                size: Some(200_000),
                sent: Some(_),
            }
        )
    });
    assert!(filled, "the window size comes from the registry");
}

#[tokio::test]
async fn a_watched_session_is_asked_for_its_context_after_a_fill_and_counts_what_was_sent() {
    let dirs = Dirs::new();
    let fill = |used| {
        echo(&event_line(&AgentEvent::ContextFill {
            used,
            size: Some(200_000),
            sent: None,
        }))
    };
    // The second fill comes within 10 seconds of the ask, so it asks nothing more.
    let script = format!(
        "IFS= read -r first; IFS= read -r steer; {}; IFS= read -r ask; \
         printf '%s\\n' \"$ask\" >> got-input; {}; {}; \
         while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; done",
        fill(1000),
        fill(2000),
        echo(&result("done")),
    );
    let h = TestDriver::new(
        TestAdapter::new(&script)
            .with_send_now()
            .with_context_asks(),
    );
    let handle = h.worker.start(dirs.spec("start")).await.unwrap();
    handle
        .control
        .send(SessionControl::WatchContext(Some(ContextDetail::Full)))
        .await
        .unwrap();
    // Sent after the watch, so the fill that follows it is watched.
    let steer = queued(1, "use the cache", Delivery::Steer);
    handle
        .control
        .send(SessionControl::Deliver(steer))
        .await
        .unwrap();
    let (events, end) = collect(handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    let got = dirs.read("got-input");
    let asks: Vec<&str> = got
        .lines()
        .filter(|line| line.contains("context"))
        .collect();
    // The watched ask, then the short count every turn is asked for as it ends.
    assert_eq!(
        asks,
        [r#"{"context":"full"}"#, r#"{"context":"summary"}"#],
        "{got}"
    );
    let sent: Vec<u64> = agent_events(&events)
        .into_iter()
        .filter_map(|e| match e {
            AgentEvent::ContextFill { sent, .. } => *sent,
            _ => None,
        })
        .collect();
    let [first, second] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    // The prompt and the delivered message, by the limit's estimate.
    assert!(*first > 0 && first == second, "{sent:?}");
}

/// A run's agent is asked for the short count after a fill though nothing shows its session,
/// and what goes on to the run's journal is its rows without the grid and the lists. The whole
/// count stays in the session's folder.
#[tokio::test]
async fn a_crew_agents_session_is_counted_in_short_and_keeps_only_its_rows() {
    use argonauts_core::runs::events::context::{
        ContextCategory, ContextCell, ContextKind, ContextUsage, MemoryFile,
    };

    let dirs = Dirs::new();
    let fill = echo(&event_line(&AgentEvent::ContextFill {
        used: 36_200,
        size: Some(200_000),
        sent: None,
    }));
    let row = ContextCategory {
        name: "Messages".into(),
        tokens: 17_500,
        color: "purple_FOR_SUBAGENTS_ONLY".into(),
        kind: ContextKind::Used,
    };
    let counted = AgentEvent::ContextBreakdown {
        usage: Box::new(ContextUsage {
            categories: vec![row.clone()],
            total_tokens: 36_200,
            window: 200_000,
            raw_window: 200_000,
            percentage: 18,
            grid: vec![vec![ContextCell {
                category: "Messages".into(),
                color: "purple_FOR_SUBAGENTS_ONLY".into(),
                filled: true,
                tokens: 17_500,
                percentage: 9,
                fullness: 1.0,
            }]],
            memory_files: vec![MemoryFile {
                path: "CLAUDE.md".into(),
                scope: "Project".into(),
                tokens: 3_700,
            }],
            ..ContextUsage::default()
        }),
    };
    let script = format!(
        "IFS= read -r first; {fill}; IFS= read -r ask; \
         printf '%s\\n' \"$ask\" >> got-input; {}; {}; \
         while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; done",
        echo(&event_line(&counted)),
        echo(&result("done")),
    );
    let h = TestDriver::new(
        TestAdapter::new(&script)
            .with_send_now()
            .with_context_asks(),
    );
    let mut spec = dirs.spec("start");
    spec.run = Some(argonauts_core::RunId::new("run-20261010-140000-1a2b").unwrap());
    let session_dir = spec.session_dir.clone();
    let handle = h.worker.start(spec).await.unwrap();
    let (events, end) = collect(handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    let whole: ContextUsage = serde_json::from_slice(
        &std::fs::read(session_dir.join(session_files::CONTEXT)).expect("context.json"),
    )
    .expect("a whole count");
    let AgentEvent::ContextBreakdown { usage: sent } = &counted else {
        unreachable!()
    };
    assert_eq!(whole, **sent);
    // The first fill's ask, then the turn's end's.
    let asks: Vec<String> = dirs.read("got-input").lines().map(str::to_owned).collect();
    assert_eq!(asks, [r#"{"context":"summary"}"#; 2]);
    let kept: Vec<&ContextUsage> = agent_events(&events)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ContextBreakdown { usage } => Some(&**usage),
            _ => None,
        })
        .collect();
    let [kept] = kept.as_slice() else {
        panic!("{kept:?}");
    };
    assert_eq!(kept.categories, [row]);
    assert_eq!((kept.total_tokens, kept.window), (36_200, 200_000));
    assert!(kept.grid.is_empty() && kept.memory_files.is_empty());
}

/// The count every turn is asked for as it ends, in short: one ask after the result, and the
/// input closes as soon as the count comes.
#[tokio::test]
async fn a_turn_is_counted_as_it_ends_and_its_input_closes_on_the_count() {
    use argonauts_core::runs::events::context::ContextUsage;

    let dirs = Dirs::new();
    let counted = AgentEvent::ContextBreakdown {
        usage: Box::new(ContextUsage {
            total_tokens: 27_755,
            window: 1_000_000,
            ..ContextUsage::default()
        }),
    };
    let script = format!(
        "IFS= read -r first; {}; IFS= read -r ask; printf '%s\\n' \"$ask\" >> got-input; {}; \
         while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; done",
        echo(&result("done")),
        echo(&event_line(&counted)),
    );
    let h = TestDriver::new(
        TestAdapter::new(&script)
            .with_input_lines()
            .with_context_asks(),
    );
    let started = std::time::Instant::now();
    let handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let (events, end) = collect(handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(dirs.read("got-input").trim(), r#"{"context":"summary"}"#);
    let totals: Vec<u64> = agent_events(&events)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ContextBreakdown { usage } => Some(usage.total_tokens),
            _ => None,
        })
        .collect();
    assert_eq!(totals, [27_755]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the input closed on the count, not when the wait for it ran out"
    );
}

/// A recorded Claude Code turn, whose count was asked just after its result and came 10 ms
/// later: the runner asks as the turn ends, and the reply, grid and all, closes the input.
#[tokio::test]
async fn a_recorded_claude_code_turn_ends_with_its_count() {
    let stream = recorded("context_usage_summary");
    let script = format!(
        "IFS= read -r first; sed -n '1,9p' {stream}; \
         IFS= read -r ask; printf '%s\\n' \"$ask\" >> got-input; sed -n '10p' {stream}; \
         while IFS= read -r line; do :; done"
    );
    let adapter = TestAdapter::new(&script)
        .with_input_lines()
        .with_context_asks()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let dirs = Dirs::new();
    let started = std::time::Instant::now();
    let handle = h
        .worker
        .start(dirs.spec("Reply with only the word: ready"))
        .await
        .unwrap();
    let (events, end) = collect(handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(dirs.read("got-input").trim(), r#"{"context":"summary"}"#);
    let counts: Vec<(u64, usize)> = agent_events(&events)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ContextBreakdown { usage } => Some((usage.total_tokens, usage.grid.len())),
            _ => None,
        })
        .collect();
    assert_eq!(counts, [(24_010, 10)], "the recorded count, its grid whole");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the input closed on the count"
    );
}

/// A count that never comes holds the input open only a short while.
#[tokio::test]
async fn a_turn_whose_count_never_comes_closes_its_input_after_a_short_wait() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; \
         while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; done",
        echo(&result("done")),
    );
    let h = TestDriver::new(
        TestAdapter::new(&script)
            .with_input_lines()
            .with_context_asks(),
    );
    let started = std::time::Instant::now();
    let handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let (_, end) = collect(handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(dirs.read("got-input").trim(), r#"{"context":"summary"}"#);
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(1_900),
        "it waited for the count: {waited:?}"
    );
}

#[tokio::test]
async fn plan_usage_goes_to_the_board_and_out() {
    let dirs = Dirs::new();
    let reading = argonauts_core::models::limits::PlanUsageReading {
        agent_id: argonauts_core::base::names::AgentId("fake".into()),
        status: argonauts_core::models::limits::PlanUsageStatus::Rejected,
        used: Some(1.0),
        resets_at: None,
        window: Some("five_hour".into()),
        source: argonauts_core::models::limits::PlanUsageSource::RateLimitEvent,
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
        detail: None,
        wrap_up: false,
    };
    let script = format!(
        "{}; exit 1",
        echo(&event_line(&AgentEvent::PlanUsage {
            reading: reading.clone()
        }))
    );
    let h = TestDriver::new(TestAdapter::new(&script));
    let (events, end) = collect(h.worker.start(dirs.spec("hi")).await.unwrap()).await;
    use argonauts_core::models::limits::PlanUsageBoard;
    assert_eq!(h.plan_usage.get(&reading.agent_id), Some(reading.clone()));
    assert!(events.contains(&RunnerEvent::AgentStatus(
        argonauts_core::runs::events::AgentStatusEvent::PlanUsage(reading)
    )));
    assert_eq!(end.reason, EndReason::UsageLimit { resets_at: None });
}

fn wrap_up_reading(wrap_up: bool) -> argonauts_core::models::limits::PlanUsageReading {
    argonauts_core::models::limits::PlanUsageReading {
        agent_id: argonauts_core::base::names::AgentId("fake".into()),
        status: argonauts_core::models::limits::PlanUsageStatus::Rejected,
        used: Some(1.0),
        resets_at: None,
        window: Some("five_hour".into()),
        source: argonauts_core::models::limits::PlanUsageSource::RateLimitEvent,
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
        detail: None,
        wrap_up,
    }
}

/// On the wrap-up allowance the agent finishes: the runner acts on the session's end, not on
/// the reading, so a clean end completes while the agent stays out for new work.
#[tokio::test]
async fn a_session_on_its_wrap_up_allowance_finishes() {
    let dirs = Dirs::new();
    let script = [
        echo(&event_line(&AgentEvent::PlanUsage {
            reading: wrap_up_reading(false),
        })),
        echo(&event_line(&AgentEvent::PlanUsage {
            reading: wrap_up_reading(true),
        })),
        echo(&message("Done with this step. Left: format_cents.")),
        echo(&event_line(&AgentEvent::PlanUsage {
            reading: wrap_up_reading(false),
        })),
        echo(&result("done")),
    ]
    .join("; ");
    let h = TestDriver::new(TestAdapter::new(&script));
    let (_, end) = collect(h.worker.start(dirs.spec("hi")).await.unwrap()).await;
    assert_eq!(end.reason, EndReason::Completed);
    use argonauts_core::models::limits::PlanUsageBoard;
    let out = h
        .plan_usage
        .get(&argonauts_core::base::names::AgentId("fake".into()));
    assert_eq!(out, Some(wrap_up_reading(false)));
}

/// `fallback.wrap_up = "fallback"` saves the allowance: the first sign ends the session on the
/// limit, so its task moves on at once.
#[tokio::test]
async fn fallback_on_wrap_up_stops_at_the_first_sign() {
    let dirs = Dirs::new();
    let script = format!(
        "{}; {}",
        echo(&event_line(&AgentEvent::PlanUsage {
            reading: wrap_up_reading(true),
        })),
        "sleep 20"
    );
    let mut settings = Settings::default();
    settings.fallback.wrap_up = argonauts_core::settings::fallback::WrapUp::Fallback;
    let h = TestDriver::with_settings(TestAdapter::new(&script), settings);
    let (_, end) = collect(h.worker.start(dirs.spec("hi")).await.unwrap()).await;
    assert_eq!(end.reason, EndReason::UsageLimit { resets_at: None });
}

/// A plan window as full as `limits.plan_usage` ends the session: as a plan that ran out, so
/// the task falls back, or as a reached limit when `when_reached` is `stop`. With no limit
/// set, the same reading stops nothing.
#[tokio::test]
async fn your_plan_limit_ends_the_session_as_when_reached_says() {
    use argonauts_core::settings::limit::OnHit;

    let reading = argonauts_core::models::limits::PlanUsageReading {
        status: argonauts_core::models::limits::PlanUsageStatus::Allowed,
        used: Some(0.82),
        ..wrap_up_reading(false)
    };
    let reading_line = echo(&event_line(&AgentEvent::PlanUsage { reading }));
    let run = async |plan_usage: Option<f64>, when_reached: OnHit, then: String| {
        let dirs = Dirs::new();
        let mut settings = Settings::default();
        settings.limits.spend.plan_usage = plan_usage;
        settings.limits.spend.when_reached = when_reached;
        let script = format!("{reading_line}; {then}");
        let h = TestDriver::with_settings(TestAdapter::new(&script), settings);
        collect(h.worker.start(dirs.spec("hi")).await.unwrap())
            .await
            .1
            .reason
    };
    assert_eq!(
        run(Some(0.8), OnHit::Fallback, "sleep 20".to_owned()).await,
        EndReason::UsageLimit { resets_at: None }
    );
    assert_eq!(
        run(Some(0.8), OnHit::Stop, "sleep 20".to_owned()).await,
        EndReason::LimitHit {
            dimension: LimitDimension::Spend
        }
    );
    let carries_on = [echo(&message("hi")), echo(&result("ok"))].join("; ");
    assert_eq!(
        run(Some(0.9), OnHit::Fallback, carries_on.clone()).await,
        EndReason::Completed
    );
    assert_eq!(
        run(None, OnHit::Fallback, carries_on).await,
        EndReason::Completed
    );
}

/// A session never speaks of its CLI's version, newer or not: start-up, Doctor and the Status
/// tab do. What it sent that the adapter didn't read is kept by kind for `/doctor`.
#[tokio::test]
async fn a_session_says_nothing_of_a_newer_cli_and_keeps_its_unread_kinds() {
    let dirs = Dirs::new();
    let started = event_line(&AgentEvent::Init {
        session_id: Some("s1".into()),
        model: None,
        tools: Vec::new(),
        plugins: Vec::new(),
        capabilities: Vec::new(),
        version: Some("1.0.3".into()),
    });
    let odd = event_line(&AgentEvent::Unknown {
        raw: serde_json::json!({"type": "system", "subtype": "turn_summary", "text": "secret"}),
    });
    let script = [
        echo(&started),
        echo(&odd),
        echo(&message("hi")),
        echo(&result("ok")),
    ]
    .join("; ");
    let h = TestDriver::new(TestAdapter::new(&script));
    let (events, _) = collect(h.worker.start(dirs.spec("hi")).await.unwrap()).await;
    assert!(
        !notices(&events)
            .iter()
            .any(|notice| notice.contains("newer") || notice.contains("is out")),
        "{:?}",
        notices(&events)
    );

    let path = argonauts_core::Paths::new(h.home.path()).cli_versions();
    let kept = crate::boards::cli_versions::CliVersions::load(&path);
    assert_eq!(
        kept.unread("fake", "1.0.3"),
        [("system/turn_summary".to_owned(), 1)]
    );
    assert!(!std::fs::read_to_string(&path).unwrap().contains("secret"));
}

/// A new Claude Code session is given its id at launch, but counts as one only once the CLI
/// says it started: one cut off before that saved nothing a resume could find.
#[tokio::test]
async fn claude_sessions_announce_their_cap_and_count_once_started() {
    let run = |script: String| async move {
        let dirs = Dirs::new();
        let adapter = TestAdapter::new(&script)
            .with_id(super::capabilities::CLAUDE_CODE)
            .with_input_lines();
        let h = TestDriver::new(adapter);
        let mut spec = dirs.spec("hi");
        spec.limits.limit = Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount: 2.0,
        });
        let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
        assert!(dirs.session.join(session_files::MCP_CONFIG).exists());
        assert_eq!(end.reason, EndReason::Completed);
        (events, end)
    };

    let (events, end) = run(format!("IFS= read -r first; {}", echo(&result("ok")))).await;
    let order = names(&events);
    assert_eq!(order[..2], ["input", "limit:native_cap"], "{order:?}");
    assert!(!order.iter().any(|name| name == "cli_session"), "{order:?}");
    assert_eq!(end.cli_session, None);

    let started = "2c1f0a9e-1b2c-4d5e-8f90-123456789abc";
    let script = format!(
        "IFS= read -r first; {}; {}",
        echo(&init(started)),
        echo(&result("ok"))
    );
    let (events, end) = run(script).await;
    let announced = session_events(&events).into_iter().find_map(|e| match e {
        SessionEvent::CliSession { id, .. } => Some(id.clone()),
        _ => None,
    });
    assert_eq!(announced.as_deref(), Some(started));
    assert_eq!(end.cli_session.as_deref(), Some(started));
}

#[tokio::test]
async fn unknown_lines_piling_up_disable_the_agent() {
    let dirs = Dirs::new();
    let script = "i=0; while [ $i -lt 30 ]; do echo \"what is this $i\"; i=$((i+1)); done";
    let h = TestDriver::new(TestAdapter::new(script));
    let (events, _) = collect(h.worker.start(dirs.spec("hi")).await.unwrap()).await;
    use argonauts_core::models::limits::HealthBoard;
    let disabled = h.health.state(&h.worker.agent_id()).disabled;
    assert!(disabled.unwrap().contains("Pin a tested version: 1.0.0"));
    let told = events.iter().any(|event| {
        matches!(
            event,
            RunnerEvent::AgentStatus(
                argonauts_core::runs::events::AgentStatusEvent::Disabled { .. }
            )
        )
    });
    assert!(told, "{:?}", names(&events));
    // The next session is refused.
    assert!(matches!(
        h.worker.start(dirs.spec("again")).await,
        Err(RunnerError::Disabled { .. })
    ));
}

#[tokio::test]
async fn cli_worker_driver_branch_keeps_the_rules() {
    let repo = TestRepo::new();
    let dirs = Dirs::in_repo(&repo);
    let script = format!(
        "IFS= read -r prompt; printf '%s' \"${SESSION_ENV}\" > ../got-session; \
         echo changed >> README.md; {}; {}; echo 'driver note' >&2; {}; \
         IFS= read -r more",
        echo(&init("acp-1")),
        echo(&event_line(&AgentEvent::FileChanged {
            path: "README.md".into(),
            kind: "update".into()
        })),
        echo(&result("done")),
    );
    let driver = FakeDriver::default();
    let h = TestDriver::new(TestAdapter::new(&script).with_driver(driver.clone()));

    let mut build = dirs.spec("build");
    build.access = Access::Build;
    assert!(matches!(
        h.worker.start(build).await,
        Err(RunnerError::Refused(_))
    ));

    let mut spec = dirs.spec("fix the readme");
    spec.status_file = Some(dirs.session.join(session_files::STATUS));
    let key = spec.key.clone();
    let (events, end) = collect(h.worker.start(spec.clone()).await.unwrap()).await;

    let RunnerEvent::Session(SessionEvent::Input(first)) = &events[0] else {
        panic!("{:?}", names(&events));
    };
    assert_eq!(first.parts, spec.prompt);
    assert_eq!(first.launch.prompt_via, PromptVia::StreamInput);
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.cli_session.as_deref(), Some("acp-1"));
    assert_eq!(end.result_text.as_deref(), Some("done"));

    let got_session = std::fs::read_to_string(repo.dir.path().join("got-session")).unwrap();
    assert_eq!(got_session, key.to_string());
    assert!(dirs.session.join(session_files::PROCESS).exists());
    assert_eq!(std::fs::read_dir(h.agents_dir()).unwrap().count(), 0);

    let sent = std::fs::read_to_string(dirs.session.join(session_files::RAW_STDIN)).unwrap();
    assert!(sent.contains("fix the readme"), "{sent}");
    let received = std::fs::read_to_string(dirs.session.join(session_files::RAW_STDOUT)).unwrap();
    assert!(received.contains("acp-1"), "{received}");
    let stderr = std::fs::read_to_string(dirs.session.join(session_files::RAW_STDERR)).unwrap();
    assert!(stderr.contains("driver note"));

    let diff = session_events(&events).into_iter().find_map(|e| match e {
        SessionEvent::FileDiff(diff) => Some(diff.clone()),
        _ => None,
    });
    let diff = diff.expect("a file diff");
    assert_eq!(diff.path, "README.md");
    assert!(
        diff.added >= 1 && diff.hunk.contains("+changed"),
        "{diff:?}"
    );

    let starts = driver.starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].prompt, prompt_text(&spec.prompt));
    assert_eq!(starts[0].cwd, dirs.work);
    let server = &starts[0].mcp_servers[0];
    assert!(server.command.is_absolute());
    assert_eq!(server.args[..3], ["mcp", "session", "--status-file"]);
}

#[tokio::test]
async fn driver_stop_cancels_then_runs_the_stop_plan() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r prompt; {}; exec sleep 30",
        echo(&message("thinking"))
    );
    let driver = FakeDriver::default();
    let h = TestDriver::new(TestAdapter::new(&script).with_driver(driver.clone()));
    let mut handle = h.worker.start(dirs.spec("hi")).await.unwrap();
    let seen = read_until(&mut handle.events, |e| is_agent_message(e, "thinking")).await;
    handle
        .control
        .send(SessionControl::Deliver(queued(4, "hurry", Delivery::Steer)))
        .await
        .unwrap();
    let mut rest = seen;
    rest.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Session(SessionEvent::Delivered { .. }))
        })
        .await,
    );
    handle.control.send(SessionControl::Stop).await.unwrap();
    let (events, end) = finish(rest, handle).await;
    assert_eq!(end.reason, EndReason::Stopped);
    assert_eq!(
        driver.controls.lock().unwrap().clone(),
        ["deliver now=true", "cancel"]
    );
    let steered = inputs(&events)
        .into_iter()
        .any(|input| input.parts.iter().any(|part| part.text == "hurry"));
    assert!(steered);
}

#[test]
fn take_over_is_the_adapters_interactive_resume() {
    let h = TestDriver::new(TestAdapter::new("true").with_take_over());
    let command = h
        .worker
        .take_over_command("cli-9", Path::new("/w"))
        .unwrap();
    assert_eq!(command.args, ["cli-9"]);
    let h = TestDriver::new(TestAdapter::new("true"));
    assert!(
        h.worker
            .take_over_command("cli-9", Path::new("/w"))
            .is_none()
    );
}

#[tokio::test]
async fn foreign_keys_in_spec_env_never_reach_the_agent() {
    let dirs = Dirs::new();
    let script = format!("env > got-env; {}", echo(&result("ok")));
    let h = TestDriver::new(TestAdapter::new(&script).removing_env(&["OPENAI_API_KEY"]));
    let mut spec = dirs.spec("hi");
    spec.env = vec![
        ("OPENAI_API_KEY".into(), "sk-foreign".into()),
        ("NPM_TOKEN".into(), "npm-secret".into()),
        ("SHARED".into(), "from-spec".into()),
    ];
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(end.reason, EndReason::Completed);
    let env = dirs.read("got-env");
    assert!(env.contains("SHARED=from-spec"), "{env}");
    assert!(!env.contains("sk-foreign"), "{env}");
    assert!(!env.contains("npm-secret"), "{env}");
    let removed = &inputs(&events)[0].launch.env_removed;
    assert!(removed.contains(&"OPENAI_API_KEY".to_owned()));
}

/// An agent on open input that ends its only turn, then waits for `go` before it exits. Resumed,
/// it writes the first input line and the CLI session to `got-resume`.
fn ends_its_turn_then_waits() -> String {
    format!(
        "IFS= read -r first; \
         if [ -z \"$2\" ]; then {}; {}; while IFS= read -r line; do :; done; {}; \
         else printf '%s|%s' \"$first\" \"$2\" > got-resume; {}; fi",
        echo(&init("cli-1")),
        echo(&result("turn 1")),
        wait_for_go(),
        echo(&result("resumed")),
    )
}

/// Starts `adapter`, and sends a message once its last turn has its result.
async fn deliver_after_the_last_result(
    adapter: TestAdapter,
    dirs: &Dirs,
    late: &argonauts_core::runs::events::session::QueuedMessage,
) -> (
    Vec<RunnerEvent>,
    argonauts_core::runs::events::session::SessionEnd,
) {
    let h = TestDriver::new(adapter);
    let mut handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let mut seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::Result { .. }))
    })
    .await;
    handle
        .control
        .send(SessionControl::Deliver(late.clone()))
        .await
        .unwrap();
    seen.extend(
        read_until(&mut handle.events, |e| {
            matches!(e, RunnerEvent::Session(SessionEvent::Queued(_)))
        })
        .await,
    );
    std::fs::write(dirs.work.join("go"), "").unwrap();
    finish(seen, handle).await
}

fn notices(events: &[RunnerEvent]) -> Vec<&str> {
    agent_events(events)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::Error {
                message,
                retryable: true,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_message_after_the_last_result_resumes_the_session() {
    let dirs = Dirs::new();
    let adapter = TestAdapter::new(&ends_its_turn_then_waits()).with_input_lines();
    let late = queued(4, "one more thing", Delivery::Steer);
    let (events, end) = deliver_after_the_last_result(adapter, &dirs, &late).await;

    let got = dirs.read("got-resume");
    assert!(got.contains("one more thing"), "{got}");
    assert!(got.ends_with("|cli-1"), "{got}");
    let delivered = session_events(&events).into_iter().any(|e| {
        *e == SessionEvent::Delivered {
            message: late.id.clone(),
            via: Delivery::NextTurn,
        }
    });
    assert!(delivered, "{:?}", names(&events));
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.result_text.as_deref(), Some("resumed"));
}

#[tokio::test]
async fn a_message_that_never_went_out_is_reported() {
    let dirs = Dirs::new();
    let adapter = TestAdapter::new(&ends_its_turn_then_waits())
        .with_input_lines()
        .without_resume();
    let late = queued(5, "too late", Delivery::NextTurn);
    let (events, end) = deliver_after_the_last_result(adapter, &dirs, &late).await;

    assert!(!dirs.work.join("got-resume").exists());
    let reported = notices(&events)
        .into_iter()
        .any(|notice| notice.contains(late.id.as_str()) && notice.contains("not delivered"));
    assert!(reported, "{:?}", notices(&events));
    assert_eq!(end.reason, EndReason::Completed);
}

#[tokio::test]
async fn stop_kills_a_grandchild_in_a_session_of_its_own() {
    let dirs = Dirs::new();
    // The shell in the middle stays in the group, so only its tree reaches the grandchild
    // until the stop plan kills it.
    let script = r#"sh -c 'perl -e "use POSIX; POSIX::setsid(); open(my \$f, qq(>>), qq(pids)); print \$f qq(\$\$\n); close \$f; sleep 300" </dev/null >/dev/null 2>&1 & wait' & while [ ! -s pids ]; do sleep 0.02; done; : > ready; exec sleep 300"#;
    let h = TestDriver::new(TestAdapter::new(script));
    let handle = h.worker.start(dirs.spec("hi")).await.unwrap();
    wait_for_file(&dirs.work.join("ready")).await;
    let detached: i32 = dirs.read("pids").trim().parse().unwrap();
    let _cleanup = KillOnDrop(detached);
    let sid = nix::unistd::getsid(Some(Pid::from_raw(detached))).unwrap();
    assert_eq!(sid.as_raw(), detached);
    handle.control.send(SessionControl::Stop).await.unwrap();
    let (_, end) = collect(handle).await;
    assert_eq!(end.reason, EndReason::Stopped);
    wait_until_gone(detached).await;
}

#[test]
fn plan_usage_prober_runs_the_binary_doctor_finds() {
    use argonauts_core::Adapter;
    use argonauts_core::settings::agents::AgentSettings;

    use crate::boards::plan_usage::registered_prober;

    let adapter: Arc<dyn Adapter> = Arc::new(TestAdapter::new("true"));
    let worker = ModelRef::new("prober-on-path", None);
    super::register_plan_usage_prober(&adapter, &worker, &Settings::default());
    let prober = registered_prober(&worker.agent_id()).unwrap();
    assert!(prober.binary.is_absolute(), "{}", prober.binary.display());
    assert!(prober.binary.ends_with("sh"));

    let worker = ModelRef::new("prober-relative", None);
    let mut settings = Settings::default();
    settings.agents.insert(
        worker.agent.clone(),
        AgentSettings {
            binary: Some(PathBuf::from("bin/sh")),
            ..AgentSettings::default()
        },
    );
    super::register_plan_usage_prober(&adapter, &worker, &settings);
    assert!(registered_prober(&worker.agent_id()).is_none());
}

#[test]
fn a_cli_that_bills_an_api_key_gets_no_plan_usage_prober() {
    use argonauts_core::Adapter;
    use argonauts_core::settings::agents::AgentSettings;

    use crate::boards::plan_usage::registered_prober;

    let adapter: Arc<dyn Adapter> = Arc::new(TestAdapter::new("true"));
    let worker = ModelRef::new("prober-api-key", None);
    let mut settings = Settings::default();
    settings.agents.insert(
        worker.agent.clone(),
        AgentSettings {
            auth: argonauts_core::AuthMode::ApiKey,
            ..AgentSettings::default()
        },
    );
    super::register_plan_usage_prober(&adapter, &worker, &settings);
    assert!(registered_prober(&worker.agent_id()).is_none());
}

#[test]
fn plan_usage_prober_names_only_the_configured_model() {
    use argonauts_core::Adapter;
    use argonauts_core::settings::agents::AgentSettings;

    use crate::boards::plan_usage::registered_prober;

    let adapter: Arc<dyn Adapter> = Arc::new(TestAdapter::new("true"));
    let worker = ModelRef::new("prober-model-from-agent", None);
    let mut settings = Settings::default();
    settings.agents.insert(
        worker.agent.clone(),
        AgentSettings {
            model: Some("cheap-model".to_owned()),
            ..AgentSettings::default()
        },
    );
    super::register_plan_usage_prober(&adapter, &worker, &settings);
    let prober = registered_prober(&worker.agent_id()).unwrap();
    assert_eq!(prober.model.as_deref(), Some("cheap-model"));

    // A runner built for another model, such as a cheap judge, leaves the probe on yours.
    let worker = ModelRef::new("prober-model-from-worker", Some("picked".to_owned()));
    settings.agents.insert(
        worker.agent.clone(),
        settings.agents["prober-model-from-agent"].clone(),
    );
    super::register_plan_usage_prober(&adapter, &worker, &settings);
    let prober = registered_prober(&worker.agent_id()).unwrap();
    assert_eq!(prober.model.as_deref(), Some("cheap-model"));

    // With none set, the CLI probes on the model it picks itself.
    let worker = ModelRef::new("prober-model-unset", Some("picked".to_owned()));
    settings
        .agents
        .insert(worker.agent.clone(), AgentSettings::default());
    super::register_plan_usage_prober(&adapter, &worker, &settings);
    let prober = registered_prober(&worker.agent_id()).unwrap();
    assert_eq!(prober.model, None);
}

fn leak_line() -> String {
    event_line(&AgentEvent::IsolationLeak {
        plugins: vec!["memory-keeper".into(), "lint-helper".into()],
        skills: Vec::new(),
        builtins: Vec::new(),
    })
}

/// A blind role's session that loaded your plugins is stopped at once, before any tool call,
/// and ends saying what it loaded so the role can go to another agent.
#[tokio::test]
async fn an_isolated_session_that_loads_your_plugins_is_stopped() {
    let dirs = Dirs::new();
    let script = format!(
        "{}; {}; exec sleep 30",
        echo(&init("s1")),
        echo(&leak_line())
    );
    let h = TestDriver::new(TestAdapter::new(&script).isolating());
    let mut spec = dirs.spec("review");
    spec.isolated = true;
    let started = std::time::Instant::now();
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(
        end.reason,
        EndReason::IsolationBroken {
            plugins: vec!["memory-keeper".into(), "lint-helper".into()],
            skills: Vec::new(),
        }
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    let tools = agent_events(&events)
        .into_iter()
        .filter(|event| matches!(event, AgentEvent::ToolStarted { .. }))
        .count();
    assert_eq!(tools, 0);
}

/// A later version's built-in that no list here names is the CLI's own, not yours: the blind
/// session runs on, says so once, and every later isolated session turns it off.
#[tokio::test]
async fn a_builtin_argonauts_doesnt_know_is_learned_and_turned_off_next_time() {
    let learned = event_line(&AgentEvent::IsolationLeak {
        plugins: Vec::new(),
        skills: Vec::new(),
        builtins: vec!["cc-plugin-new-thing".into()],
    });
    let script = format!(
        "printf '%s' \"$FAKE_BUILTINS_OFF\" > builtins-off; {}; {}; {}",
        echo(&init("s1")),
        echo(&learned),
        echo(&result("reviewed"))
    );
    let h = TestDriver::new(TestAdapter::new(&script).isolating());
    let dirs = Dirs::new();
    let mut spec = dirs.spec("review");
    spec.isolated = true;
    let (events, end) = collect(h.worker.start(spec.clone()).await.unwrap()).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(dirs.read("builtins-off"), "");
    let said: Vec<&str> = notices(&events)
        .into_iter()
        .filter(|notice| notice.contains("doesn't know yet"))
        .collect();
    assert_eq!(said.len(), 1, "{:?}", notices(&events));
    assert!(said[0].contains("cc-plugin-new-thing"), "{said:?}");
    let path = argonauts_core::Paths::new(h.home.path()).cli_versions();
    let kept = crate::boards::cli_versions::CliVersions::load(&path);
    assert_eq!(kept.builtins("fake"), ["cc-plugin-new-thing"]);

    let (events, _) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(dirs.read("builtins-off"), "cc-plugin-new-thing");
    assert!(
        !notices(&events)
            .iter()
            .any(|notice| notice.contains("doesn't know yet")),
        "said once"
    );
}

/// A CLI that can't keep your MCP servers and plugins out says nothing of what it loaded, so a
/// blind session on it says so itself, the first one only, since the rest would say the same.
/// One that isolates needs no such word.
#[tokio::test]
async fn a_blind_session_on_a_cli_that_cant_isolate_says_so() {
    let script = format!("{}; {}", echo(&init("s1")), echo(&result("reviewed")));
    for (adapter, says) in [
        (TestAdapter::new(&script), [true, false]),
        (TestAdapter::new(&script).isolating(), [false, false]),
    ] {
        let h = TestDriver::new(adapter);
        for says in says {
            let dirs = Dirs::new();
            let mut spec = dirs.spec("review");
            spec.isolated = true;
            let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
            assert_eq!(end.reason, EndReason::Completed);
            let said = notices(&events)
                .into_iter()
                .any(|notice| notice.contains("can't keep its own MCP servers"));
            assert_eq!(said, says, "{:?}", notices(&events));
        }
    }
}

/// With `sandbox.isolated_leak = "warn"` it runs on, and a notice says what it loaded. A
/// session that isn't isolated is just running with your setup.
#[tokio::test]
async fn a_leak_only_warns_when_set_to_and_means_nothing_outside_isolation() {
    let script = format!(
        "{}; {}; {}",
        echo(&init("s1")),
        echo(&leak_line()),
        echo(&result("reviewed"))
    );
    let mut settings = Settings::default();
    settings.sandbox.isolated_leak = IsolatedLeak::Warn;
    let h = TestDriver::with_settings(TestAdapter::new(&script).isolating(), settings);
    let dirs = Dirs::new();
    let mut spec = dirs.spec("review");
    spec.isolated = true;
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;
    assert_eq!(end.reason, EndReason::Completed);
    let warned = notices(&events)
        .into_iter()
        .any(|notice| notice.contains("memory-keeper and lint-helper") && notice.contains("warn"));
    assert!(warned, "{:?}", notices(&events));

    let h = TestDriver::new(TestAdapter::new(&script));
    let dirs = Dirs::new();
    let (events, end) = collect(h.worker.start(dirs.spec("review")).await.unwrap()).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert!(
        !notices(&events)
            .into_iter()
            .any(|notice| notice.contains("memory-keeper"))
    );
}

fn report_call(id: &str, started: bool) -> String {
    event_line(&if started {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: "mcp__argonauts_status__argonauts_report".into(),
            input: serde_json::json!({ "status": "done" }),
            parent: None,
        }
    } else {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: None,
            parent: None,
        }
    })
}

/// Claude Code's own workflow runs in the background past the first turn. A report sent while
/// it runs is held back, the session waits for the workflow, and one that then ends without
/// reporting is asked once; the report it sends after that is the one kept.
#[tokio::test]
async fn a_session_in_its_own_workflow_reports_once_its_work_is_done() {
    let dirs = Dirs::new();
    let report = dirs.session.join("report.json");
    let report = report.display();
    let script = format!(
        "IFS= read -r first; {}; {}; echo '{{\"status\":\"blocked\"}}' > '{report}'; {}; {}; \
         {}; {}; {}; IFS= read -r asked; printf '%s\\n' \"$asked\" >> got-input; {}; \
         echo '{{\"status\":\"done\"}}' > '{report}'; {}; {}; \
         while IFS= read -r line; do :; done; : > input-closed",
        echo(&background(&["w1"], false)),
        echo(&report_call("early", true)),
        echo(&report_call("early", false)),
        echo(&result("Workflow launched in background.")),
        wait_for_go(),
        echo(&background(&[], true)),
        echo(&result("The workflow finished.")),
        echo(&report_call("final", true)),
        echo(&report_call("final", false)),
        echo(&result("Reported.")),
    );
    let h = TestDriver::new(TestAdapter::new(&script).with_send_now());
    let mut spec = dirs.spec("add due dates");
    spec.json_schema = Some(serde_json::json!({"type": "object"}));
    spec.cli_workflow.ultracode = true;
    spec.limits.inactivity_min = Some(0.1);
    let mut handle = h.worker.start(spec).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::Result { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!dirs.work.join("input-closed").exists());
    std::fs::write(dirs.work.join("go"), "").unwrap();
    let (events, end) = finish(seen, handle).await;

    assert!(dirs.read("got-input").contains("argonauts_report"));
    assert_eq!(end.structured, Some(serde_json::json!({"status": "done"})));
    let reports: Vec<String> = agent_events(&events)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ToolStarted { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reports, ["final"]);
    assert!(dirs.work.join("input-closed").exists());
}

/// What Claude Code writes on stderr as its spend cap stops a session's background agents.
const CAP_NOTE: &str = "Budget limit reached ($0.68 of $0.66); stopping background agents.";

/// Claude Code 2.1.301's lines for a workflow it starts and then stops, as its spend limit
/// stops one: the turn's result comes at once, it says on stderr that the cap stops the
/// workflow, and the stopped workflow is never told to the model, so no turn follows it.
fn stopped_workflow_lines() -> String {
    let stopped = [
        r#"{"type":"system","subtype":"task_updated","task_id":"w1","patch":{"status":"killed","end_time":1},"session_id":"cc-1"}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"w1","tool_use_id":"toolu_1","status":"stopped","summary":"Fix the rounding","session_id":"cc-1"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[],"session_id":"cc-1"}"#,
    ];
    let note = format!("{} >&2", echo(CAP_NOTE));
    [started_workflow_lines(), note, stopped.map(echo).join("; ")].join("; ")
}

/// Claude Code 2.1.301's lines for a turn that starts its own workflow: the turn's result comes
/// as soon as the workflow runs in the background.
fn started_workflow_lines() -> String {
    [
        r#"{"type":"system","subtype":"init","session_id":"cc-1","tools":["Read","Workflow","mcp__argonauts_status__argonauts_report"],"mcp_servers":[{"name":"argonauts_status","status":"connected"}],"model":"claude-haiku-5-5","claude_code_version":"2.1.301"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"local_workflow","description":"Fix the rounding"}],"session_id":"cc-1"}"#,
        r#"{"type":"system","subtype":"task_started","task_id":"w1","tool_use_id":"toolu_1","description":"Fix the rounding","task_type":"local_workflow","workflow_name":"fix-money-rounding","session_id":"cc-1"}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"The workflow is running in the background.","stop_reason":"end_turn","terminal_reason":"completed","num_turns":3,"total_cost_usd":0.4274356,"usage":{"input_tokens":10,"output_tokens":5},"session_id":"cc-1"}"#,
    ]
    .map(echo)
    .join("; ")
}

/// Stops a session once its first turn has its result, while the workflow that turn started
/// runs on in the background.
async fn stop_while_its_workflow_runs(
    control: SessionControl,
    own_workflow: bool,
) -> argonauts_core::runs::events::session::SessionEnd {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; while IFS= read -r line; do :; done",
        started_workflow_lines(),
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let mut spec = dirs.spec("fix the rounding");
    if own_workflow {
        spec.json_schema = Some(serde_json::json!({"type": "object"}));
        spec.cli_workflow.ultracode = true;
    }
    let mut handle = h.worker.start(spec).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::Result { .. }))
    })
    .await;
    handle.control.send(control).await.unwrap();
    finish(seen, handle).await.1
}

/// A turn that starts the CLI's own workflow has its result while the workflow runs on, so a
/// pause or a stop then cuts the work off: the session ends paused or stopped, to carry on
/// when resumed, never completed.
#[tokio::test]
async fn a_session_stopped_while_its_own_workflow_runs_ends_paused_or_stopped() {
    let paused = stop_while_its_workflow_runs(SessionControl::StopAfterTool, true).await;
    assert_eq!(paused.reason, EndReason::Paused);
    assert_eq!(paused.structured, None);
    let stopped = stop_while_its_workflow_runs(SessionControl::Stop, true).await;
    assert_eq!(stopped.reason, EndReason::Stopped);
}

/// A session that owes no report is cut off all the same while its CLI's workflow runs.
#[tokio::test]
async fn a_session_stopped_while_its_cli_workflow_runs_is_stopped() {
    let end = stop_while_its_workflow_runs(SessionControl::Stop, false).await;
    assert_eq!(end.reason, EndReason::Stopped);
}

/// Claude Code 2.1.301's lines for a command it ran in the background and stopped at its
/// deadline: the notification says stopped, yet the command is told to the model, in a turn of
/// Claude Code's own that comes after the runner took the task as untold.
fn stopped_command_lines() -> [String; 2] {
    let started = [
        r#"{"type":"system","subtype":"init","session_id":"cc-1","tools":["Read","Bash"],"mcp_servers":[],"model":"claude-haiku-5-5","claude_code_version":"2.1.301"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","task_type":"local_bash","description":"Run the tests"}],"session_id":"cc-1"}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"The tests run in the background.","num_turns":2,"total_cost_usd":0.21,"usage":{"input_tokens":10,"output_tokens":5},"session_id":"cc-1"}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"b1","status":"stopped","summary":"Run the tests","session_id":"cc-1"}"#,
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[],"session_id":"cc-1"}"#,
    ];
    let told = [
        r#"{"type":"system","subtype":"init","session_id":"cc-1","tools":["Read","Bash"],"mcp_servers":[],"model":"claude-haiku-5-5","claude_code_version":"2.1.301"}"#,
        r#"{"type":"assistant","message":{"model":"claude-haiku-5-5","id":"msg_2","type":"message","role":"assistant","content":[{"type":"text","text":"The test run was stopped."}],"usage":{"input_tokens":5,"output_tokens":7}},"parent_tool_use_id":null,"session_id":"cc-1"}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"The test run was stopped.","num_turns":1,"total_cost_usd":0.23,"usage":{"input_tokens":5,"output_tokens":7},"session_id":"cc-1"}"#,
    ];
    [started.map(echo).join("; "), told.map(echo).join("; ")]
}

/// A command Claude Code stopped at its deadline is told to the model after all. Its turn comes
/// after the input closed, and the session ends with that turn's words.
#[tokio::test]
async fn a_command_stopped_at_its_deadline_and_told_after_the_input_closed_completes() {
    let dirs = Dirs::new();
    let [started, told] = stopped_command_lines();
    let script = format!(
        "IFS= read -r first; {started}; while IFS= read -r line; do :; done; : > input-closed; \
         {told}"
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let (_, end) = collect(h.worker.start(dirs.spec("run the tests")).await.unwrap()).await;

    assert!(dirs.work.join("input-closed").exists());
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(
        end.result_text.as_deref(),
        Some("The test run was stopped.")
    );
}

/// A session that owes its report is asked for it once when the stopped command leaves, and
/// the told turn that comes before the answer neither asks again nor takes the report's place.
#[tokio::test]
async fn a_session_owing_its_report_is_asked_once_around_a_told_command() {
    let dirs = Dirs::new();
    let report = dirs.session.join("report.json");
    let report = report.display();
    let [started, told] = stopped_command_lines();
    let script = format!(
        "IFS= read -r first; {started}; IFS= read -r asked; printf '%s\\n' \"$asked\" >> got-input; \
         {told}; while IFS= read -r line; do printf '%s\\n' \"$line\" >> got-input; done; \
         : > input-closed; echo '{{\"status\":\"done\"}}' > '{report}'; {}",
        echo(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Reported.","num_turns":1,"total_cost_usd":0.25,"usage":{"input_tokens":5,"output_tokens":3},"session_id":"cc-1"}"#
        ),
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let mut spec = dirs.spec("fix the rounding");
    spec.json_schema = Some(serde_json::json!({"type": "object"}));
    spec.cli_workflow.ultracode = true;
    let (_, end) = collect(h.worker.start(spec).await.unwrap()).await;

    assert!(dirs.work.join("input-closed").exists());
    assert_eq!(
        dirs.read("got-input").matches("argonauts_report").count(),
        1
    );
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.structured, Some(serde_json::json!({"status": "done"})));
}

/// A session whose turn is done with nothing left in the background finished before the stop.
#[tokio::test]
async fn a_session_stopped_after_its_work_is_done_completed() {
    let dirs = Dirs::new();
    let script = format!("{}; exec sleep 30", echo(&result("Done.")));
    let h = TestDriver::new(TestAdapter::new(&script));
    let mut handle = h.worker.start(dirs.spec("fix the rounding")).await.unwrap();
    let seen = read_until(&mut handle.events, |e| {
        matches!(e, RunnerEvent::Agent(AgentEvent::Result { .. }))
    })
    .await;
    handle.control.send(SessionControl::Stop).await.unwrap();
    let (_, end) = finish(seen, handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(end.result_text.as_deref(), Some("Done."));
}

/// The result of a turn that Claude Code ended at its own spend cap. Its totals are the
/// session's whole, with what the agents of the workflow it stopped spent after the first
/// result.
const PAST_THE_CAP: &str = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"num_turns":1,"total_cost_usd":0.69,"usage":{"input_tokens":0,"output_tokens":0},"modelUsage":{"claude-haiku-5-5":{"inputTokens":24,"outputTokens":3100,"cacheReadInputTokens":1093000,"cacheCreationInputTokens":366000,"costUSD":0.69}},"session_id":"cc-1"}"#;

/// A session in its own workflow whose workflow Claude Code stopped is asked for its report at
/// once rather than waiting for a turn that never comes, and ends with its input closed. Claude
/// Code checks its cap before each request, so the ask costs nothing, and its result brings
/// all the session spent: the session ends at its spend limit with that.
#[tokio::test]
async fn a_session_whose_own_workflow_was_stopped_is_asked_for_its_report_and_ends() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; IFS= read -r asked; printf '%s\\n' \"$asked\" >> got-input; \
         {}; while IFS= read -r line; do :; done; : > input-closed",
        stopped_workflow_lines(),
        echo(PAST_THE_CAP),
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let mut spec = dirs.spec("fix the rounding");
    spec.json_schema = Some(serde_json::json!({"type": "object"}));
    spec.cli_workflow.ultracode = true;
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;

    assert!(dirs.read("got-input").contains("argonauts_report"));
    assert!(dirs.work.join("input-closed").exists());
    let results = agent_events(&events)
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::Result { .. }))
        .count();
    assert_eq!(results, 2);
    assert_eq!(
        end.reason,
        EndReason::LimitHit {
            dimension: LimitDimension::Spend
        }
    );
    // Claude Code's own total stands over the lesser figure its note on stderr gives.
    assert!((end.spend.cost_usd - 0.69).abs() < 1e-9, "{:?}", end.spend);
    assert!(!end.spend.cost_estimated, "{:?}", end.spend);
}

/// A session that owes no report closes its input as soon as the stopped workflow is gone.
#[tokio::test]
async fn a_session_whose_workflow_was_stopped_closes_its_input_at_once() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; while IFS= read -r line; do :; done; : > input-closed",
        stopped_workflow_lines(),
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let (_events, end) =
        collect(h.worker.start(dirs.spec("fix the rounding")).await.unwrap()).await;

    assert!(dirs.work.join("input-closed").exists());
    assert_eq!(end.reason, EndReason::Completed);
    assert_eq!(
        end.result_text.as_deref(),
        Some("The workflow is running in the background.")
    );
}

/// What a session's background work spent after its last result is in no result when Claude
/// Code's cap stops that work and the session ends with no turn after it. The session is
/// counted what Claude Code's note on stderr says, not the $0.43 of its last result, and the
/// last reading the run takes says so too.
#[tokio::test]
async fn a_session_whose_cap_stopped_its_background_work_counts_what_the_cap_says() {
    let dirs = Dirs::new();
    let script = format!(
        "IFS= read -r first; {}; while IFS= read -r line; do :; done; : > input-closed",
        stopped_workflow_lines(),
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let (events, end) = collect(h.worker.start(dirs.spec("fix the rounding")).await.unwrap()).await;

    assert_eq!(end.reason, EndReason::Completed);
    assert!((end.spend.cost_usd - 0.675).abs() < 1e-9, "{:?}", end.spend);
    assert!(end.spend.cost_estimated, "{:?}", end.spend);
    let last = session_events(&events)
        .into_iter()
        .rev()
        .find_map(|event| match event {
            SessionEvent::Limit(LimitEvent::Spend(status)) => Some(status.spend.cost_usd),
            _ => None,
        });
    assert!(
        last.is_some_and(|usd| (usd - 0.675).abs() < 1e-9),
        "{last:?}"
    );
}

/// A request billed past the session's limit stops the session at that request, before its
/// turn's result. This is Claude Code 2.1.301's first request of a run session: 361,887 tokens
/// of prompt, written to the cache for an hour, which a flat price puts at a tenth of its bill.
#[tokio::test]
async fn a_request_billed_past_the_limit_stops_the_session_at_that_request() {
    let dirs = Dirs::new();
    let init = r#"{"type":"system","subtype":"init","session_id":"cc-1","tools":["Read"],"mcp_servers":[],"model":"claude-haiku-5-5","claude_code_version":"2.1.301"}"#;
    let start = r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-haiku-5-5","id":"msg_1","type":"message","role":"assistant","content":[],"usage":{"input_tokens":2,"cache_creation_input_tokens":361885,"cache_read_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":361885},"output_tokens":2}}},"session_id":"cc-1"}"#;
    let script = format!(
        "IFS= read -r first; {}; {}; while IFS= read -r line; do :; done; : > input-closed",
        echo(init),
        echo(start),
    );
    let adapter = TestAdapter::new(&script)
        .with_send_now()
        .with_parser(|| ClaudeCodeAdapter.parser());
    let h = TestDriver::new(adapter);
    let mut spec = dirs.spec("plan the change");
    spec.limits.limit = Some(LimitAmount {
        unit: LimitUnit::Usd,
        amount: 0.2255,
    });
    let (events, end) = collect(h.worker.start(spec).await.unwrap()).await;

    assert_eq!(
        end.reason,
        EndReason::LimitHit {
            dimension: LimitDimension::Spend
        }
    );
    assert!(dirs.work.join("input-closed").exists());
    // The reading the run learns what such a session costs to open from.
    let first = session_events(&events)
        .into_iter()
        .find_map(|event| match event {
            SessionEvent::Limit(LimitEvent::Spend(status)) if status.spend.cost_usd > 0.0 => {
                Some(status.spend)
            }
            _ => None,
        });
    let first = first.expect("a spend reading");
    assert!((first.cost_usd - 0.361891).abs() < 1e-9, "{first:?}");
    assert!(first.cost_estimated);
}

/// Reads agent events as they are and, once the process has ended, asks for one more command,
/// whose output it reads as a message: the way OpenCode's export fills in a step its stream
/// left out.
struct AfterEndParser;

impl argonauts_core::agents::adapter::StreamParser for AfterEndParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        vec![serde_json::from_str(line).unwrap_or_else(|_| AgentEvent::unknown_line(line))]
    }
    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn after_end(
        &mut self,
        command: &argonauts_core::CommandSpec,
    ) -> Option<argonauts_core::CommandSpec> {
        let mut after = argonauts_core::CommandSpec::new("/bin/sh", &command.cwd)
            .args(["-c", "printf 'after the end in %s' \"${PWD##*/}\""]);
        after.env_set.clone_from(&command.env_set);
        Some(after)
    }
    fn read_after_end(&mut self, stdout: Option<&str>) -> Vec<AgentEvent> {
        vec![AgentEvent::Message {
            text: stdout.unwrap_or("nothing came").to_owned(),
            parent: None,
        }]
    }
}

/// What a parser asks to run once its process has ended runs where the session ran, and what it
/// prints is read before the session ends.
#[tokio::test]
async fn what_a_parser_asks_for_after_the_end_is_read_before_the_session_ends() {
    let dirs = Dirs::new();
    let adapter = TestAdapter::new(&echo(&result("done"))).with_parser(|| Box::new(AfterEndParser));
    let h = TestDriver::new(adapter);
    let handle = h.worker.start(dirs.spec("start")).await.unwrap();
    let (events, end) = collect(handle).await;
    assert_eq!(end.reason, EndReason::Completed);
    assert!(
        events
            .iter()
            .any(|event| is_agent_message(event, "after the end in work")),
        "{:?}",
        names(&events)
    );
}
