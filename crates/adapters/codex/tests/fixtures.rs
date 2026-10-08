use std::path::{Path, PathBuf};

use argonauts_adapter_codex::{CodexAdapter, TESTED_VERSIONS};
use argonauts_core::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use argonauts_core::limits::{QuotaSource, QuotaStatus};
use argonauts_core::{
    Adapter, AgentEvent, Report, ReportStatus, SessionRequest, SessionRole, report_json_schema,
};
use serde_json::{Value, json};

const VERSION: &str = "0.158.0";

struct Fixture {
    name: String,
    stdout: String,
    stderr: String,
    meta: Value,
}

impl Fixture {
    fn load(version: &str, name: &str) -> Self {
        let base = fixtures_dir().join(version).join(name);
        let read = |ext: &str| {
            let path = base.with_extension(ext);
            std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        };
        Self {
            name: name.to_owned(),
            stdout: read("stdout.jsonl"),
            stderr: read("stderr.txt"),
            meta: serde_json::from_str(&read("meta.json")).unwrap(),
        }
    }

    fn exit_code(&self) -> Option<i32> {
        self.meta["exit_code"]
            .as_i64()
            .map(|c| i32::try_from(c).unwrap())
    }

    fn replay(&self) -> Replay {
        let mut parser = CodexAdapter.parser();
        let stdout = self
            .stdout
            .lines()
            .flat_map(|line| parser.parse_stdout_line(line))
            .collect();
        let stderr = self
            .stderr
            .lines()
            .flat_map(|line| parser.parse_stderr_line(line))
            .collect();
        let finish = parser.finish(self.exit_code());
        Replay {
            stdout,
            stderr,
            finish,
        }
    }
}

struct Replay {
    stdout: Vec<AgentEvent>,
    stderr: Vec<AgentEvent>,
    finish: Vec<AgentEvent>,
}

impl Replay {
    fn all(&self) -> Vec<&AgentEvent> {
        self.stdout
            .iter()
            .chain(&self.stderr)
            .chain(&self.finish)
            .collect()
    }

    fn types(&self) -> Vec<&'static str> {
        self.all().into_iter().map(AgentEvent::type_name).collect()
    }

    fn session_id(&self) -> String {
        match &self.stdout[0] {
            AgentEvent::Init {
                session_id: Some(id),
                ..
            } => id.clone(),
            other => panic!("first event is not init: {other:?}"),
        }
    }

    fn usage(&self) -> (u64, u64, u64) {
        self.all()
            .into_iter()
            .find_map(|event| match event {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cached_tokens,
                    cumulative: true,
                    cost_usd: None,
                } => Some((*input_tokens, *output_tokens, *cached_tokens)),
                _ => None,
            })
            .expect("a cumulative usage event")
    }

    fn result(&self) -> (Option<String>, Option<Value>, bool) {
        match self.all().last() {
            Some(AgentEvent::Result {
                text,
                structured,
                is_error,
            }) => (text.clone(), structured.clone(), *is_error),
            other => panic!("last event is not a result: {other:?}"),
        }
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn fixture_names(version: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir().join(version))
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.strip_suffix(".stdout.jsonl").map(str::to_owned)
        })
        .collect();
    names.sort();
    names
}

#[test]
fn fixture_folders_match_tested_versions() {
    let mut versions: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    versions.sort();
    assert_eq!(versions, TESTED_VERSIONS);
}

#[test]
fn every_fixture_replays_cleanly() {
    for version in TESTED_VERSIONS {
        let names = fixture_names(version);
        assert!(!names.is_empty(), "no fixtures for {version}");
        for name in names {
            let fixture = Fixture::load(version, &name);
            let replay = fixture.replay();
            assert!(
                !replay
                    .stdout
                    .iter()
                    .any(|e| matches!(e, AgentEvent::Unknown { .. })),
                "{name}: unrecognized stdout lines in {:?}",
                replay.stdout
            );
            assert!(!replay.session_id().is_empty(), "{name}");
            let results = replay
                .all()
                .iter()
                .filter(|e| e.type_name() == "result")
                .count();
            assert_eq!(results, 1, "{name}");
            assert_eq!(replay.result().2, fixture.exit_code() != Some(0), "{name}");
            // An interrupted turn has no closing event, so no usage.
            if fixture.exit_code() == Some(0) {
                replay.usage();
            }
        }
    }
}

#[test]
fn fixtures_use_the_cheapest_model_and_hold_no_local_paths() {
    let (model, effort) = CodexAdapter.cheapest_model();
    for name in fixture_names(VERSION) {
        let fixture = Fixture::load(VERSION, &name);
        assert_eq!(fixture.meta["model"], model, "{name}");
        assert_eq!(fixture.meta["effort"], json!(effort), "{name}");
        for text in [&fixture.stdout, &fixture.stderr, &fixture.meta.to_string()] {
            for leak in ["/Users/", "/home/", "/private/", "sk-"] {
                assert!(!text.contains(leak), "{name} contains {leak:?}");
            }
        }
    }
}

fn recorded_request(fixture: &Fixture) -> SessionRequest {
    let meta = &fixture.meta;
    let resume_session = meta["resumed_from"]
        .as_str()
        .map(|from| Fixture::load(VERSION, from).replay().session_id());
    SessionRequest {
        workdir: PathBuf::from("<workdir>"),
        prompt: meta["prompt"].as_str().unwrap().to_owned(),
        model: meta["model"].as_str().map(str::to_owned),
        effort: meta["effort"].as_str().map(str::to_owned),
        role: serde_json::from_value(meta["role"].clone()).unwrap(),
        json_schema: meta["output_schema"]
            .as_bool()
            .unwrap()
            .then(report_json_schema),
        resume_session,
        scratch_dir: Some(PathBuf::from("<scratch>")),
        compaction: optional(&meta["compaction"]),
        native_fanout: optional(&meta["native_fanout"]),
        user_hooks_off: meta["user_hooks_off"].as_bool().unwrap_or(false),
        ..SessionRequest::default()
    }
}

/// Fields that recordings made before them don't have.
fn optional<T: serde::de::DeserializeOwned>(value: &Value) -> Option<T> {
    (!value.is_null()).then(|| serde_json::from_value(value.clone()).unwrap())
}

#[test]
fn command_still_builds_the_recorded_arguments() {
    for name in fixture_names(VERSION) {
        let fixture = Fixture::load(VERSION, &name);
        let spec = CodexAdapter.command(&recorded_request(&fixture)).unwrap();
        let recorded: Vec<String> = serde_json::from_value(fixture.meta["args"].clone()).unwrap();
        assert_eq!(spec.program, fixture.meta["program"], "{}", fixture.name);
        assert_eq!(spec.args, recorded, "{}", fixture.name);
    }
}

#[test]
fn read_only_reply() {
    let replay = Fixture::load(VERSION, "read_only_ok").replay();
    assert_eq!(
        replay.types(),
        ["init", "message", "usage", "context_fill", "result"]
    );
    assert_eq!(
        replay.stdout[1],
        AgentEvent::Message {
            text: "ok".into(),
            parent: None
        }
    );
    assert_eq!(replay.usage(), (13464, 5, 11008));
    assert_eq!(replay.result(), (Some("ok".into()), None, false));
    assert!(Report::from_text("ok").is_none());
}

#[test]
fn build_writes_a_file_and_returns_a_structured_report() {
    let fixture = Fixture::load(VERSION, "build_hello");
    assert_eq!(fixture.meta["role"], "build");
    assert!(fixture.meta["args"].to_string().contains("workspace-write"));
    let replay = fixture.replay();
    assert_eq!(
        replay.types(),
        [
            "init",
            "message",
            "tool_started",
            "tool_finished",
            "file_changed",
            "tool_started",
            "tool_finished",
            "usage",
            "context_fill",
            "result"
        ]
    );
    // Every message is schema-shaped. The preamble reads as its summary, and the last one is
    // the report, a call the transcript shows as its summary.
    assert_eq!(
        replay.stdout[1],
        AgentEvent::Message {
            text: "Creating hello.txt now.".into(),
            parent: None
        }
    );
    assert!(matches!(
        &replay.stdout[5],
        AgentEvent::ToolStarted { name, input, .. }
            if name == "structured_output" && input["files_changed"] == json!(["hello.txt"])
    ));
    assert_eq!(
        replay.stdout[2],
        AgentEvent::ToolStarted {
            id: "item_1".into(),
            name: "file_change".into(),
            input: json!({"changes": [{"path": "<workdir>/hello.txt", "kind": "add"}]}),
            parent: None,
        }
    );
    assert!(matches!(
        &replay.stdout[3],
        AgentEvent::ToolFinished { id, ok: true, .. } if id == "item_1"
    ));
    assert_eq!(
        replay.stdout[4],
        AgentEvent::FileChanged {
            path: "<workdir>/hello.txt".into(),
            kind: "add".into()
        }
    );
    assert_eq!(replay.usage(), (28204, 138, 13056));

    // Codex sends a schema-shaped preamble first; the result is the last message.
    let (text, structured, is_error) = replay.result();
    assert!(!is_error);
    let report = Report::from_value(&structured.expect("structured report")).unwrap();
    assert_eq!(report.status, ReportStatus::Done);
    assert_eq!(report.files_changed, ["hello.txt"]);
    assert_eq!(Report::from_text(&text.unwrap()), Some(report));
}

#[test]
fn resume_continues_the_session_and_runs_a_command() {
    let build = Fixture::load(VERSION, "build_hello").replay();
    let fixture = Fixture::load(VERSION, "resume_ls");
    assert_eq!(fixture.meta["role"], "read-only");
    let replay = fixture.replay();
    assert_eq!(replay.session_id(), build.session_id());
    assert_eq!(
        replay.types(),
        [
            "init",
            "tool_started",
            "tool_finished",
            "message",
            "usage",
            "context_fill",
            "result"
        ]
    );
    assert_eq!(
        replay.stdout[1],
        AgentEvent::ToolStarted {
            id: "item_0".into(),
            name: "command_execution".into(),
            input: json!({"command": "/bin/zsh -lc ls"}),
            parent: None,
        }
    );
    assert_eq!(
        replay.stdout[2],
        AgentEvent::ToolFinished {
            id: "item_0".into(),
            ok: true,
            output: Some("README.md\nhello.txt\n".into()),
            parent: None,
        }
    );
    // Codex seeds a resumed session with its earlier totals, so usage keeps growing.
    let (input, output, cached) = replay.usage();
    let (build_input, build_output, build_cached) = build.usage();
    assert!(input > build_input && output > build_output && cached > build_cached);
    assert_eq!(replay.result(), (Some("2 files".into()), None, false));
}

#[test]
fn recorded_roles_match_their_sandbox() {
    for name in fixture_names(VERSION) {
        let fixture = Fixture::load(VERSION, &name);
        let role: SessionRole = serde_json::from_value(fixture.meta["role"].clone()).unwrap();
        let args: Vec<String> = serde_json::from_value(fixture.meta["args"].clone()).unwrap();
        let at = args.iter().position(|a| a == "--sandbox").unwrap();
        let expected = match role {
            SessionRole::ReadOnly => "read-only",
            SessionRole::Build | SessionRole::Direct => "workspace-write",
        };
        assert_eq!(args[at + 1], expected, "{name}");
    }
}

fn subagent_updates(replay: &Replay) -> Vec<&SubagentUpdate> {
    replay
        .all()
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::Subagent { update } => Some(update),
            _ => None,
        })
        .collect()
}

#[test]
fn interrupt_then_resume_with_a_message() {
    let fixture = Fixture::load(VERSION, "interrupt_sleep");
    assert_eq!(fixture.meta["interrupted"], true);
    assert_eq!(fixture.exit_code(), Some(1));
    let interrupted = fixture.replay();
    // SIGINT ends the turn with no closing event: the open command fails, then the session.
    assert_eq!(
        interrupted.types(),
        [
            "init",
            "message",
            "tool_started",
            "tool_finished",
            "error",
            "result"
        ]
    );
    assert!(matches!(
        &interrupted.finish[0],
        AgentEvent::ToolFinished { id, ok: false, .. } if id == "item_1"
    ));
    assert!(interrupted.result().2);

    let fixture = Fixture::load(VERSION, "resume_with_message");
    let resumed = fixture.replay();
    assert_eq!(resumed.session_id(), interrupted.session_id());
    assert_eq!(resumed.result(), (Some("resumed".into()), None, false));
    // The message went on stdin after `resume <id> -`, with every flag of the first launch.
    let first: Vec<String> =
        serde_json::from_value(Fixture::load(VERSION, "interrupt_sleep").meta["args"].clone())
            .unwrap();
    let args: Vec<String> = serde_json::from_value(fixture.meta["args"].clone()).unwrap();
    let resume = args.iter().position(|a| a == "resume").unwrap();
    assert_eq!(args[..resume], first[..first.len() - 1]);
    assert_eq!(args[resume..], ["resume", &resumed.session_id(), "-"]);
}

#[test]
fn codex_collab_threads_to_subagents() {
    let fixture = Fixture::load(VERSION, "subagent_spawn");
    assert!(
        fixture.meta["args"]
            .to_string()
            .contains("agents.max_concurrent_threads_per_session=1")
    );
    let replay = fixture.replay();
    let updates = subagent_updates(&replay);
    let child = "01a0f090-1628-71a3-8bb1-79018a946869";
    assert_eq!(updates.len(), 2, "{updates:?}");
    for update in &updates {
        assert_eq!(update.id, child);
        assert_eq!(update.parent, None);
        assert_eq!(update.kind, SubagentKind::Thread);
        assert!(!update.steps_visible);
        assert_eq!(
            update.prompt.as_deref(),
            Some("Count the files in this repository. Reply with just the number.")
        );
    }
    assert_eq!(updates[0].status, SubagentStatus::Started);
    assert_eq!(updates[0].last_message, None);
    assert_eq!(updates[1].status, SubagentStatus::Done);
    assert_eq!(updates[1].last_message.as_deref(), Some("1"));
    // The child's answer also shows under it in the transcript, and the main agent reports it.
    assert!(replay.all().contains(&&AgentEvent::Message {
        text: "1".into(),
        parent: Some(child.into())
    }));
    assert_eq!(replay.result(), (Some("1".into()), None, false));
}

#[test]
fn codex_messages_arrive_whole() {
    for name in fixture_names(VERSION) {
        let fixture = Fixture::load(VERSION, &name);
        let replay = fixture.replay();
        assert!(!replay.types().contains(&"text_delta"), "{name}");
        let mut items: Vec<String> = fixture
            .stdout
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|v| v["type"] == "item.completed" && v["item"]["type"] == "agent_message")
            .map(|v| v["item"]["text"].as_str().unwrap().to_owned())
            .collect();
        // A schema-shaped message shows as its summary, and the last one is the report.
        if let Some(Value::Object(_)) = items.last().and_then(|t| serde_json::from_str(t).ok()) {
            items.pop();
        }
        for item in &mut items {
            if let Some(summary) = serde_json::from_str::<Value>(item)
                .ok()
                .as_ref()
                .and_then(argonauts_core::steps::summary_line)
            {
                *item = summary;
            }
        }
        let messages: Vec<String> = replay
            .all()
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::Message { text, parent: None } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(!items.is_empty(), "{name}");
        assert_eq!(messages, items, "{name}");
    }
}

#[test]
fn context_fill_from_each_vendor() {
    for name in fixture_names(VERSION) {
        let fixture = Fixture::load(VERSION, &name);
        let replay = fixture.replay();
        let fills: Vec<(u64, Option<u64>)> = replay
            .all()
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::ContextFill { used, size, .. } => Some((*used, *size)),
                _ => None,
            })
            .collect();
        if fixture.exit_code() != Some(0) {
            assert!(fills.is_empty(), "{name}");
            continue;
        }
        // Codex never says the window's size; the worker fills it in from the registry.
        let (input, output, cached) = replay.usage();
        assert_eq!(fills, [(input - cached + output, None)], "{name}");
    }
    let replay = Fixture::load(VERSION, "resume_with_message").replay();
    assert!(replay.all().contains(&&AgentEvent::ContextFill {
        used: 8430,
        size: None,
        sent: None,
    }));
}

#[test]
fn codex_rate_limits_probe_parses() {
    let dir = fixtures_dir().join(VERSION);
    let answer = std::fs::read_to_string(dir.join("rate_limits_probe.app-server.jsonl")).unwrap();
    let meta: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("rate_limits_probe.meta.json")).unwrap(),
    )
    .unwrap();
    // The recorded exchange is the one the adapter sends.
    let spec = CodexAdapter.quota_probe(Path::new("<live>"), None).unwrap();
    assert_eq!(meta["args"], json!(spec.args));
    assert_eq!(
        requests(meta["stdin"].as_str().unwrap()),
        requests(spec.stdin.as_deref().unwrap())
    );
    assert!(spec.keep_stdin_open);

    // Nothing to read until the rate-limit answer arrives.
    let before: String = answer.lines().take(3).map(|l| format!("{l}\n")).collect();
    assert!(CodexAdapter.parse_quota_probe(&before).is_none());

    let reading = CodexAdapter.parse_quota_probe(&answer).unwrap();
    assert_eq!(reading.vendor.0, "codex");
    assert_eq!(reading.source, QuotaSource::AppServer);
    assert_eq!(reading.status, QuotaStatus::Allowed);
    assert_eq!(reading.used, Some(0.0));
    assert_eq!(reading.window.as_deref(), Some("five_hour"));
    assert_eq!(
        reading.resets_at.map(|t| t.unix_timestamp()),
        Some(1_790_760_327)
    );
    let detail = reading.detail.unwrap();
    assert!(detail.get("accountId").is_none());
    assert_eq!(
        detail["rateLimits"]["secondary"]["windowDurationMins"],
        10080
    );

    // Primary and secondary are two readings, named by their length: 300 minutes is the
    // five-hour window (`5h` on screen) and 10080 the weekly one (`7d`). The plan comes along.
    let answer = CodexAdapter.parse_quota_probe_answer(&answer).unwrap();
    let windows: Vec<_> = answer
        .windows
        .iter()
        .map(|r| {
            (
                r.window.as_deref().unwrap(),
                r.used.unwrap(),
                r.resets_at.unwrap().unix_timestamp(),
            )
        })
        .collect();
    assert_eq!(
        windows,
        [
            ("five_hour", 0.0, 1_790_760_327),
            ("weekly", 0.0, 1_791_259_618)
        ]
    );
    assert_eq!(answer.plan.as_deref(), Some("<plan>"));
}

/// Each request's id, method, and params, without the client version that changes with ours.
fn requests(stdin: &str) -> Vec<Value> {
    stdin
        .lines()
        .map(|line| {
            let mut request: Value = serde_json::from_str(line).unwrap();
            if let Some(info) = request.pointer_mut("/params/clientInfo") {
                info.as_object_mut().unwrap().remove("version");
            }
            request
        })
        .collect()
}
