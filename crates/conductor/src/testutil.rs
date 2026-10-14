//! Test helpers for a scripted fake adapter, temporary git repositories, and log inspection.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use argonauts_core::{
    Adapter, AgentEvent, Capabilities, CommandSpec, EventLog, RunEvent, RunEventKind, RunId,
    SessionRequest, StopPlan, StreamParser, extract_version,
};
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;

use crate::recorder::Recorder;

/// `/bin/sh -c <script> sh`, so extra args become `$1`, `$2`, ...
pub(crate) fn sh_spec(dir: &Path, script: &str) -> CommandSpec {
    CommandSpec::new("/bin/sh", dir).args(["-c", script, "sh"])
}

/// A fake agent: a shell script that prints [`AgentEvent`]s as JSON lines, with the prompt as `$1`
/// and any session id to resume as `$2`. Like the real adapters, it rejects a flag-like model.
pub(crate) struct ScriptAdapter {
    script: String,
    program: String,
    stop_plan: StopPlan,
    stdin_wrap_up: bool,
    capabilities: Capabilities,
}

impl ScriptAdapter {
    pub(crate) fn new(script: &str) -> Self {
        Self {
            script: script.to_owned(),
            program: "/bin/sh".to_owned(),
            stop_plan: StopPlan::stepped(Duration::from_secs(1), Duration::from_secs(1)),
            stdin_wrap_up: false,
            // The scripts put dollar costs in their usage events.
            capabilities: Capabilities {
                reports_cost: true,
                ..Capabilities::default()
            },
        }
    }

    pub(crate) fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub(crate) fn with_stop_plan(mut self, plan: StopPlan) -> Self {
        self.stop_plan = plan;
        self
    }

    pub(crate) fn with_stdin_wrap_up(mut self) -> Self {
        self.stdin_wrap_up = true;
        self
    }

    pub(crate) fn with_program(mut self, program: &str) -> Self {
        self.program = program.to_owned();
        self
    }
}

impl Adapter for ScriptAdapter {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn display_name(&self) -> &'static str {
        "Fake Agent"
    }
    fn binary(&self) -> &'static str {
        "sh"
    }
    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        if let Some(model) = req.model.as_deref() {
            anyhow::ensure!(!model.starts_with('-'), "the model can't start with '-'");
        }
        let mut spec = CommandSpec::new(&self.program, &req.workdir).args([
            "-c",
            &self.script,
            "sh",
            &req.prompt,
            req.resume_session.as_deref().unwrap_or(""),
        ]);
        if self.stdin_wrap_up {
            spec.stdin = Some(String::new());
            spec.keep_stdin_open = true;
        }
        Ok(spec)
    }
    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(JsonEventParser)
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
    fn wrap_up_input(&self, message: &str) -> Option<String> {
        self.stdin_wrap_up.then(|| format!("{message}\n"))
    }
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }
}

/// Reads each stdout line as a serialized [`AgentEvent`], or a JSON array of them, the way a
/// real CLI's final line carries both usage and the result.
struct JsonEventParser;

impl StreamParser for JsonEventParser {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        if let Ok(events) = serde_json::from_str::<Vec<AgentEvent>>(line) {
            return events;
        }
        vec![serde_json::from_str(line).unwrap_or_else(|_| AgentEvent::unknown_line(line))]
    }
    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        Vec::new()
    }
}

pub(crate) fn usage_line(input: u64, output: u64, cost_usd: Option<f64>) -> String {
    serde_json::to_string(&AgentEvent::Usage {
        input_tokens: input,
        output_tokens: output,
        cached_tokens: 0,
        cost_usd,
        cumulative: true,
    })
    .unwrap()
}

pub(crate) fn final_line(usage: &str, result: &str) -> String {
    format!("[{usage},{result}]")
}

pub(crate) fn open_recorder(dir: &Path) -> Recorder {
    let log = EventLog::open(dir.join("events.jsonl")).unwrap();
    Recorder::new(log, RunId::new("run-test").unwrap(), None)
}

/// Each event's kind, with agent events as `agent:<type>`.
pub(crate) fn events_of(events: &[RunEvent]) -> Vec<String> {
    events.iter().map(|e| kind_name(&e.kind)).collect()
}

pub(crate) fn kind_name(kind: &RunEventKind) -> String {
    let name = match kind {
        RunEventKind::Agent(event) => return format!("agent:{}", event.type_name()),
        RunEventKind::RunStarted(_) => "run_started",
        RunEventKind::Phase(_) => "phase",
        RunEventKind::SessionStarted(_) => "session_started",
        RunEventKind::SessionEnded(_) => "session_ended",
        RunEventKind::CheckResult(_) => "check_result",
        RunEventKind::Commit(_) => "commit",
        RunEventKind::Report(_) => "report",
        RunEventKind::Approval(_) => "approval",
        RunEventKind::Decision(_) => "decision",
        RunEventKind::Spend(_) => "limit",
        RunEventKind::Note(_) => "note",
        RunEventKind::RunFinished(_) => "run_finished",
    };
    name.to_owned()
}

pub(crate) async fn wait_until_gone(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while kill(Pid::from_raw(pid), None) != Err(Errno::ESRCH) && !is_zombie(pid) {
        assert!(Instant::now() < deadline, "process {pid} is still running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Containers without an init process never reap orphans, so a dead one lingers as a zombie.
fn is_zombie(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let state = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .next()?
                .to_owned();
            Some(state == "Z")
        })
        .unwrap_or(false)
}

pub(crate) fn git_test_env() -> Vec<(String, String)> {
    vec![
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]
}

/// A temporary repository on branch `main` with one commit containing `README.md`.
pub(crate) struct TestRepo {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) root: PathBuf,
}

impl TestRepo {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = Self { dir, root };
        repo.git(&["init", "--quiet", "-b", "main"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.write("README.md", "# Test\n");
        repo.git(&["add", "README.md"]);
        repo.git(&["commit", "--quiet", "-m", "init"]);
        repo
    }

    pub(crate) fn git(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .envs(git_test_env())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    pub(crate) fn write(&self, rel: &str, contents: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
}
