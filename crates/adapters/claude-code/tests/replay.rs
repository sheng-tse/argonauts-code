use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use argonauts_adapter_claude_code::{ClaudeCodeAdapter, TESTED_VERSIONS};
use argonauts_core::events::session::{SubagentStatus, SubagentUpdate};
use argonauts_core::limits::{QuotaSource, QuotaStatus};
use argonauts_core::{
    Adapter, AgentEvent, BudgetAction, BudgetLimit, BudgetMeter, ErrorCategory, Report,
    ReportStatus,
};
use serde_json::Value;
use time::OffsetDateTime;
use time::macros::datetime;

/// One-shot sessions with the prompt on stdin.
const ORIGINAL: &str = "2.1.284";
/// Sessions with stdin kept open, a chosen id, the status server, and a resume.
const STEERED: &str = "2.1.285";

const STATUS_TOOL: &str = "mcp__argonauts_status__argonauts_status";
const SUBAGENT_CALL: &str = "toolu_01V464aQnAvS82ukoevdYdMW";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_fixture(version: &str, name: &str, ext: &str) -> String {
    let path = fixture_dir().join(version).join(format!("{name}.{ext}"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn meta(version: &str, name: &str) -> Value {
    serde_json::from_str(&read_fixture(version, name, "meta.json")).unwrap()
}

fn exit_code(version: &str, name: &str) -> Option<i32> {
    meta(version, name)["exit_code"]
        .as_i64()
        .map(|code| code as i32)
}

fn replay_lines(version: &str, name: &str, stdout: &str) -> Vec<AgentEvent> {
    let mut parser = ClaudeCodeAdapter.parser();
    let mut events = Vec::new();
    for line in stdout.lines() {
        events.extend(parser.parse_stdout_line(line));
    }
    for line in read_fixture(version, name, "stderr.txt").lines() {
        events.extend(parser.parse_stderr_line(line));
    }
    events.extend(parser.finish(exit_code(version, name)));
    normalized(events)
}

fn replay_version(version: &str, name: &str) -> Vec<AgentEvent> {
    replay_lines(version, name, &read_fixture(version, name, "stdout.jsonl"))
}

fn replay(name: &str) -> Vec<AgentEvent> {
    replay_version(ORIGINAL, name)
}

fn replay_steered(name: &str) -> Vec<AgentEvent> {
    replay_version(STEERED, name)
}

fn every_fixture() -> Vec<(&'static str, &'static str)> {
    vec![
        (ORIGINAL, "read-only"),
        (ORIGINAL, "write"),
        (ORIGINAL, "shell"),
        (STEERED, "stream-input"),
        (STEERED, "resume"),
        (STEERED, "status-tool-error"),
    ]
}

/// A quota reading is stamped when it is parsed, so replays pin that time to compare runs.
fn normalized(events: Vec<AgentEvent>) -> Vec<AgentEvent> {
    const PARSED_AT: OffsetDateTime = datetime!(2026-09-30 00:00 UTC);
    events
        .into_iter()
        .map(|event| match event {
            AgentEvent::Quota { mut reading } => {
                reading.observed_at = PARSED_AT;
                AgentEvent::Quota { reading }
            }
            other => other,
        })
        .collect()
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

/// (input, output, cached, cost) of the last cumulative usage event. Each turn's result
/// reports the session's totals so far.
fn cumulative_totals(events: &[AgentEvent]) -> (u64, u64, u64, Option<f64>) {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cached_tokens,
                cost_usd,
                cumulative: true,
            } => Some((*input_tokens, *output_tokens, *cached_tokens, *cost_usd)),
            _ => None,
        })
        .expect("a cumulative usage event")
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

fn results(events: &[AgentEvent]) -> Vec<(Option<String>, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Result { text, is_error, .. } => Some((text.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

fn subagent_updates<'a>(events: &'a [AgentEvent], id: &str) -> Vec<&'a SubagentUpdate> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Subagent { update } if update.id == id => Some(update),
            _ => None,
        })
        .collect()
}

fn session_id(events: &[AgentEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        AgentEvent::Init { session_id, .. } => session_id.clone(),
        _ => None,
    })
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
        .filter(|e| {
            matches!(
                e,
                AgentEvent::Unknown { .. }
                    | AgentEvent::Error { .. }
                    | AgentEvent::QuotaSignal { .. }
            )
        })
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
            "context_fill",
            "text_delta",
            "message",
            "usage",
            "quota",
            "usage",
            "context_fill",
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
            "context_fill",
            "text_delta",
            "message",
            "tool_started",
            "tool_finished",
            "file_changed",
            "usage",
            "quota",
            "usage",
            "context_fill",
            "text_delta",
            "message",
            "usage",
            "context_fill",
            "tool_started",
            "tool_finished",
            "usage",
            "context_fill",
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
fn claude_partial_text_streams() {
    for (version, name) in every_fixture() {
        let events = replay_version(version, name);
        let mut streamed = String::new();
        let mut checked = 0;
        for event in &events {
            match event {
                AgentEvent::TextDelta { text, parent } => {
                    // Partial messages cover the main session only.
                    assert!(parent.is_none(), "{name}");
                    streamed.push_str(text);
                }
                AgentEvent::Message { text, parent: None } => {
                    assert_eq!(&streamed, text, "{name}: deltas then the whole message");
                    streamed.clear();
                    checked += 1;
                }
                _ => {}
            }
        }
        assert!(checked > 0, "{name} has no main-session text");
        assert!(
            streamed.is_empty(),
            "{name} streamed text never arrived whole"
        );
    }
    // Subagent text arrives whole, with no deltas before it.
    let events = replay_steered("stream-input");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Message { parent: Some(p), .. } if p == SUBAGENT_CALL
    )));
}

#[test]
fn claude_rate_limit_event_to_quota() {
    // The fullest window counts, whichever one `rateLimitType` names.
    let cases = [
        (ORIGINAL, "read-only", "five_hour", 0.17, 1790678400),
        (STEERED, "stream-input", "seven_day", 0.62, 1790820000),
    ];
    for (version, name, window, used, resets_at) in cases {
        let events = replay_version(version, name);
        let readings: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Quota { reading } => Some(reading),
                _ => None,
            })
            .collect();
        let [reading] = readings.as_slice() else {
            panic!("{name}: expected one reading, got {readings:?}");
        };
        assert_eq!(reading.vendor.0, "claude-code");
        assert_eq!(reading.status, QuotaStatus::Allowed);
        assert_eq!(reading.used, Some(used), "{name}");
        assert_eq!(
            reading.resets_at.map(|at| at.unix_timestamp()),
            Some(resets_at)
        );
        assert_eq!(reading.window.as_deref(), Some(window), "{name}");
        assert_eq!(reading.source, QuotaSource::RateLimitEvent);
        let detail = reading.detail.as_ref().unwrap();
        assert_eq!(detail["rateLimitType"], "five_hour");
        assert_eq!(detail["unifiedWindows"][window]["utilization"], used);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::QuotaSignal { .. }))
        );
    }
}

#[test]
fn claude_status_tool_listed_in_init() {
    let events = replay_steered("stream-input");
    let AgentEvent::Init { tools, .. } = &events[0] else {
        panic!("the first event should be init");
    };
    assert!(tools.iter().any(|t| t == STATUS_TOOL), "{tools:?}");
    let call = events.iter().find_map(|e| match e {
        AgentEvent::ToolStarted { id, name, .. } if name == STATUS_TOOL => Some(id.clone()),
        _ => None,
    });
    let call = call.expect("the agent called the status tool");
    // The generated settings allow it, so neither dontAsk nor the prompt setting denies it.
    let output = events.iter().find_map(|e| match e {
        AgentEvent::ToolFinished {
            id,
            ok: true,
            output,
            ..
        } if *id == call => output.clone(),
        _ => None,
    });
    assert_eq!(
        output.as_deref(),
        Some("Run r-01: card-001 is building. No messages for you.")
    );

    let stdout = read_fixture(STEERED, "stream-input", "stdout.jsonl");
    let init: Value = serde_json::from_str(stdout.lines().nth(1).unwrap()).unwrap();
    assert_eq!(init["subtype"], "init");
    assert_eq!(init["mcp_servers"][0]["name"], "argonauts_status");
    assert_eq!(init["mcp_servers"][0]["status"], "connected");
    assert!(init["mcp_server_errors"].is_null());

    let args = &meta(STEERED, "stream-input")["args"];
    let args: Vec<&str> = args
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mcp = args.iter().position(|a| *a == "--mcp-config").unwrap();
    assert_eq!(args[mcp + 2], "--strict-mcp-config");
}

#[test]
fn a_rejected_mcp_config_replays_as_adapter_broken() {
    let events = replay_steered("status-tool-error");
    assert_eq!(shape(&events)[..2], ["init", "error"]);
    let AgentEvent::Error {
        category, message, ..
    } = &events[1]
    else {
        unreachable!()
    };
    assert_eq!(*category, ErrorCategory::Cli);
    assert!(message.starts_with("adapter broken"), "{message}");
    assert!(message.contains("argonauts_status"), "{message}");
    // Claude Code runs on without the server and exits cleanly, so the init decides the end:
    // the clean result turns into an error, with the broken config as the turn's last error.
    let stdout = read_fixture(STEERED, "status-tool-error", "stdout.jsonl");
    let recorded: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(recorded["type"], "result");
    assert_eq!(recorded["is_error"], false);
    assert_eq!(exit_code(STEERED, "status-tool-error"), Some(0));
    assert_eq!(result(&events), (Some("ok".into()), None, true));
    let last_error = events.iter().rev().find_map(|e| match e {
        AgentEvent::Error {
            category,
            message,
            retryable,
        } => Some((*category, message.as_str(), *retryable)),
        _ => None,
    });
    let (category, message, retryable) = last_error.unwrap();
    assert_eq!(category, ErrorCategory::Cli);
    assert!(!retryable);
    assert!(message.contains("argonauts_status"), "{message}");
}

#[test]
fn claude_subagent_parent_ids() {
    let events = replay_steered("stream-input");
    let spawn = events.iter().find_map(|e| match e {
        AgentEvent::ToolStarted {
            id, name, parent, ..
        } if name == "Agent" => Some((id.clone(), parent.clone())),
        _ => None,
    });
    assert_eq!(spawn, Some((SUBAGENT_CALL.to_owned(), None)));

    let updates = subagent_updates(&events, SUBAGENT_CALL);
    let statuses: Vec<_> = updates.iter().map(|u| u.status).collect();
    assert_eq!(statuses.first(), Some(&SubagentStatus::Started));
    assert_eq!(statuses.last(), Some(&SubagentStatus::Done));
    assert!(statuses.contains(&SubagentStatus::Running));
    let last = updates.last().unwrap();
    assert_eq!(last.label.as_deref(), Some("general-purpose"));
    assert!(last.parent.is_none() && last.steps_visible);
    assert_eq!(
        last.last_message.as_deref(),
        Some("The first line of README.md is: `# Throwaway`")
    );

    // The subagent's own steps carry its call's id as their parent.
    let read = events.iter().find_map(|e| match e {
        AgentEvent::ToolStarted {
            id, name, parent, ..
        } if name == "Read" => Some((id.clone(), parent.clone())),
        _ => None,
    });
    let (read_id, read_parent) = read.expect("the subagent read the file");
    assert_eq!(read_parent.as_deref(), Some(SUBAGENT_CALL));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished { id, parent: Some(p), ok: true, .. }
            if *id == read_id && p == SUBAGENT_CALL
    )));
    // The background launch returns at once without ending the subagent.
    let first_done = updates
        .iter()
        .position(|u| u.status == SubagentStatus::Done)
        .unwrap();
    assert_eq!(first_done, updates.len() - 1);
}

#[test]
fn context_fill_from_each_vendor() {
    for (version, name) in every_fixture() {
        let events = replay_version(version, name);
        let fills: Vec<(u64, Option<u64>)> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ContextFill { used, size } => Some((*used, *size)),
                _ => None,
            })
            .collect();
        assert!(!fills.is_empty(), "{name}");
        let first_fill = events
            .iter()
            .position(|e| matches!(e, AgentEvent::ContextFill { .. }))
            .unwrap();
        let first_result = events
            .iter()
            .position(|e| matches!(e, AgentEvent::Result { .. }))
            .unwrap();
        assert!(
            first_fill < first_result,
            "{name}: the fill comes before the end"
        );
        // The window comes from the first result's modelUsage.
        assert_eq!(fills.last().unwrap().1, Some(200_000), "{name}");
        for (used, size) in &fills {
            assert!(*used > 0 && size.is_none_or(|s| *used < s), "{name}");
        }
    }
    // The context holds input plus cache tokens: 10 + 4563 + 1798 in the read-only reply.
    let read_only = replay("read-only");
    assert!(read_only.contains(&AgentEvent::ContextFill {
        used: 6371,
        size: None
    }));
    // A resumed session starts from everything before it.
    let resumed = replay_steered("resume");
    let first = resumed.iter().find_map(|e| match e {
        AgentEvent::ContextFill { used, .. } => Some(*used),
        _ => None,
    });
    assert!(first.unwrap() > 20_000);
}

#[test]
fn stream_input_takes_a_second_message_mid_run() {
    let events = replay_steered("stream-input");
    assert_clean_session_but_totals(&events);
    // A subagent's messages carry the usage from their start, so its output shows in full only
    // in the result's totals, which replace the increments.
    let (input, output, cached, _) = cumulative_totals(&events);
    let (run_in, run_out, run_cached) = incremental_totals(&events);
    assert_eq!((run_in, run_cached), (input, cached));
    assert!(run_out < output);
    let meta = meta(STEERED, "stream-input");
    assert_eq!(meta["keep_input_open"], true);
    let sent: Vec<Value> = meta["stdin"]
        .as_str()
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(sent.len(), 2);
    for message in &sent {
        assert_eq!(message["type"], "user");
        assert_eq!(message["message"]["role"], "user");
    }
    let follow_up = sent[1]["message"]["content"].as_str().unwrap();
    let line = ClaudeCodeAdapter.input_line(follow_up).unwrap();
    assert!(
        line.ends_with('\n') && line.matches('\n').count() == 1,
        "{line:?}"
    );
    // Key order depends on serde_json's `preserve_order`, which another crate in the
    // workspace turns on, so compare the values.
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), sent[1]);
    // 2.1.285 delivered the mid-turn message with the next tool result, not as its own turn.
    // The second turn here came from the background subagent finishing.
    let turns = results(&events);
    assert_eq!(turns.len(), 2);
    assert!(turns.iter().all(|(_, is_error)| !is_error));
    let inits = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Init { .. }))
        .count();
    assert_eq!(inits, 2, "each turn starts with its own init");
    assert_eq!(session_id(&events).as_deref(), meta["session_id"].as_str());
}

#[test]
fn resume_continues_the_same_session_with_every_flag() {
    let first = replay_steered("stream-input");
    let resumed = replay_steered("resume");
    assert_clean_session_but_totals(&resumed);
    assert_eq!(session_id(&resumed), session_id(&first));

    // Totals after --resume include the earlier runs, so the meter adds only the difference.
    let (input, output, cached, cost) = cumulative_totals(&resumed);
    let (before_in, before_out, before_cached, before_cost) = cumulative_totals(&first);
    let (run_in, run_out, run_cached) = incremental_totals(&resumed);
    assert_eq!(
        (input, output, cached),
        (
            before_in + run_in,
            before_out + run_out,
            before_cached + run_cached
        )
    );
    assert!(cost.unwrap() > before_cost.unwrap());

    let flags = |name: &str| -> BTreeSet<String> {
        meta(STEERED, name)["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|a| a.starts_with('-'))
            .map(str::to_owned)
            .collect()
    };
    let mut launch = flags("stream-input");
    let mut resume = flags("resume");
    assert!(launch.remove("--session-id"));
    assert!(resume.remove("--resume"));
    assert_eq!(resume, launch);
    assert_eq!(
        meta(STEERED, "resume")["resume_session"],
        meta(STEERED, "stream-input")["session_id"]
    );
}

fn assert_clean_session_but_totals(events: &[AgentEvent]) {
    assert!(matches!(events.first(), Some(AgentEvent::Init { .. })));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Unknown { .. } | AgentEvent::Error { .. }))
    );
    assert!(!result(events).2);
}

#[test]
fn budget_meter_counts_each_recording_once() {
    let one_session = [
        (ORIGINAL, "read-only"),
        (ORIGINAL, "write"),
        (ORIGINAL, "shell"),
        (STEERED, "stream-input"),
        (STEERED, "status-tool-error"),
    ];
    for (version, name) in one_session {
        let events = replay_version(version, name);
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
    let stdout = read_fixture(ORIGINAL, "read-only", "stdout.jsonl");
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.insert(3, "{this is not json");
    lines.insert(5, r#"{"type":"from_the_future","x":1}"#);
    let events = replay_lines(ORIGINAL, "read-only", &lines.join("\n"));
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
    let stdout = read_fixture(ORIGINAL, "write", "stdout.jsonl");
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
    for (version, name) in every_fixture() {
        assert_eq!(meta(version, name)["cli_version"], version, "{name}");
    }
}

#[test]
fn fixtures_hold_no_local_paths_or_keys() {
    let leaks = [
        "/Users/",
        "/home/",
        "/private/",
        "/var/folders/",
        "sk-ant-",
        "sk-proj-",
        "ghp_",
        "github_pat_",
        "AIza",
    ];
    for version in fs::read_dir(fixture_dir()).unwrap() {
        for file in fs::read_dir(version.unwrap().path()).unwrap() {
            let path = file.unwrap().path();
            let text = fs::read_to_string(&path).unwrap();
            for leak in leaks {
                assert!(!text.contains(leak), "{} contains {leak:?}", path.display());
            }
        }
    }
}
