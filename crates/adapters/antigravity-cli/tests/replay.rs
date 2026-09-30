use std::fs;
use std::path::{Path, PathBuf};

use argonauts_adapter_antigravity_cli::{AntigravityCli, TESTED_VERSIONS};
use argonauts_core::{
    Adapter, AgentEvent, BudgetLimit, BudgetMeter, ErrorCategory, Report, ReportStatus, RetryReason,
};
use serde_json::{Value, json};

const VERSION: &str = "1.2.13";

struct Replay {
    events: Vec<AgentEvent>,
    meta: Value,
}

fn replay(name: &str) -> Replay {
    let dir = fixtures_dir().join(VERSION);
    let read = |ext: &str| fs::read_to_string(dir.join(format!("{name}.{ext}"))).unwrap();
    let meta: Value = serde_json::from_str(&read("meta.json")).unwrap();
    let exit_code = meta["exit_code"].as_i64().map(|c| c as i32);

    let mut parser = AntigravityCli.parser();
    let mut events = Vec::new();
    for line in read("stdout.jsonl").lines() {
        events.extend(parser.parse_stdout_line(line));
    }
    for line in read("stderr.txt").lines() {
        events.extend(parser.parse_stderr_line(line));
    }
    events.extend(parser.finish(exit_code));
    Replay { events, meta }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn names(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(AgentEvent::type_name).collect()
}

fn usage(input: u64, output: u64, cumulative: bool) -> AgentEvent {
    AgentEvent::Usage {
        input_tokens: input,
        output_tokens: output,
        cached_tokens: 0,
        cost_usd: None,
        cumulative,
    }
}

fn billable_tokens(events: &[AgentEvent]) -> u64 {
    let mut meter = BudgetMeter::new(BudgetLimit::Tokens(10_000_000), 0.85);
    for event in events {
        meter.observe(event);
    }
    meter.spend().billable_tokens()
}

#[test]
fn every_tested_version_has_fixtures() {
    for version in TESTED_VERSIONS {
        let dir = fixtures_dir().join(version);
        let recordings = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".meta.json"))
            .count();
        assert!(recordings >= 3, "{version} has {recordings} recordings");
    }
}

#[test]
fn recordings_used_the_cheapest_model() {
    for name in ["readonly_ok", "build_hello_schema", "build_shell"] {
        let meta = replay(name).meta;
        let args: Vec<&str> = meta["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let after = |flag: &str| {
            let at = args.iter().position(|a| *a == flag).unwrap();
            args[at + 1]
        };
        assert_eq!(after("--model"), "gemini-3.6-flash-low", "{name}");
        assert_eq!(after("--effort"), "low", "{name}");
        assert_eq!(meta["cli_version"], VERSION);
    }
}

#[test]
fn read_only_reply_streams_text_then_succeeds() {
    let Replay { events, .. } = replay("readonly_ok");
    assert_eq!(
        names(&events),
        [
            "init",
            "text_delta",
            "text_delta",
            "message",
            "usage",
            "usage",
            "result",
            "unknown"
        ]
    );
    match &events[0] {
        AgentEvent::Init {
            session_id,
            model,
            tools,
        } => {
            assert_eq!(
                session_id.as_deref(),
                Some("0dd9320f-06e4-49fd-a40f-0bd3b193c898")
            );
            assert_eq!(model.as_deref(), Some("gemini-3.6-flash-low"));
            assert!(tools.iter().any(|t| t == "run_command"));
            assert!(tools.iter().any(|t| t == "write_to_file"));
        }
        other => panic!("expected init, got {other:?}"),
    }
    assert_eq!(
        events[3],
        AgentEvent::Message {
            text: "ok\n".into(),
            parent: None,
        }
    );
    assert_eq!(events[4], usage(14_432, 1, false));
    assert_eq!(events[5], usage(14_432, 1, true));
    assert_eq!(
        events[6],
        AgentEvent::Result {
            text: Some("ok\n".into()),
            structured: None,
            is_error: false,
        }
    );
    // The stderr warning about --mode plan is kept, not dropped.
    assert!(
        matches!(&events[7], AgentEvent::Unknown { raw } if raw.as_str().is_some_and(|s| s.starts_with("warning:")))
    );
    assert_eq!(billable_tokens(&events), 14_433);
}

#[test]
fn build_with_schema_writes_a_file_and_reports() {
    let Replay { events, meta } = replay("build_hello_schema");
    assert_eq!(
        meta["files_after"],
        json!(["README.md", "hello.txt", "notes.txt"])
    );
    assert_eq!(
        names(&events),
        [
            "init",
            "usage",
            "tool_started",
            "tool_finished",
            "file_changed",
            "text_delta",
            "text_delta",
            "message",
            "usage",
            "text_delta",
            "message",
            "usage",
            "tool_started",
            "tool_finished",
            "usage",
            "retry",
            "result"
        ]
    );
    assert_eq!(
        events[2],
        AgentEvent::ToolStarted {
            id: "step-2".into(),
            name: "write_to_file".into(),
            input: json!({"TargetFile": "<workdir>/hello.txt"}),
            parent: None,
        }
    );
    assert_eq!(
        events[3],
        AgentEvent::ToolFinished {
            id: "step-2".into(),
            ok: true,
            output: None,
            parent: None,
        }
    );
    assert_eq!(
        events[4],
        AgentEvent::FileChanged {
            path: "hello.txt".into(),
            kind: "add".into(),
        }
    );
    assert!(matches!(
        &events[12],
        AgentEvent::ToolStarted { id, name, .. } if id == "step-7" && name == "finish"
    ));
    assert!(matches!(
        &events[13],
        AgentEvent::ToolFinished { id, ok: true, .. } if id == "step-7"
    ));
    assert_eq!(events[14], usage(45_308, 212, true));
    assert_eq!(
        events[15],
        AgentEvent::Retry {
            reason: RetryReason::Overloaded,
            delay_ms: None,
        }
    );

    let AgentEvent::Result {
        text: Some(text),
        structured: Some(structured),
        is_error: false,
    } = &events[16]
    else {
        panic!("expected a successful result, got {:?}", events[16]);
    };
    assert!(text.starts_with("done\n"));
    let report = Report::from_value(structured).unwrap();
    assert_eq!(report.status, ReportStatus::Done);
    assert_eq!(report.files_changed, ["hello.txt"]);

    // Step usage adds up to the result's session total, so the meter doesn't double count.
    assert_eq!(billable_tokens(&events), 45_520);
}

#[test]
fn denied_command_then_capacity_error_fails_once() {
    let Replay { events, .. } = replay("build_shell");
    assert_eq!(
        names(&events),
        [
            "init",
            "usage",
            "tool_started",
            "tool_finished",
            "usage",
            "error",
            "error",
            "result"
        ]
    );
    assert_eq!(
        events[2],
        AgentEvent::ToolStarted {
            id: "step-4".into(),
            name: "run_command".into(),
            input: json!({"CommandLine": "ls -1 | wc -l"}),
            parent: None,
        }
    );
    assert!(matches!(
        &events[3],
        AgentEvent::ToolFinished { id, ok: false, output: Some(out), .. }
            if id == "step-4" && out.starts_with("permission check failed")
    ));
    assert_eq!(
        events[5],
        AgentEvent::Error {
            category: ErrorCategory::Permission,
            message: "agy denied RunCommand (command)".into(),
            retryable: false,
        }
    );
    assert_eq!(
        events[6],
        AgentEvent::Error {
            category: ErrorCategory::Overloaded,
            message: "API error (attempt 2): UNAVAILABLE (code 503): No capacity available for model gemini-3.6-flash-low on the server (error id 844557f9-77d9-49e9-810a-944f4e10d04d-2-2010)".into(),
            retryable: false,
        }
    );
    assert_eq!(
        events[7],
        AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        }
    );
    assert_eq!(billable_tokens(&events), 14_567);
}

#[test]
fn a_killed_run_still_gets_exactly_one_result() {
    for name in ["readonly_ok", "build_hello_schema", "build_shell"] {
        let dir = fixtures_dir().join(VERSION);
        let stdout = fs::read_to_string(dir.join(format!("{name}.stdout.jsonl"))).unwrap();
        let mut parser = AntigravityCli.parser();
        let mut events: Vec<AgentEvent> = stdout
            .lines()
            .flat_map(|line| parser.parse_stdout_line(line))
            .collect();
        events.extend(parser.finish(None));
        let results = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Result { .. }))
            .count();
        assert_eq!(results, 1, "{name}");
    }
}

#[test]
fn truncated_and_garbled_fixture_lines_become_unknown() {
    let dir = fixtures_dir().join(VERSION);
    let stdout = fs::read_to_string(dir.join("build_hello_schema.stdout.jsonl")).unwrap();
    let mut parser = AntigravityCli.parser();
    for line in stdout.lines() {
        let mut end = line.len() / 2;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        let cut = &line[..end];
        let events = parser.parse_stdout_line(cut);
        assert!(
            matches!(&events[..], [AgentEvent::Unknown { raw: Value::String(s) }] if s == cut),
            "{cut}"
        );
    }
}
