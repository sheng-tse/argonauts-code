//! Shared by the policy scenarios: a trusted repo with a global config, waits on the run's
//! journal, and an MCP client driving `argonauts mcp serve` over the child's stdio, playing
//! OpenClaw (a 2025-11-25 handshake) or Hermes (2026-07-28, no handshake).

#![allow(dead_code)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind};
use argonauts_core::events::checkpoint::{
    CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
};
use argonauts_core::events::run::{RunLifecycle, RunStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::ChoiceId;
use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::{Invocation, Key, Pty, argonauts_bin};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
    ProtocolVersion,
};
use rmcp::service::RunningService;
use rmcp::{ClientLifecycleMode, ClientServiceExt, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::process::{Child, Command};

/// The idle prompt's hint line.
pub const IDLE: &str = "for shortcuts";
/// What every policy scenario asks for. The fake owners match on `hello.txt`.
pub const TASK: &str = "Create hello.txt containing hi";
/// A solo card from start to its merge checkpoint, with fakes.
pub const RUN_WAIT: Duration = Duration::from_secs(90);
/// Merges decided by your rules alone, loose enough for a one-line change with no tests, and
/// everything else the rules ask before auto merges: a gate that checks the card's file, so
/// the gates and the final check pass on real results, and a review by another company than
/// the owner's, here the fake Codex. Codex names no model it ran, so the reviewer names one.
pub const AUTO_MERGE_RULES: &str = concat!(
    "[approvals.merge]\nmode = \"auto\"\ndecide_with = \"rules\"\n\n",
    "[approvals.merge.rules]\nmin_changed_line_coverage = 0.0\nmax_combined_diff_lines = 400\n\n",
    "[gates]\nrun = [{ name = \"hello\", command = [\"test\", \"-f\", \"hello.txt\"] }]\n\n",
    "[solo]\nreview = true\n\n[roles.reviewer]\nworkers = [\"codex/gpt-5.6-luna\"]\n",
);
/// Merges wait for you, for the scenarios about a merge checkpoint that does. Merging is
/// automatic by default.
pub const SUPERVISED_MERGE: &str = "[approvals.merge]\nmode = \"supervised\"\n";
/// The gate alone, for merges the approver decides once the hard limits pass.
pub const HELLO_GATE: &str =
    "[gates]\nrun = [{ name = \"hello\", command = [\"test\", \"-f\", \"hello.txt\"] }]\n";

pub fn scenario(name: &str) -> anyhow::Result<Scenario> {
    Scenario::new(&format!("policy/{name}"))
}

/// A scenario in a repo you set up with `argonauts init` and trusted, then `config` as
/// `~/.argonauts/config.toml`. The config goes in after init, since init writes that file
/// with its own answers. It's the global layer, so safety keys there need no trust.
pub fn trusted_scenario(name: &str, config: &str) -> anyhow::Result<Scenario> {
    let scenario = scenario(name)?;
    trust_repo(&scenario)?;
    write_global_config(&scenario, config)?;
    Ok(scenario)
}

pub fn write_global_config(scenario: &Scenario, text: &str) -> anyhow::Result<()> {
    let path = scenario.repo.global_config();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

/// Runs `argonauts init` with each step's recommended answer, except that it makes
/// `.argonauts/` now and trusts the repo's settings files if it asks: a repo set up so
/// `argonauts mcp serve` takes it.
pub fn trust_repo(scenario: &Scenario) -> anyhow::Result<()> {
    const PICKS: [&str; 2] = ["Now", "Trust them"];
    let mut pty = scenario.launch(&["init"])?;
    let deadline = Instant::now() + WAIT;
    let mut answered: Vec<ChoiceId> = Vec::new();
    loop {
        let open = open_setup_steps(scenario)?;
        if let Some(step) = open.iter().find(|step| !answered.contains(&step.id)) {
            let pick = step
                .options
                .iter()
                .position(|option| PICKS.contains(&option.label.as_str()));
            match pick.and_then(|index| char::from_digit(index as u32 + 1, 10)) {
                Some(number) => pty.press(Key::Char(number))?,
                None => pty.press(Key::Enter)?,
            }
            answered.push(step.id.clone());
        } else if !answered.is_empty()
            && open.is_empty()
            && scenario.repo.root().join(".argonauts").is_dir()
            && pty.screen().text().contains("Saved to")
        {
            break;
        }
        if Instant::now() >= deadline {
            bail!(
                "argonauts init never finished setting up the repo; the screen was:\n{}",
                pty.screen().text()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    pty.kill();
    Ok(())
}

/// The setup steps the app has open, from its trace.
fn open_setup_steps(scenario: &Scenario) -> anyhow::Result<Vec<AppChoice>> {
    let mut open: Vec<AppChoice> = Vec::new();
    for event in scenario.trace.events()? {
        match event {
            AppEvent::Choice { choice }
                if matches!(choice.kind, ChoiceKind::TermsNote | ChoiceKind::Init) =>
            {
                open.push(choice);
            }
            AppEvent::ChoiceClosed { id, .. } => open.retain(|choice| choice.id != id),
            _ => {}
        }
    }
    Ok(open)
}

/// Each repo's `trusted.json` under the test's Argonauts home.
pub fn trusted_files(scenario: &Scenario) -> Vec<PathBuf> {
    let state = scenario.repo.home().join("state");
    fs::read_dir(state)
        .map(|repos| {
            repos
                .filter_map(Result::ok)
                .map(|repo| repo.path().join("trusted.json"))
                .filter(|path| path.is_file())
                .collect()
        })
        .unwrap_or_default()
}

pub fn launch_ready(scenario: &Scenario, args: &[&str]) -> anyhow::Result<Pty> {
    let pty = scenario.launch(args)?;
    pty.wait_for(IDLE, WAIT)?;
    Ok(pty)
}

pub fn start_solo(pty: &mut Pty) -> anyhow::Result<()> {
    pty.send(&format!("/solo {TASK}\r"))
}

/// Polls the newest run's journal until `done` holds.
pub fn wait_for_events(
    scenario: &Scenario,
    what: &str,
    timeout: Duration,
    done: impl Fn(&[RunEvent]) -> bool,
) -> anyhow::Result<Vec<RunEvent>> {
    let deadline = Instant::now() + timeout;
    loop {
        // No run yet reads as no events.
        let events = scenario.latest_run_events().unwrap_or_default();
        if done(&events) {
            return Ok(events);
        }
        if Instant::now() >= deadline {
            let kinds: Vec<String> = events.iter().map(describe).collect();
            bail!(
                "timed out waiting for {what}; the journal has:\n{}",
                kinds.join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn describe(event: &RunEvent) -> String {
    let kind = serde_json::to_value(&event.kind).unwrap_or(Value::Null);
    format!("{} {} {}", event.seq, kind["type"], kind["data"]["event"])
}

pub fn run_started(events: &[RunEvent]) -> Option<&RunStarted> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => Some(started.as_ref()),
        _ => None,
    })
}

pub fn opened_of_kind(events: &[RunEvent], kind: CheckpointKind) -> Vec<&CheckpointOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
                Some(opened.as_ref())
            }
            _ => None,
        })
        .collect()
}

pub fn merge_opened(events: &[RunEvent]) -> bool {
    !opened_of_kind(events, CheckpointKind::Merge).is_empty()
}

pub fn decisions(events: &[RunEvent]) -> Vec<&CheckpointDecided> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => Some(decided),
            _ => None,
        })
        .collect()
}

pub fn merge_decided(events: &[RunEvent]) -> Option<&CheckpointDecided> {
    let merges: Vec<_> = opened_of_kind(events, CheckpointKind::Merge)
        .into_iter()
        .map(|opened| &opened.id)
        .collect();
    decisions(events)
        .into_iter()
        .find(|decided| merges.contains(&&decided.id))
}

pub fn run_finished(events: &[RunEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::Finished { .. })
        )
    })
}

/// The fake vendor's sessions of `program`: its calls, less the version, help, login, model
/// list, and quota probes the app makes.
pub fn invocations_of(scenario: &Scenario, program: &str) -> anyhow::Result<Vec<Invocation>> {
    let probe = |args: &[String]| {
        args.iter()
            .any(|arg| ["--version", "--help", "models"].contains(&arg.as_str()))
            || args
                .first()
                .is_some_and(|arg| ["login", "auth", "app-server"].contains(&arg.as_str()))
    };
    Ok(scenario
        .vendors
        .invocations()?
        .into_iter()
        .filter(|call| call.argv0 == program && !probe(&call.args))
        .collect())
}

/// Every file under `dir` named `name`, at any depth.
pub fn files_named(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|file| file == name) {
                found.push(path);
            }
        }
    }
    found
}

pub fn finish_screen(scenario: &Scenario, pty: &Pty, name: &str) -> anyhow::Result<()> {
    scenario.shot(pty, name)
}

pub fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting a tokio runtime")
}

/// How a simulated front end opens its MCP session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontEnd {
    /// OpenClaw's MCP library: a 2025-11-25 handshake.
    OpenClaw,
    /// Hermes: 2026-07-28, no handshake, its version and name on every request.
    Hermes,
}

impl FrontEnd {
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
        }
    }
}

/// `argonauts mcp serve --repo <root>` with the scenario's homes and fakes, and a client on
/// its stdio.
pub struct McpServer {
    pub client: RunningService<RoleClient, ClientConfig>,
    child: Child,
}

impl McpServer {
    pub async fn start(scenario: &Scenario, front_end: FrontEnd) -> anyhow::Result<Self> {
        let binary = argonauts_bin().context("finding the argonauts binary")?;
        let stderr_path = scenario.repo.scratch().join("mcp-serve.stderr.txt");
        let stderr = File::create(&stderr_path)
            .with_context(|| format!("creating {}", stderr_path.display()))?;
        let mut child = Command::new(binary)
            .args(["mcp", "serve", "--repo"])
            .arg(scenario.repo.root())
            // A host can't always set a working directory, which is what --repo is for.
            .current_dir(scenario.repo.scratch())
            .envs(scenario.envs())
            .env_remove("ARGONAUTS_SESSION")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .context("starting argonauts mcp serve")?;
        let stdout = child.stdout.take().context("the server's stdout")?;
        let stdin = child.stdin.take().context("the server's stdin")?;
        let config = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(front_end.name(), "1.0"),
        );
        let connected = match front_end {
            FrontEnd::OpenClaw => {
                config
                    .with_protocol_version(ProtocolVersion::V_2025_11_25)
                    .serve((stdout, stdin))
                    .await
            }
            FrontEnd::Hermes => {
                config
                    .serve_with_lifecycle(
                        (stdout, stdin),
                        ClientLifecycleMode::Discover {
                            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                        },
                    )
                    .await
            }
        };
        let client = connected.with_context(|| {
            format!(
                "connecting to argonauts mcp serve; its stderr:\n{}",
                fs::read_to_string(&stderr_path).unwrap_or_default()
            )
        })?;
        Ok(Self { client, child })
    }

    pub async fn tool_names(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .client
            .list_all_tools()
            .await?
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect())
    }

    pub async fn call(&self, name: &str, arguments: Value) -> anyhow::Result<CallToolResult> {
        let arguments = match arguments {
            Value::Object(arguments) => arguments,
            _ => serde_json::Map::new(),
        };
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        Ok(self.client.call_tool(params).await?)
    }

    /// Calls a tool that must succeed, and returns its structured result.
    pub async fn call_ok(&self, name: &str, arguments: Value) -> anyhow::Result<Value> {
        let result = self.call(name, arguments).await?;
        if failed(&result) {
            bail!("{name} failed: {}", text(&result));
        }
        Ok(structured(&result))
    }

    /// Long-polls `argonauts_run_events` from the start until `done` holds for what came.
    pub async fn events_until(
        &self,
        run: &str,
        timeout: Duration,
        done: impl Fn(&[Value]) -> bool,
    ) -> anyhow::Result<Vec<Value>> {
        let deadline = Instant::now() + timeout;
        let mut since = 0;
        let mut events = Vec::new();
        loop {
            let fields = self
                .call_ok(
                    "argonauts_run_events",
                    json!({ "run": run, "since": since, "wait_ms": 5_000 }),
                )
                .await?;
            events.extend(fields["events"].as_array().cloned().unwrap_or_default());
            since = fields["next_since"].as_u64().unwrap_or(since);
            if done(&events) {
                return Ok(events);
            }
            if Instant::now() >= deadline {
                bail!("timed out polling {run}; the events were:\n{events:#?}");
            }
        }
    }

    pub async fn close(mut self) -> anyhow::Result<()> {
        self.client.cancel().await?;
        self.child.kill().await.ok();
        Ok(())
    }
}

pub fn failed(result: &CallToolResult) -> bool {
    result.is_error == Some(true)
}

pub fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn structured(result: &CallToolResult) -> Value {
    result.structured_content.clone().unwrap_or(Value::Null)
}

/// Whether the polled events include a merge checkpoint that waits for you.
pub fn merge_waits(events: &[Value]) -> bool {
    events.iter().any(|event| {
        event["type"] == json!("checkpoint")
            && event["event"] == json!("opened")
            && event["checkpoint_kind"] == json!("merge")
            && event["needs_you"] == json!(true)
    })
}

/// The one global config every policy scenario starts from: a solo crew of the fake Claude
/// Code, with `extra` appended.
pub fn solo_config(extra: &str) -> String {
    format!("preset = \"solo\"\ndefault_agent = \"claude-code\"\n\n{extra}")
}
