//! Replays of OpenCode output recorded against a local Ollama (each recording's meta says
//! how), next to the hand-written ones in `fixtures.rs`.

use std::path::{Path, PathBuf};

use argonauts_adapter_opencode::{OpenCodeAdapter, TESTED_VERSIONS};
use argonauts_core::{Adapter, AgentEvent, ErrorCategory, SessionRequest};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read(name: &str) -> String {
    let path = fixtures_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn meta(name: &str) -> Value {
    serde_json::from_str(&read(&format!("{name}.meta.json"))).unwrap()
}

fn replay(name: &str) -> Vec<AgentEvent> {
    let meta = meta(name);
    assert_eq!(meta["synthetic"], false, "{name} is a recording");
    let exit_code = meta["exit_code"]
        .as_i64()
        .map(|c| i32::try_from(c).unwrap());
    let mut parser = OpenCodeAdapter::new().parser();
    let mut events: Vec<AgentEvent> = read(&format!("{name}.stdout.jsonl"))
        .lines()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect();
    events.extend(
        read(&format!("{name}.stderr.txt"))
            .lines()
            .flat_map(|line| parser.parse_stderr_line(line)),
    );
    events.extend(parser.finish(exit_code));
    events
}

fn request(name: &str, resume: Option<&str>, extra_args: &[&str]) -> SessionRequest {
    let meta = meta(name);
    SessionRequest {
        workdir: PathBuf::from("/work/tree"),
        prompt: meta["prompt"].as_str().unwrap().to_owned(),
        model: meta["model"].as_str().map(str::to_owned),
        resume_session: resume.map(str::to_owned),
        extra_args: extra_args.iter().map(|arg| (*arg).to_owned()).collect(),
        ..SessionRequest::default()
    }
}

/// The command each recording's meta names is the one the adapter builds today.
fn assert_command_matches(name: &str, req: &SessionRequest) {
    let meta = meta(name);
    // The version it was recorded with decides the flags, as the installed one does.
    let req = SessionRequest {
        cli_version: meta["cli_version"].as_str().map(str::to_owned),
        ..req.clone()
    };
    let cmd = OpenCodeAdapter::new().command(&req).unwrap();
    let args: Vec<String> = serde_json::from_value(meta["args"].clone()).unwrap();
    assert_eq!(cmd.program, meta["program"].as_str().unwrap());
    assert_eq!(cmd.args, args, "{name}");
    assert_eq!(cmd.stdin.as_deref(), meta["prompt"].as_str());
}

#[test]
fn recorded_build_parses() {
    assert_eq!(OpenCodeAdapter::new().tested_versions(), TESTED_VERSIONS);
    assert_eq!(
        TESTED_VERSIONS,
        ["build_hello", "build_hello_standalone"]
            .map(|name| meta(name)["cli_version"].as_str().unwrap().to_owned())
    );
    assert_command_matches(
        "build_hello",
        &request("build_hello", None, &["--thinking"]),
    );

    let events = replay("build_hello");
    assert!(matches!(
        &events[0],
        AgentEvent::Init { session_id: Some(id), model: None, .. }
            if id == "ses_f03e2788effeHx5RxCybTvStVR"
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolStarted { id, name, input, .. }
            if id == "call_io5920kl" && name == "write" && input["content"] == "hi"
    )));
    assert!(events.contains(&AgentEvent::ToolFinished {
        id: "call_io5920kl".to_owned(),
        ok: true,
        output: Some("Wrote file successfully.".to_owned()),
        parent: None,
    }));
    let usage: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cumulative: false,
                cost_usd: Some(cost),
                ..
            } if *cost == 0.0 => Some((*input_tokens, *output_tokens)),
            _ => None,
        })
        .collect();
    assert_eq!(usage, [(6487, 53), (2256, 2)]);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Result {
            text: Some("done".to_owned()),
            structured: None,
            is_error: false,
        })
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Unknown { .. }))
    );
}

/// OpenCode 2.0.32 on a private server, with the provider config the adapter sends inline: the
/// command is the one the adapter builds for 2.0.32, and a model that can't call the tools
/// reads as tool calls that failed, every one finished, and the run's last text as its answer.
#[test]
fn a_standalone_run_whose_model_cant_call_the_tools_reads_whole() {
    assert_command_matches(
        "build_hello_standalone",
        &request("build_hello_standalone", None, &[]),
    );
    let events = replay("build_hello_standalone");
    assert!(
        matches!(&events[0], AgentEvent::Init { .. }),
        "{:?}",
        events[0]
    );
    let started = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolStarted { .. }))
        .count();
    let failed = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolFinished { ok: false, .. }))
        .count();
    let finished = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolFinished { .. }))
        .count();
    assert_eq!((started, finished), (19, 19));
    assert!(failed >= 12, "{failed} of the calls failed");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::FileChanged { .. }))
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Result { text: Some(text), is_error: false, .. })
            if text.contains("\"name\": \"ls\"")
    ));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Unknown { .. }))
    );
}

#[test]
fn recorded_missing_session_says_why() {
    let session = "ses_0000000000000000000000000000";
    assert_command_matches(
        "resume_missing",
        &request("resume_missing", Some(session), &[]),
    );

    let events = replay("resume_missing");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Error { category: ErrorCategory::Cli, message, retryable: false }
            if message == "opencode exited with code 1: Session not found"
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Result {
            text: None,
            is_error: true,
            ..
        })
    ));
}
