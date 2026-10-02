//! Replays of OpenCode 1.18.34 output recorded against a local Ollama (see the folder's
//! `meta.json`), next to the hand-written 1.18.33 ones in `fixtures.rs`.

use std::path::{Path, PathBuf};

use argonauts_adapter_opencode::OpenCodeAdapter;
use argonauts_core::{Adapter, AgentEvent, ErrorCategory, SessionRequest};
use serde_json::Value;

const VERSION: &str = "1.18.34";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(VERSION)
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
    let cmd = OpenCodeAdapter::new().command(req).unwrap();
    let args: Vec<String> = serde_json::from_value(meta["args"].clone()).unwrap();
    assert_eq!(cmd.program, meta["program"].as_str().unwrap());
    assert_eq!(cmd.args, args, "{name}");
    assert_eq!(cmd.stdin.as_deref(), meta["prompt"].as_str());
}

#[test]
fn recorded_build_parses() {
    let folder: Value = serde_json::from_str(&read("meta.json")).unwrap();
    assert_eq!(folder["synthetic"], false);
    assert_eq!(folder["cli_version"], VERSION);
    assert_eq!(OpenCodeAdapter::new().tested_versions(), [VERSION]);
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
