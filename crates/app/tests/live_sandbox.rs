//! Live: one real session per vendor CLI inside the OS sandbox, with network only through the
//! proxy, on each CLI's cheapest model. They spend a little quota, so they are ignored. Run one
//! by hand with `ARGONAUTS_HOME` at a scratch folder and `ARGONAUTS_LIVE_DIR` where the
//! throwaway repositories go. Your `HOME` stays, so each CLI finds its own login:
//!
//! ```sh
//! ARGONAUTS_HOME=/tmp/live/home ARGONAUTS_LIVE_DIR=/tmp/live cargo test -p argonauts-app \
//!     --test live_sandbox -- --ignored --nocapture claude_code
//! ```
//!
//! Each prints the shell commands the agent ran and whether they worked, the hosts the proxy
//! opened and refused, and the tokens spent, then checks that a shell command wrote `out.txt`.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_adapter_codex::CodexAdapter;
use argonauts_conductor::harness_worker::HarnessWorker;
use argonauts_conductor::sandbox::proxy::Proxy;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::worker::WorkerEvent;
use argonauts_core::worker::{Access, Worker};
use argonauts_core::{Adapter, AgentEvent, CardId, Paths, create_private_dir_all};
use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};
use argonauts_testkit::worker::{drain, session_spec};

const PROMPT: &str = "Run `ls` and `echo ok > out.txt`, then reply done.";

/// Each CLI's shell tool, as its stream names it. agy can also write a file with a tool of its
/// own, so `out.txt` alone doesn't show that a command ran.
const SHELL_TOOLS: &[&str] = &["Bash", "command_execution", "run_command"];

#[tokio::test]
#[ignore = "live vendor call; see the module docs"]
async fn claude_code() {
    live(Arc::new(ClaudeCodeAdapter::new()), "haiku").await;
}

#[tokio::test]
#[ignore = "live vendor call; see the module docs"]
async fn codex() {
    live(Arc::new(CodexAdapter::new()), "gpt-5.6-luna").await;
}

#[tokio::test]
#[ignore = "live vendor call; see the module docs"]
async fn antigravity_cli() {
    live(Arc::new(AntigravityCli::new()), "gemini-3.6-flash-low").await;
}

async fn live(adapter: Arc<dyn Adapter>, model: &str) {
    let proxy_log = capture_proxy_log();
    let vendor = adapter.id().to_owned();
    let home = Paths::new(required_dir("ARGONAUTS_HOME"));
    let repo =
        throwaway_repo(&required_dir("ARGONAUTS_LIVE_DIR").join(format!("sandbox-{vendor}")));
    let proxy = Proxy::start().unwrap();
    let mut settings = Settings::default();
    settings.sandbox.mode = SandboxChoice::Os;
    let worker = HarnessWorker::new(
        adapter,
        WorkerRef::new(&vendor, Some(model.to_owned())),
        vendor.clone(),
        Arc::new(settings),
        Arc::new(MemoryQuotaBoard::default()),
        Arc::new(MemoryHealthBoard::default()),
    )
    .with_home(home.clone())
    .with_proxy(proxy.clone());

    let mut spec = session_spec(&vendor, Access::Build, &repo);
    spec.worker = WorkerRef::new(&vendor, Some(model.to_owned()));
    spec.effort = Some("low".to_owned());
    spec.card = Some(CardId::new("live").unwrap());
    spec.goal_sha256 = Some("0".repeat(64));
    spec.session_dir = home.home().join(format!("state/live/{vendor}/s-1"));
    let _ = std::fs::remove_dir_all(&spec.session_dir);
    create_private_dir_all(&spec.session_dir).unwrap();
    spec.prompt = vec![InputPart {
        kind: InputPartKind::Prompt,
        label: "prompt".to_owned(),
        text: PROMPT.to_owned(),
        approx_tokens: 0,
        source: None,
    }];

    let handle = worker.start(spec).await.unwrap();
    let (events, end) = tokio::time::timeout(Duration::from_secs(600), drain(handle))
        .await
        .expect("the session never ended");

    let out = std::fs::read_to_string(repo.join("out.txt")).ok();
    let commands = shell_commands(&events);
    let opened = tunnels(&proxy_log, "opened");
    let refused: Vec<String> = proxy
        .refusals()
        .iter()
        .map(|refusal| format!("{}:{} ({:?})", refusal.host, refusal.port, refusal.why))
        .collect();
    println!("vendor: {vendor} ({model}, low effort)");
    println!("ended: {:?}, exit {:?}", end.reason, end.exit_code);
    println!("out.txt: {out:?}");
    println!("reply: {:?}", end.result_text.or(end.last_message));
    println!("tool calls: {}", end.tool_calls);
    println!("shell commands (worked?): {commands:?}");
    println!("proxy opened: {opened:?}");
    println!("proxy refused: {refused:?}");
    println!(
        "tokens: {} in ({} cached), {} out, ${:.4}",
        end.spend.input_tokens,
        end.spend.cached_tokens,
        end.spend.output_tokens,
        end.spend.cost_usd
    );
    println!("raw log: {:?}", end.raw_log);
    assert!(
        commands.iter().any(|(_, worked)| *worked),
        "no shell command worked"
    );
    assert_eq!(
        out.as_deref(),
        Some("ok\n"),
        "out.txt isn't what `echo` wrote"
    );
}

/// Each shell command the agent started, with whether it finished without an error.
fn shell_commands(events: &[WorkerEvent]) -> Vec<(String, bool)> {
    let mut started = Vec::new();
    let mut finished = Vec::new();
    for event in events {
        match event {
            WorkerEvent::Agent(AgentEvent::ToolStarted {
                id, name, input, ..
            }) if SHELL_TOOLS.contains(&name.as_str()) => {
                started.push((id.clone(), input.to_string()));
            }
            WorkerEvent::Agent(AgentEvent::ToolFinished { id, ok, .. }) => {
                finished.push((id.clone(), *ok));
            }
            _ => {}
        }
    }
    started
        .into_iter()
        .map(|(id, input)| {
            let worked = finished.iter().any(|(done, ok)| *done == id && *ok);
            (input, worked)
        })
        .collect()
}

fn required_dir(name: &str) -> PathBuf {
    let dir = std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| panic!("set {name} to an absolute folder; see the module docs"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// A fresh repository with one small file and one commit.
fn throwaway_repo(dir: &Path) -> PathBuf {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("README.md"), "A throwaway repository.\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "README.md"],
        &[
            "-c",
            "user.name=live",
            "-c",
            "user.email=live@local",
            "commit",
            "-qm",
            "start",
        ],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    dir.canonicalize().unwrap()
}

/// The proxy's log lines, which name only hosts and ports.
fn capture_proxy_log() -> Arc<Mutex<Vec<u8>>> {
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let log = Arc::new(Mutex::new(Vec::new()));
        let writer = SharedLog(log.clone());
        tracing_subscriber::fmt()
            .with_env_filter("argonauts_conductor::sandbox::proxy=debug")
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .init();
        log
    })
    .clone()
}

/// `host:port` of each tunnel whose log line says `what`, in order, without repeats.
fn tunnels(log: &Mutex<Vec<u8>>, what: &str) -> Vec<String> {
    let text =
        String::from_utf8_lossy(&log.lock().unwrap_or_else(PoisonError::into_inner)).into_owned();
    let mut found = Vec::new();
    for line in text.lines().filter(|line| line.contains(what)) {
        let field = |name: &str| {
            line.split_whitespace()
                .find_map(|part| part.strip_prefix(&format!("{name}=")))
                .unwrap_or("?")
                .to_owned()
        };
        let tunnel = format!("{}:{}", field("host"), field("port"));
        if !found.contains(&tunnel) {
            found.push(tunnel);
        }
    }
    found
}

#[derive(Clone)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl Write for SharedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
