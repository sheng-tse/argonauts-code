//! Replays of hand-written OpenCode 1.18.33 output. OpenCode isn't installed here, so every
//! fixture is synthetic, built from the event shapes in OpenCode's source.

use std::path::{Path, PathBuf};

use argonauts_adapter_opencode::OpenCodeAdapter;
use argonauts_core::{Adapter, AgentEvent, ErrorCategory, SessionRequest};
use serde_json::Value;

const VERSION: &str = "1.18.33";
const SESSION: &str = "ses_3a1f0c2e5ffeYk2hQ8mZr7TnWb";

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
    assert_eq!(meta["synthetic"], true, "{name} must say it is synthetic");
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

/// The command each fixture's meta names is the one the adapter builds today.
fn assert_command_matches(name: &str, req: &SessionRequest) {
    let meta = meta(name);
    let cmd = OpenCodeAdapter::new().command(req).unwrap();
    let args: Vec<String> = serde_json::from_value(meta["args"].clone()).unwrap();
    assert_eq!(cmd.program, meta["program"].as_str().unwrap());
    assert_eq!(cmd.args, args, "{name}");
    assert_eq!(cmd.stdin.as_deref(), meta["prompt"].as_str());
}

#[test]
fn opencode_events_parse() {
    let folder: Value = serde_json::from_str(&read("meta.json")).unwrap();
    assert_eq!(folder["synthetic"], true);

    assert_command_matches(
        "build_hello",
        &SessionRequest {
            prompt: meta("build_hello")["prompt"].as_str().unwrap().to_owned(),
            model: Some("ollama/llama3.2".to_owned()),
            // Both synthetic runs asked for reasoning parts, which `--thinking` turns on.
            extra_args: vec!["--thinking".to_owned()],
            ..SessionRequest::default()
        },
    );
    let events = replay("build_hello");
    assert_eq!(
        events[0],
        AgentEvent::Init {
            session_id: Some(SESSION.to_owned()),
            model: None,
            tools: Vec::new(),
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        }
    );
    let inits = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Init { .. }))
        .count();
    assert_eq!(inits, 1);
    assert!(events.contains(&AgentEvent::Reasoning {
        text: "I need to create hello.txt with the word hi.".to_owned()
    }));
    let started = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolStarted {
                id, name, input, ..
            } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
        .unwrap();
    assert_eq!(started.0, "call_write_1");
    assert_eq!(started.1, "write");
    assert_eq!(started.2["filePath"], "/work/tree/hello.txt");
    assert!(events.contains(&AgentEvent::ToolFinished {
        id: "call_write_1".to_owned(),
        ok: true,
        output: Some("Wrote file successfully.".to_owned()),
        parent: None,
    }));
    let usage: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cached_tokens,
                cumulative,
                ..
            } => Some((*input_tokens, *output_tokens, *cached_tokens, *cumulative)),
            _ => None,
        })
        .collect();
    // Cache reads added back to input, reasoning to output.
    assert_eq!(usage, [(9272, 88, 1152, false), (9426, 2, 9216, false)]);
    assert!(events.contains(&AgentEvent::ContextFill {
        used: 9360,
        size: None
    }));
    assert!(events.contains(&AgentEvent::Message {
        text: "done".to_owned(),
        parent: None
    }));
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
            .any(|e| matches!(e, AgentEvent::Unknown { .. } | AgentEvent::Error { .. }))
    );

    assert_command_matches(
        "resume_error",
        &SessionRequest {
            prompt: meta("resume_error")["prompt"].as_str().unwrap().to_owned(),
            model: Some("openrouter/z-ai/glm-5".to_owned()),
            resume_session: Some(SESSION.to_owned()),
            extra_args: vec!["--thinking".to_owned()],
            ..SessionRequest::default()
        },
    );
    let events = replay("resume_error");
    assert!(events.contains(&AgentEvent::ToolFinished {
        id: "call_read_1".to_owned(),
        ok: false,
        output: Some("File not found: /work/tree/missing.txt".to_owned()),
        parent: None,
    }));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Usage {
            input_tokens: 1700,
            cached_tokens: 0,
            cost_usd: Some(_),
            ..
        }
    )));
    let errors: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Error { .. }))
        .collect();
    assert_eq!(
        errors,
        [&AgentEvent::Error {
            category: ErrorCategory::RateLimit,
            message: "Rate limit exceeded".to_owned(),
            retryable: true,
        }]
    );
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        })
    );
}
