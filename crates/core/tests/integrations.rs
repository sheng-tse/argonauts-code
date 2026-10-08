//! The agent integration contracts: ACP profiles and harness manifests, session drivers, front
//! ends over MCP, context fill, and the settings that go with them.

use std::sync::Arc;

use argonauts_core::adapter::{Adapter, StopPlan, StreamParser, Transport};
use argonauts_core::api::StartRun;
use argonauts_core::driver::{
    DriverEnd, DriverIo, DriverStart, DriverStop, McpServerSpec, PermissionScope, RawFrame,
    SessionDriver,
};
use argonauts_core::events::RunLifecycle;
use argonauts_core::events::run::Party;
use argonauts_core::future::BoxFuture;
use argonauts_core::layout::{session_files, user_files};
use argonauts_core::registry::acp::{
    AcpModes, AcpProfile, EffortRoute, ModelRoute, QuotaRoute, ReportRoute, ResumeRoute,
    SteerRoute, UsageSource,
};
use argonauts_core::registry::harness::{HomeKeep, OutputKind, PromptChannel};
use argonauts_core::registry::{HarnessManifest, ModelEntry, RegistryFile};
use argonauts_core::session::SessionRequest;
use argonauts_core::settings::agents::{Share, StartRuns};
use argonauts_core::settings::approvals::Presence;
use argonauts_core::settings::ui::ChatTrigger;
use argonauts_core::settings::{GLOBAL_ONLY_KEYS, SAFETY_KEYS, Settings, Span};
use argonauts_core::{AgentEvent, CommandSpec, ErrorCategory};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Parses `value` as `T`, writes it back, and checks nothing was lost or added.
fn round_trip<T: Serialize + DeserializeOwned + std::fmt::Debug + PartialEq>(value: Value) -> T {
    let parsed: T =
        serde_json::from_value(value.clone()).unwrap_or_else(|err| panic!("{err}\n{value:#}"));
    let written = serde_json::to_value(&parsed).unwrap();
    assert_eq!(written, value, "the JSON changed on the way through");
    let again: T = serde_json::from_value(written).unwrap();
    assert_eq!(again, parsed);
    parsed
}

fn schema_errors(schema: Value, value: &Value) -> Vec<String> {
    let validator = jsonschema::validator_for(&schema).unwrap();
    validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect()
}

/// Parses a `models.toml`, checks it against the registry schema, and round-trips it through
/// JSON and TOML.
fn registry(text: &str) -> RegistryFile {
    let registry: RegistryFile = toml::from_str(text).unwrap_or_else(|err| panic!("{err}"));
    let value = serde_json::to_value(&registry).unwrap();
    let errors = schema_errors(schemars::schema_for!(RegistryFile).to_value(), &value);
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(
        serde_json::from_value::<RegistryFile>(value).unwrap(),
        registry
    );
    let written = toml::to_string(&registry).unwrap();
    assert_eq!(toml::from_str::<RegistryFile>(&written).unwrap(), registry);
    registry
}

// `errors` and `interactive_resume` belong to the harness, so they sit above `[harness.acp]`.
const HERMES: &str = r#"
[[harness]]
id = "hermes"
display_name = "Hermes Agent"
binary = "/Users/you/.local/bin/hermes"
version_args = ["--version"]
tested_versions = ["0.21.5"]
family = "from-model"
transport = "acp"
read_only_args = ["acp"]
build_args = ["acp"]
env_set = { HERMES_ACP_SKIP_CONFIGURED_MCP = "1" }
home = { env = "HERMES_HOME", template = "hermes", keep = "card" }
errors = [{ pattern = "credentials or agent init failed", category = "auth" }]
interactive_resume = []

[harness.acp]
modes = { read_only = "default", build = "default", spike = "default" }
model = "legacy-set-model"
effort = "none"
report = "mcp-tool"
resume = "resume"
steer = "input"
usage = ["end-turn", "usage-update"]
quota = "none"
"#;

#[test]
fn the_hermes_profile_parses_validates_and_round_trips() {
    let file = registry(HERMES);
    let hermes = &file.harnesses[0];
    assert_eq!(hermes.transport, Transport::Acp);
    assert_eq!(hermes.family, "from-model");
    assert_eq!(hermes.env_set["HERMES_ACP_SKIP_CONFIGURED_MCP"], "1");
    let home = hermes.home.as_ref().unwrap();
    assert_eq!(
        (home.env.as_str(), home.keep),
        ("HERMES_HOME", HomeKeep::Card)
    );
    assert_eq!(hermes.errors[0].category, ErrorCategory::Auth);
    assert!(!hermes.errors[0].retryable);
    assert!(hermes.interactive_resume.is_empty());
    let default_mode = Some("default".to_owned());
    assert_eq!(
        hermes.acp,
        Some(AcpProfile {
            modes: AcpModes {
                read_only: default_mode.clone(),
                build: default_mode.clone(),
                spike: default_mode,
            },
            model: ModelRoute::LegacySetModel,
            effort: EffortRoute::None,
            report: ReportRoute::McpTool,
            resume: ResumeRoute::Resume,
            steer: SteerRoute::Input,
            usage: vec![UsageSource::EndTurn, UsageSource::UsageUpdate],
            quota: QuotaRoute::None,
            quota_probe: Vec::new(),
            mcp_servers: true,
        })
    );
}

#[test]
fn the_parser_and_final_json_manifests_parse_validate_and_round_trip() {
    let file = registry(
        r#"
[[harness]]
id = "grok-build"
display_name = "Grok Build"
binary = "/usr/local/bin/grok"
version_args = ["--version"]
tested_versions = ["1.0.44"]
family = "grok"
prompt = "argument"
output = "parser"
parser = "claude-code"
read_only_args = ["-p", "{prompt}", "--output-format", "streaming-messages-json",
                  "--allowedTools", "Read,Glob,Grep", "--model", "{model}"]
build_args = ["-p", "{prompt}", "--output-format", "streaming-messages-json", "--model", "{model}"]
resume_args = ["-r", "{session}"]
env_set = { GROK_DISABLE_AUTOUPDATER = "1" }

[[harness]]
id = "openclaw-exec"
display_name = "OpenClaw one-shot"
binary = "/opt/homebrew/bin/openclaw"
version_args = ["--version"]
tested_versions = []
family = "from-model"
prompt = "stdin"
output = "final-json"
read_only_args = ["agent", "exec", "--message-file", "-", "--cwd", "{workdir}",
                  "--config", "{home}/read-only.json5", "--model", "{model}", "--json"]
build_args = ["agent", "exec", "--message-file", "-", "--cwd", "{workdir}",
              "--config", "{home}/build.json5", "--model", "{model}", "--json"]
env_set = { OPENCLAW_NO_AUTO_UPDATE = "1" }
home = { env = "OPENCLAW_HOME", template = "openclaw-exec", keep = "session" }
exit_codes = { "2" = "timeout" }

[harness.final_json]
text = "/final"
status = "/status"
ok_status = ["ok"]
session_id = "/sessionId"
model = "/model"
input_tokens = "/usage/input"
output_tokens = "/usage/output"
cost_usd = "/costUsd"
error_kind = "/error/kind"
error_message = "/error/message"
"#,
    );
    let [grok, openclaw] = &file.harnesses[..] else {
        panic!("expected two CLIs");
    };
    assert_eq!(grok.transport, Transport::Lines);
    assert_eq!(grok.prompt, PromptChannel::Argument);
    assert_eq!(grok.output, OutputKind::Parser);
    assert_eq!(grok.parser.as_deref(), Some("claude-code"));
    assert_eq!(grok.resume_args, ["-r", "{session}"]);
    assert!(grok.acp.is_none() && grok.final_json.is_none());

    assert_eq!(openclaw.prompt, PromptChannel::Stdin);
    assert_eq!(openclaw.output, OutputKind::FinalJson);
    assert_eq!(openclaw.exit_codes["2"], ErrorCategory::Timeout);
    assert_eq!(openclaw.home.as_ref().unwrap().keep, HomeKeep::Session);
    let pointers = openclaw.final_json.as_ref().unwrap();
    assert_eq!(pointers.ok_status, ["ok"]);
    assert_eq!(pointers.cached_tokens, None);
    assert_eq!(pointers.error_message.as_deref(), Some("/error/message"));
}

#[test]
fn a_minimal_harness_entry_is_the_default_manifest_with_its_required_keys() {
    let file = registry(
        r#"
[[harness]]
id = "my-agent"
display_name = "My Agent"
binary = "/usr/local/bin/my-agent"
read_only_args = ["--read-only"]
build_args = []
family = "mine"
"#,
    );
    assert_eq!(
        file.harnesses[0],
        HarnessManifest {
            id: "my-agent".into(),
            display_name: "My Agent".into(),
            binary: "/usr/local/bin/my-agent".into(),
            read_only_args: vec!["--read-only".into()],
            family: "mine".into(),
            ..HarnessManifest::default()
        }
    );
    let default = HarnessManifest::default();
    assert_eq!(default.transport, Transport::Lines);
    assert_eq!(default.prompt, PromptChannel::Stdin);
    assert_eq!(default.output, OutputKind::ArgonautsJsonl);
    assert!(default.acp.is_none() && default.home.is_none() && default.stop.is_none());
    assert!(default.errors.is_empty() && default.interactive_resume.is_empty());
}

#[test]
fn the_default_acp_profile_claims_nothing_unproven() {
    let profile: AcpProfile = round_trip(json!({
        "modes": {"read_only": null, "build": null, "spike": null},
        "model": "config-option", "effort": "none", "report": "mcp-tool", "resume": "none",
        "steer": "cancel-and-prompt", "usage": ["usage-update"], "quota": "none",
        "quota_probe": [], "mcp_servers": true}));
    assert_eq!(profile, AcpProfile::default());
    assert_eq!(AcpModes::default().build, None);
    // An empty table is the default profile, key by key.
    let file = registry(
        r#"
[[harness]]
id = "acp-agent"
display_name = "ACP Agent"
binary = "/usr/local/bin/acp-agent"
read_only_args = ["acp"]
build_args = ["acp"]
family = "from-model"
transport = "acp"
[harness.acp]
"#,
    );
    assert_eq!(file.harnesses[0].acp, Some(AcpProfile::default()));
}

#[test]
fn harness_keys_inside_the_acp_table_are_refused() {
    let entry = |acp: &str| {
        format!(
            "[[harness]]\nid = \"acp-agent\"\ndisplay_name = \"ACP Agent\"\n\
             binary = \"/usr/local/bin/acp-agent\"\nread_only_args = [\"acp\"]\n\
             build_args = [\"acp\"]\nfamily = \"from-model\"\ntransport = \"acp\"\n\
             [harness.acp]\n{acp}\n"
        )
    };
    for misplaced in [
        r#"errors = [{ pattern = "init failed", category = "auth" }]"#,
        "interactive_resume = []",
        r#"modes = { biuld = "default" }"#,
    ] {
        let err = toml::from_str::<RegistryFile>(&entry(misplaced)).unwrap_err();
        assert!(
            err.to_string().contains("unknown field"),
            "{misplaced}: {err}"
        );
    }
    assert!(toml::from_str::<RegistryFile>(&entry(r#"model = "launch-arg""#)).is_ok());
}

#[test]
fn quota_routes_take_three_forms_and_refuse_the_rest() {
    let route = |text: &str| serde_json::from_value::<QuotaRoute>(json!(text));
    assert_eq!(route("none").unwrap(), QuotaRoute::None);
    assert_eq!(route("probe").unwrap(), QuotaRoute::Probe);
    assert_eq!(
        route("same-login-as:codex").unwrap(),
        QuotaRoute::SameLoginAs("codex".into())
    );
    for bad in ["same-login-as:", "same-login-as", "sometimes", "", "Probe"] {
        assert!(route(bad).is_err(), "{bad:?}");
    }
    for text in ["none", "probe", "same-login-as:codex"] {
        round_trip::<QuotaRoute>(json!(text));
    }
    assert_eq!(QuotaRoute::default(), QuotaRoute::None);
    assert_eq!(
        QuotaRoute::SameLoginAs("codex".into()).to_string(),
        "same-login-as:codex"
    );
}

#[test]
fn model_entries_leave_out_an_unset_tier() {
    let entry = json!({"id": "codex/gpt-x", "family": "gpt", "kind": "harness",
        "access": "subscription", "tools": true,
        "cost": {"unit": "quota", "input_per_mtok": null, "output_per_mtok": null,
                 "cached_input_per_mtok": null},
        "strengths": [], "good_for": [], "context_window": 400000, "efforts": [],
        "aliases": [], "retired": false, "successor": null, "discovered": false,
        "launch": null});
    let untiered: ModelEntry = round_trip(entry.clone());
    assert_eq!(untiered.tier, None);
    let mut tiered = entry;
    tiered["tier"] = json!(4);
    assert_eq!(round_trip::<ModelEntry>(tiered).tier, Some(4));
}

#[test]
fn transports_and_the_context_fill_event_keep_their_wire_format() {
    assert_eq!(Transport::default(), Transport::Lines);
    round_trip::<Transport>(json!("lines"));
    round_trip::<Transport>(json!("acp"));
    let fill: AgentEvent =
        round_trip(json!({"type": "context_fill", "used": 120000, "size": 200000}));
    assert_eq!(fill.type_name(), "context_fill");
    let without_window: AgentEvent = round_trip(json!({"type": "context_fill", "used": 120000}));
    assert_eq!(
        without_window,
        AgentEvent::ContextFill {
            used: 120000,
            size: None,
            sent: None,
        }
    );
}

#[test]
fn front_ends_are_a_party_by_the_name_they_report() {
    let party: Party = round_trip(json!({"party": "front_end", "client": "hermes"}));
    assert_eq!(
        party,
        Party::FrontEnd {
            client: "hermes".into()
        }
    );
}

fn run_started() -> Value {
    json!({"event": "started", "request": "add per-key rate limiting",
        "title": "Per-key rate limiting", "mode": "crew", "forced": false,
        "workflow": "council", "estimate": null, "triage": null, "chat_turn": null,
        "continues": null, "repo_root": "/repo", "base_branch": "main",
        "base_commit": "8b41d07", "uncommitted": [], "uncommitted_included": false,
        "settings_sha256": "5e7f", "settings": {"workflow": "council"}, "overrides": [],
        "harness_versions": {"codex": "0.158.0"}, "version": "0.0.1"})
}

fn start_run() -> Value {
    json!({"request": "fix the typo", "mode": "solo", "forced": true, "estimate": null,
        "triage": null, "chat_turn": "t-001", "continues": null, "overrides": [],
        "include_uncommitted": false, "main_agent": "codex/gpt-x",
        "approval_mode": "supervised"})
}

#[test]
fn runs_without_a_front_end_keep_their_older_json() {
    let RunLifecycle::Started(started) = round_trip::<RunLifecycle>(run_started()) else {
        panic!("not a start");
    };
    assert_eq!(started.front_end, None);
    let mut from_chat = run_started();
    from_chat["front_end"] = json!("hermes");
    let RunLifecycle::Started(started) = round_trip::<RunLifecycle>(from_chat) else {
        panic!("not a start");
    };
    assert_eq!(started.front_end.as_deref(), Some("hermes"));

    assert_eq!(round_trip::<StartRun>(start_run()).front_end, None);
    let mut from_chat = start_run();
    from_chat["front_end"] = json!("openclaw");
    assert_eq!(
        round_trip::<StartRun>(from_chat).front_end.as_deref(),
        Some("openclaw")
    );
}

#[test]
fn every_driver_stop_keeps_its_wire_format() {
    for stop in [
        json!({"stop": "end_turn"}),
        json!({"stop": "cancelled"}),
        json!({"stop": "max_tokens"}),
        json!({"stop": "max_turn_requests"}),
        json!({"stop": "refusal"}),
        json!({"stop": "failed", "category": "auth", "message": "not logged in"}),
        json!({"stop": "disconnected"}),
    ] {
        round_trip::<DriverStop>(stop);
    }
    round_trip::<DriverEnd>(json!({"vendor_session": "019a-abc", "stop": {"stop": "end_turn"}}));
    round_trip::<DriverEnd>(json!({"vendor_session": null, "stop": {"stop": "disconnected"}}));
}

#[test]
fn driver_starts_and_frames_round_trip() {
    let start: DriverStart = round_trip(json!({
        "cwd": "/state/run-01/worktrees/task-002", "prompt": "Add the token bucket store",
        "model": "z-ai/glm-5", "effort": null, "access": "build", "resume": "019a-abc",
        "mcp_servers": [{"name": "argonauts", "command": "/usr/local/bin/argonauts",
                         "args": ["mcp", "session"]}],
        "scope": {"roots": ["/state/run-01/worktrees/task-002"],
                  "writable": ["/state/run-01/worktrees/task-002"], "deny": ["/repo/.git"],
                  "forbidden": [".env*"], "network": false},
        "json_schema": {"type": "object"},
        "report_file": "/state/run-01/sessions/s-003/report.json"}));
    assert_eq!(
        start.mcp_servers[0],
        McpServerSpec {
            name: "argonauts".into(),
            command: "/usr/local/bin/argonauts".into(),
            args: vec!["mcp".into(), "session".into()],
        }
    );
    round_trip::<RawFrame>(json!({"outgoing": true, "line": "{\"jsonrpc\":\"2.0\"}"}));
    let scope = PermissionScope::default();
    assert!(scope.roots.is_empty() && scope.writable.is_empty() && scope.deny.is_empty());
    assert!(scope.forbidden.is_empty() && !scope.network);
}

/// Sends the prompt as one frame, reads one reply line, and ends the turn.
struct OneTurn;

impl SessionDriver for OneTurn {
    fn run(&self, io: DriverIo, start: DriverStart) -> BoxFuture<'static, DriverEnd> {
        Box::pin(async move {
            let DriverIo {
                mut stdin,
                stdout,
                events,
                frames,
                ..
            } = io;
            let sent = json!({"prompt": start.prompt}).to_string();
            let failed = |message: String| DriverEnd {
                vendor_session: None,
                stop: DriverStop::Failed {
                    category: ErrorCategory::Cli,
                    message,
                },
            };
            if let Err(err) = stdin.write_all(format!("{sent}\n").as_bytes()).await {
                return failed(err.to_string());
            }
            let _ = frames.send(RawFrame {
                outgoing: true,
                line: sent,
            });
            let Ok(Some(line)) = BufReader::new(stdout).lines().next_line().await else {
                return DriverEnd {
                    vendor_session: None,
                    stop: DriverStop::Disconnected,
                };
            };
            let _ = events.send(AgentEvent::Message {
                text: line.clone(),
                parent: None,
            });
            let _ = frames.send(RawFrame {
                outgoing: false,
                line,
            });
            DriverEnd {
                vendor_session: Some("sess-1".into()),
                stop: DriverStop::EndTurn,
            }
        })
    }
}

#[tokio::test]
async fn a_session_driver_runs_over_the_childs_pipes() {
    let (driver_stdin, agent_stdin) = tokio::io::duplex(1024);
    let (mut agent_stdout, driver_stdout) = tokio::io::duplex(1024);
    let (_stderr_tx, stderr_lines) = mpsc::channel(4);
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (_control_tx, control) = mpsc::channel(4);
    let (frames_tx, mut frames) = mpsc::unbounded_channel();
    agent_stdout.write_all(b"done\n").await.unwrap();

    let driver: Arc<dyn SessionDriver> = Arc::new(OneTurn);
    let io = DriverIo {
        stdin: Box::new(driver_stdin),
        stdout: Box::new(driver_stdout),
        stderr_lines,
        events: events_tx,
        control,
        frames: frames_tx,
    };
    let start = DriverStart {
        cwd: "/tmp/wt".into(),
        prompt: "hi".into(),
        model: None,
        effort: None,
        access: argonauts_core::worker::Access::ReadOnly,
        resume: None,
        mcp_servers: Vec::new(),
        scope: PermissionScope::default(),
        json_schema: None,
        report_file: None,
        asking: None,
    };
    let end = driver.run(io, start).await;
    assert_eq!(end.stop, DriverStop::EndTurn);
    assert_eq!(end.vendor_session.as_deref(), Some("sess-1"));

    let mut sent = String::new();
    BufReader::new(agent_stdin)
        .read_line(&mut sent)
        .await
        .unwrap();
    assert_eq!(sent, "{\"prompt\":\"hi\"}\n");
    assert!(
        matches!(events.recv().await, Some(AgentEvent::Message { text, .. }) if text == "done")
    );
    assert!(frames.recv().await.unwrap().outgoing);
    assert!(!frames.recv().await.unwrap().outgoing);
}

struct LinesOnly;

struct NoEvents;

impl StreamParser for NoEvents {
    fn parse_stdout_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        Vec::new()
    }
}

impl Adapter for LinesOnly {
    fn id(&self) -> &'static str {
        "lines-only"
    }
    fn display_name(&self) -> &'static str {
        "Lines only"
    }
    fn binary(&self) -> &'static str {
        "lines-only"
    }
    fn command(&self, _req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        anyhow::bail!("never launched")
    }
    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(NoEvents)
    }
    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }
    fn parse_version(&self, output: &str) -> Option<String> {
        Some(output.trim().to_owned())
    }
    fn tested_versions(&self) -> &'static [&'static str] {
        &[]
    }
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("small", None)
    }
    fn stop_plan(&self) -> StopPlan {
        StopPlan::escalating(
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        )
    }
}

#[test]
fn adapters_read_lines_and_have_no_driver_unless_they_say_so() {
    assert_eq!(LinesOnly.transport(), Transport::Lines);
    assert!(LinesOnly.session_driver().is_none());
}

#[test]
fn the_new_integration_defaults_hold() {
    let settings = Settings::default();
    let mcp = &settings.mcp;
    assert!(mcp.status_tool && mcp.guidance && mcp.pause);
    assert_eq!(mcp.start_runs, StartRuns::TrustedRepos);
    assert_eq!(mcp.share, Share::Summaries);
    assert_eq!(mcp.run_limit, Span::new("60m"));
    assert_eq!(mcp.runs_per_day, 5);
    assert_eq!(settings.approvals.presence, Presence::Warn);
    assert_eq!(
        settings.approvals.refuse_ancestor_env,
        ["OPENCLAW_SHELL", "HERMES_SESSION_ID"]
    );
    let chat = &settings.notifications.chat;
    assert!(chat.command.is_empty());
    assert_eq!(
        chat.on,
        [
            ChatTrigger::NeedsYou,
            ChatTrigger::Finished,
            ChatTrigger::VendorDown
        ]
    );
}

#[test]
fn settings_with_the_integration_keys_set_pass_the_schema() {
    let settings: Settings = toml::from_str(
        r#"
[mcp]
status_tool = false
start_runs = "off"
guidance = false
pause = false
share = "status-only"
run_limit = "30m"
runs_per_day = 2
[approvals]
presence = "required"
refuse_ancestor_env = ["OTHER_AGENT_SESSION"]
[notifications.chat]
command = ["/usr/local/bin/hermes", "send", "{text}"]
on = ["needs_you"]
"#,
    )
    .unwrap();
    assert_eq!(settings.mcp.start_runs, StartRuns::Off);
    assert_eq!(settings.mcp.share, Share::StatusOnly);
    assert_eq!(settings.approvals.presence, Presence::Required);
    assert_eq!(settings.notifications.chat.on, [ChatTrigger::NeedsYou]);
    let value = serde_json::to_value(&settings).unwrap();
    let errors = schema_errors(schemars::schema_for!(Settings).to_value(), &value);
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(serde_json::from_value::<Settings>(value).unwrap(), settings);
}

#[test]
fn chat_commands_are_safety_keys_only_your_global_config_may_set() {
    assert_eq!(GLOBAL_ONLY_KEYS, ["notifications.chat.*"]);
    for key in GLOBAL_ONLY_KEYS {
        assert!(SAFETY_KEYS.contains(key), "{key}");
    }
    assert!(SAFETY_KEYS.contains(&"mcp.*"));
}

#[test]
fn integration_files_have_fixed_names() {
    assert_eq!(session_files::REPORT, "report.json");
    assert_eq!(session_files::PROMPT, "prompt.md");
    assert_eq!(session_files::HOME, "home");
    assert_eq!(session_files::RAW_STDIN, "raw.stdin.jsonl");
    assert_eq!(session_files::RAW_STDOUT, "raw.stdout.jsonl");
    assert_eq!(user_files::HARNESSES, "harnesses");
}
