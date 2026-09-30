use std::path::{Path, PathBuf};

use argonauts_adapter_codex::{CodexAdapter, TESTED_VERSIONS};
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
            replay.usage();
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
        ..SessionRequest::default()
    }
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
    assert_eq!(replay.types(), ["init", "message", "usage", "result"]);
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
            "message",
            "usage",
            "result"
        ]
    );
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
            SessionRole::Build => "workspace-write",
        };
        assert_eq!(args[at + 1], expected, "{name}");
    }
}
