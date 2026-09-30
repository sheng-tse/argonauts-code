use std::fs;
use std::path::{Path, PathBuf};

use argonauts_adapter_claude_code::{ClaudeCodeAdapter, TESTED_VERSIONS};
use argonauts_core::{
    Adapter, AgentEvent, BudgetAction, BudgetLimit, BudgetMeter, Report, ReportStatus,
};
use serde_json::Value;

const VERSION: &str = "2.1.284";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_fixture(name: &str, ext: &str) -> String {
    let path = fixture_dir().join(VERSION).join(format!("{name}.{ext}"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn exit_code(name: &str) -> Option<i32> {
    let meta: Value = serde_json::from_str(&read_fixture(name, "meta.json")).unwrap();
    meta["exit_code"].as_i64().map(|code| code as i32)
}

fn replay_lines(name: &str, stdout: &str) -> Vec<AgentEvent> {
    let mut parser = ClaudeCodeAdapter.parser();
    let mut events = Vec::new();
    for line in stdout.lines() {
        events.extend(parser.parse_stdout_line(line));
    }
    for line in read_fixture(name, "stderr.txt").lines() {
        events.extend(parser.parse_stderr_line(line));
    }
    events.extend(parser.finish(exit_code(name)));
    events
}

fn replay(name: &str) -> Vec<AgentEvent> {
    replay_lines(name, &read_fixture(name, "stdout.jsonl"))
}

/// Event types in order, with runs of the same type collapsed to one.
fn shape(events: &[AgentEvent]) -> Vec<&'static str> {
    let mut shape: Vec<&'static str> = Vec::new();
    for event in events {
        if shape.last() != Some(&event.type_name()) {
            shape.push(event.type_name());
        }
    }
    shape
}

/// (input, output, cached) summed over incremental usage events.
fn incremental_totals(events: &[AgentEvent]) -> (u64, u64, u64) {
    events.iter().fold((0, 0, 0), |sum, event| match event {
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            cached_tokens,
            cumulative: false,
            ..
        } => (
            sum.0 + input_tokens,
            sum.1 + output_tokens,
            sum.2 + cached_tokens,
        ),
        _ => sum,
    })
}

/// (input, output, cached, cost) of the one cumulative usage event.
fn cumulative_totals(events: &[AgentEvent]) -> (u64, u64, u64, Option<f64>) {
    let totals: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cached_tokens,
                cost_usd,
                cumulative: true,
            } => Some((*input_tokens, *output_tokens, *cached_tokens, *cost_usd)),
            _ => None,
        })
        .collect();
    assert_eq!(totals.len(), 1, "expected one cumulative usage event");
    totals[0]
}

fn result(events: &[AgentEvent]) -> (Option<String>, Option<Value>, bool) {
    match events.last() {
        Some(AgentEvent::Result {
            text,
            structured,
            is_error,
        }) => (text.clone(), structured.clone(), *is_error),
        other => panic!("the last event should be the result, got {other:?}"),
    }
}

fn assert_clean_session(events: &[AgentEvent]) {
    match events.first() {
        Some(AgentEvent::Init {
            session_id, model, ..
        }) => {
            assert!(session_id.is_some());
            assert!(
                model.as_deref().is_some_and(|m| m.contains("haiku")),
                "{model:?}"
            );
        }
        other => panic!("the first event should be init, got {other:?}"),
    }
    let odd: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Unknown { .. } | AgentEvent::Error { .. }))
        .collect();
    assert!(odd.is_empty(), "unexpected events: {odd:?}");

    // Increments must add up to the result's totals, or the meter would count twice.
    let (input, output, cached, cost) = cumulative_totals(events);
    assert_eq!(incremental_totals(events), (input, output, cached));
    assert!(cost.is_some_and(|c| c > 0.0));
    assert!(!result(events).2);
}

#[test]
fn read_only_reply() {
    let events = replay("read-only");
    assert_eq!(
        shape(&events),
        [
            "init",
            "usage",
            "text_delta",
            "message",
            "usage",
            "quota_signal",
            "usage",
            "result"
        ]
    );
    assert_clean_session(&events);
    let AgentEvent::Init { tools, .. } = &events[0] else {
        unreachable!()
    };
    assert_eq!(tools, &["Glob", "Grep", "Read"]);
    assert!(events.contains(&AgentEvent::Message {
        text: "ok".into(),
        parent: None
    }));
    assert_eq!(result(&events).0.as_deref(), Some("ok"));
}

#[test]
fn write_creates_a_file_and_returns_a_structured_report() {
    let events = replay("write");
    assert_eq!(
        shape(&events),
        [
            "init",
            "usage",
            "text_delta",
            "message",
            "tool_started",
            "tool_finished",
            "file_changed",
            "usage",
            "quota_signal",
            "usage",
            "text_delta",
            "message",
            "usage",
            "tool_started",
            "tool_finished",
            "usage",
            "result"
        ]
    );
    assert_clean_session(&events);

    let write = events.iter().find_map(|e| match e {
        AgentEvent::ToolStarted {
            id, name, input, ..
        } if name == "Write" => Some((id.clone(), input.clone())),
        _ => None,
    });
    let (write_id, input) = write.expect("a Write call");
    assert!(input["file_path"].as_str().unwrap().ends_with("/hello.txt"));
    assert_eq!(input["content"], "hi");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished { id, ok: true, .. } if *id == write_id
    )));
    assert!(events.contains(&AgentEvent::FileChanged {
        path: "hello.txt".into(),
        kind: "add".into()
    }));

    let (_, structured, _) = result(&events);
    let report = Report::from_value(&structured.expect("structured output")).unwrap();
    assert_eq!(report.status, ReportStatus::Done);
    assert_eq!(report.files_changed, ["hello.txt"]);

    // The report also survives in the final message text, for harnesses without a schema.
    let last_message = events.iter().rev().find_map(|e| match e {
        AgentEvent::Message { text, .. } => Some(text.clone()),
        _ => None,
    });
    let from_text = Report::from_text(&last_message.unwrap()).unwrap();
    assert_eq!(from_text.status, ReportStatus::Done);
}

#[test]
fn shell_runs_one_command() {
    let events = replay("shell");
    assert_clean_session(&events);
    let bash = events.iter().find_map(|e| match e {
        AgentEvent::ToolStarted {
            id, name, input, ..
        } if name == "Bash" => Some((id.clone(), input["command"].as_str().map(str::to_owned))),
        _ => None,
    });
    let (bash_id, command) = bash.expect("a Bash call");
    assert_eq!(command.as_deref(), Some("ls"));
    let output = events.iter().find_map(|e| match e {
        AgentEvent::ToolFinished {
            id,
            ok: true,
            output,
            ..
        } if *id == bash_id => output.clone(),
        _ => None,
    });
    assert!(output.unwrap().contains("README.md"));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::FileChanged { .. }))
    );
    assert!(result(&events).0.unwrap().contains('2'));
}

#[test]
fn budget_meter_counts_each_recording_once() {
    for name in ["read-only", "write", "shell"] {
        let events = replay(name);
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(0.5), 0.85);
        for event in &events {
            assert_eq!(meter.observe(event), BudgetAction::Continue, "{name}");
        }
        let (input, output, cached, cost) = cumulative_totals(&events);
        let spend = meter.spend();
        assert_eq!(
            (spend.input_tokens, spend.output_tokens, spend.cached_tokens),
            (input, output, cached),
            "{name}"
        );
        assert_eq!(Some(spend.cost_usd), cost, "{name}");
        assert!(!spend.cost_partial, "{name}");
    }
}

#[test]
fn a_tight_token_budget_stops_mid_stream() {
    let mut meter = BudgetMeter::new(BudgetLimit::Tokens(300), 0.5);
    let actions: Vec<BudgetAction> = replay("write").iter().map(|e| meter.observe(e)).collect();
    let first_stop = actions.iter().position(|a| *a == BudgetAction::Stop);
    let last_event = actions.len() - 1;
    assert!(first_stop.is_some_and(|i| i < last_event), "{actions:?}");
}

#[test]
fn malformed_lines_become_unknown_without_disturbing_the_rest() {
    let clean = replay("read-only");
    let stdout = read_fixture("read-only", "stdout.jsonl");
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.insert(3, "{this is not json");
    lines.insert(5, r#"{"type":"from_the_future","x":1}"#);
    let events = replay_lines("read-only", &lines.join("\n"));
    let unknown: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Unknown { .. }))
        .collect();
    assert_eq!(unknown.len(), 2);
    let rest: Vec<_> = events
        .into_iter()
        .filter(|e| !matches!(e, AgentEvent::Unknown { .. }))
        .collect();
    assert_eq!(rest, clean);
}

#[test]
fn truncated_stream_still_ends_with_a_result() {
    let stdout = read_fixture("write", "stdout.jsonl");
    let cut: Vec<&str> = stdout.lines().take(40).collect();
    let mut parser = ClaudeCodeAdapter.parser();
    for line in &cut {
        parser.parse_stdout_line(line);
    }
    let end = parser.finish(Some(143));
    assert!(matches!(
        end.as_slice(),
        [
            AgentEvent::Error { .. },
            AgentEvent::Result { is_error: true, .. }
        ]
    ));
}

#[test]
fn tested_versions_match_the_recorded_fixtures() {
    let mut recorded: Vec<String> = fs::read_dir(fixture_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    recorded.sort();
    assert_eq!(recorded, TESTED_VERSIONS);
    assert_eq!(ClaudeCodeAdapter.tested_versions(), TESTED_VERSIONS);
}
