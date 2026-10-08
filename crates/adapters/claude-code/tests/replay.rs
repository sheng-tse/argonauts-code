use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use argonauts_adapter_claude_code::{ClaudeCodeAdapter, TESTED_VERSIONS};
use argonauts_core::events::context::{ContextDetail, ContextPart};
use argonauts_core::events::session::{
    SubagentKind, SubagentStatus, SubagentUpdate, VendorAgentState, VendorWorkflow,
};
use argonauts_core::limits::{QuotaSource, QuotaStatus};
use argonauts_core::screen::ScreenChange;
use argonauts_core::{
    Adapter, AgentEvent, BudgetAction, BudgetLimit, BudgetMeter, ErrorCategory, MessageStage,
    Report, ReportStatus, ReportedCommand, SessionRequest, SessionRole,
};
use serde_json::Value;
use time::OffsetDateTime;
use time::macros::datetime;

/// One-shot sessions with the prompt on stdin.
const ORIGINAL: &str = "2.1.284";
/// Sessions with stdin kept open, a chosen id, the status server, and a resume.
const STEERED: &str = "2.1.285";
/// A workflow Claude Code ran itself, recorded by hand without partial messages, so it stays
/// out of `every_fixture`.
const WORKFLOWS: &str = "2.1.289";
const WORKFLOW_CALL: &str = "toolu_01T9VznHYcM7UjRKWHQ4QH2c";
/// Direct solo: calls asked through the permission prompt tool, and a read-only estimate
/// resumed with every tool.
const DIRECT: &str = "2.1.289";

/// Live calls with stream-json both ways and `initialize` first, recorded without partial
/// messages, so they stay out of `every_fixture` too.
const TWO_WAY: &str = "2.1.289";
const TWO_WAY_CALLS: &[&str] = &[
    "agents_md",
    "sync_off",
    "setting_sources_user",
    "you_should_know",
    "checkup",
];

/// The version installed when this was last checked, recorded as `ORIGINAL` was.
const INSTALLED: &str = "2.1.293";

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
        (DIRECT, "permission_prompt_tool"),
        (DIRECT, "readonly_estimate"),
        (DIRECT, "direct_resume"),
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

/// The installed version reads as 2.1.284 did: its new `session_title_changed` line is read and
/// set aside, and an answer and a shell call come through whole.
#[test]
fn the_installed_version_reads_an_answer_and_a_shell_call() {
    let answer = replay_version(INSTALLED, "read-only");
    assert_clean_session(&answer);
    assert_eq!(result(&answer).0.as_deref(), Some("ok"));

    let shell = replay_version(INSTALLED, "shell");
    assert_clean_session(&shell);
    let bash = shell.iter().find_map(|e| match e {
        AgentEvent::ToolStarted { id, name, .. } if name == "Bash" => Some(id.clone()),
        _ => None,
    });
    let bash = bash.expect("a Bash call");
    assert!(
        shell
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolFinished { id, ok: true, .. } if *id == bash))
    );
    assert!(result(&shell).0.unwrap().contains('2'));
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
        // The fullest comes first, and every other account window after it.
        let Some(reading) = readings.first() else {
            panic!("{name}: expected a reading");
        };
        let detail = reading.detail.as_ref().unwrap();
        let windows = detail["unifiedWindows"].as_object().unwrap();
        assert_eq!(readings.len(), windows.len(), "{name}: {readings:?}");
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
fn every_window_of_a_rate_limit_event_is_a_reading_of_its_own() {
    let events = replay_version(TWO_WAY, "checkup");
    let readings: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Quota { reading } => Some(reading),
            _ => None,
        })
        .collect();
    let windows: Vec<_> = readings
        .iter()
        .map(|r| {
            (
                r.window.as_deref().unwrap(),
                r.used.unwrap(),
                r.resets_at.unwrap().unix_timestamp(),
                r.status,
            )
        })
        .collect();
    assert_eq!(
        windows,
        [
            ("five_hour", 0.54, 1791097200, QuotaStatus::Allowed),
            ("seven_day", 0.49, 1791424800, QuotaStatus::Allowed),
        ]
    );
    // The plan comes from the initialize reply, and nothing else about the account.
    assert!(events.contains(&AgentEvent::Plan {
        plan: "<plan>".into()
    }));
}

/// Checked live: `get_context_usage` with `summary` after one turn, and on a resumed session
/// before its first.
#[test]
fn a_recorded_context_usage_reply_becomes_a_breakdown() {
    let asked: Value = serde_json::from_str(
        &ClaudeCodeAdapter
            .context_usage_line(ContextDetail::Summary)
            .unwrap(),
    )
    .unwrap();
    for name in ["context_usage_summary", "context_usage_resumed"] {
        // The request the adapter writes is the one recorded.
        let stdin = read_fixture(TWO_WAY, name, "stdin.jsonl");
        let recorded: Value = stdin
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|line| line["request"]["subtype"] == "get_context_usage")
            .unwrap();
        assert_eq!(recorded["request"], asked["request"], "{name}");

        let events = replay_version(TWO_WAY, name);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::Unknown { .. })),
            "{name}"
        );
        let at = events
            .iter()
            .position(|e| matches!(e, AgentEvent::ContextBreakdown { .. }))
            .unwrap_or_else(|| panic!("{name}: no breakdown"));
        let AgentEvent::ContextBreakdown {
            categories,
            auto_compact_at,
            window,
            model,
            memory_files,
            mcp_tools,
            skills,
            ..
        } = &events[at]
        else {
            unreachable!()
        };
        let parts: Vec<(ContextPart, u64)> =
            categories.iter().map(|c| (c.part, c.tokens)).collect();
        // The deferred system tools are out of the window, and the buffer is held past the
        // compaction point.
        assert_eq!(
            parts,
            [
                (ContextPart::SystemPrompt, 6888),
                (ContextPart::Tools, 15135),
                (ContextPart::MemoryFiles, 17),
                (ContextPart::Skills, 1969),
                (ContextPart::Messages, 1),
                (ContextPart::Free, 142990),
                (ContextPart::Buffer, 33000),
            ],
            "{name}"
        );
        assert_eq!((*window, *auto_compact_at), (200_000, Some(167_000)));
        assert_eq!(model.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert_eq!(memory_files.len(), 1);
        assert_eq!(memory_files[0].source.as_deref(), Some("Project"));
        assert!(mcp_tools.is_empty());
        assert_eq!(skills.len(), 23);
        // Answering costs no model call.
        assert!(
            !events[at..]
                .iter()
                .any(|e| matches!(e, AgentEvent::Usage { .. })),
            "{name}"
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
    // The background launch returns at once without ending the subagent. It ends when its
    // task does, and all that follows is the total the task counted for it.
    let first_done = updates
        .iter()
        .position(|u| u.status == SubagentStatus::Done)
        .unwrap();
    assert_eq!(first_done, updates.len() - 2);
    assert_eq!(updates[first_done].tokens, Some(9_986));
    assert_eq!(
        (last.status, last.tokens),
        (SubagentStatus::Done, Some(10_215))
    );
}

#[test]
fn context_fill_from_each_vendor() {
    for (version, name) in every_fixture() {
        let events = replay_version(version, name);
        let fills: Vec<(u64, Option<u64>)> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ContextFill { used, size, .. } => Some((*used, *size)),
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
        size: None,
        sent: None,
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
        (DIRECT, "permission_prompt_tool"),
        (DIRECT, "readonly_estimate"),
        (DIRECT, "direct_resume"),
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

fn workflows(events: &[AgentEvent]) -> Vec<&VendorWorkflow> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Workflow { workflow } => Some(&**workflow),
            _ => None,
        })
        .collect()
}

/// Claude Code 2.1.289 runs a workflow script in the background and reports it through
/// `system/task_*` events. The replay mirrors it as it goes: the script's own name,
/// description, and phases first, then each agent under its phase, with its model, tokens, and
/// time, whatever the names.
#[test]
fn claude_runs_its_own_workflow_and_the_replay_mirrors_it() {
    let events = replay_version(WORKFLOWS, "workflow");
    assert_eq!(meta(WORKFLOWS, "workflow")["cli_version"], WORKFLOWS);
    let seen = workflows(&events);
    assert!(seen.len() >= 4, "{seen:#?}");

    let first = seen[0];
    assert_eq!(first.id, "w4d273qmn");
    assert_eq!(first.call.as_deref(), Some(WORKFLOW_CALL));
    assert_eq!(first.name, "probe");
    assert_eq!(
        first.description,
        "Two-phase workflow that asks and answers with single words"
    );
    let titles: Vec<&str> = first.phases.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["Ask", "Answer"]);
    assert!(first.agents.is_empty());
    assert_eq!(first.status, SubagentStatus::Running);

    // The first agent runs in the first phase before the second starts.
    let asking = seen
        .iter()
        .find(|workflow| !workflow.agents.is_empty())
        .unwrap_or_else(|| panic!("no agent ever showed"));
    let ask = &asking.agents[0];
    assert_eq!((ask.label.as_str(), ask.phase), ("ask:one", Some(1)));
    assert_eq!(ask.state, VendorAgentState::Running);
    assert_eq!(ask.model.as_deref(), Some("claude-haiku-4-5-20251001"));
    assert_eq!(asking.count(1), Some((0, 1)));
    assert_eq!(asking.count(2), None);

    let last = seen[seen.len() - 1];
    assert_eq!(last.status, SubagentStatus::Done);
    assert_eq!(last.tokens, 20_626);
    assert!(
        last.summary
            .as_deref()
            .is_some_and(|s| s.contains("completed")),
        "{:?}",
        last.summary
    );
    /// Label, phase, state, tokens, and the start of its answer.
    type Row<'a> = (&'a str, Option<u32>, VendorAgentState, u64, Option<&'a str>);
    let agents: Vec<Row<'_>> = last
        .agents
        .iter()
        .map(|agent| {
            (
                agent.label.as_str(),
                agent.phase,
                agent.state,
                agent.tokens,
                agent.result.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        agents,
        [
            (
                "ask:one",
                Some(1),
                VendorAgentState::Done,
                10_311,
                Some("Ready")
            ),
            (
                "answer:one",
                Some(2),
                VendorAgentState::Done,
                10_315,
                Some("success")
            ),
        ]
    );
    assert_eq!((last.count(1), last.count(2)), (Some((1, 1)), Some((1, 1))));
    assert_eq!(last.agents[0].duration_ms, 3_266);

    // It's a subagent too, marked as a workflow, whose agents don't stream their own steps.
    let updates = subagent_updates(&events, WORKFLOW_CALL);
    let end = updates
        .last()
        .unwrap_or_else(|| panic!("no subagent update"));
    assert_eq!(end.kind, SubagentKind::Workflow);
    assert_eq!(end.label.as_deref(), Some("probe"));
    assert_eq!(end.status, SubagentStatus::Done);
    assert!(!end.steps_visible);

    // The launch and the answer after the workflow finished are two turns, nothing odd.
    let turns = results(&events);
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert!(
        turns[1]
            .0
            .as_deref()
            .is_some_and(|text| text.contains("Ready"))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Unknown { .. } | AgentEvent::Error { .. })),
        "{events:#?}"
    );
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

/// A stream cut mid-workflow, as when its session is stopped, paused, killed for a stall, or
/// out of budget: the workflow ends with it, and so does every agent still going, so nothing
/// stays running in the panel or the view.
#[test]
fn a_workflow_cut_off_mid_stream_ends_with_its_session() {
    let whole = read_fixture(WORKFLOWS, "workflow", "stdout.jsonl");
    let cut: Vec<&str> = whole.lines().take(20).collect();
    let events = replay_lines(WORKFLOWS, "workflow", &cut.join("\n"));
    let seen = workflows(&events);
    let last = seen.last().unwrap_or_else(|| panic!("no workflow showed"));
    assert_eq!(last.id, "w4d273qmn");
    assert_eq!(last.status, SubagentStatus::Failed);
    assert!(
        last.summary
            .as_deref()
            .is_some_and(|summary| summary.contains("session ended")),
        "{last:#?}"
    );
    assert!(
        last.agents.iter().all(|agent| matches!(
            agent.state,
            VendorAgentState::Done | VendorAgentState::Failed
        )),
        "{last:#?}"
    );
    assert!(
        seen.iter()
            .rev()
            .skip(1)
            .all(|workflow| workflow.status == SubagentStatus::Running)
    );
}

/// A report that comes before the workflow's start still shows the workflow, and the start's
/// name and phases join it when they come.
#[test]
fn a_report_before_its_start_still_shows_the_workflow() {
    let whole = read_fixture(WORKFLOWS, "workflow", "stdout.jsonl");
    let mut lines: Vec<&str> = whole.lines().collect();
    // Line 14 is the first progress report; line 11 its task's start.
    let started = lines.remove(10);
    lines.insert(13, started);
    let events = replay_lines(WORKFLOWS, "workflow", &lines.join("\n"));
    let seen = workflows(&events);
    let first = seen.first().unwrap_or_else(|| panic!("no workflow showed"));
    assert_eq!(first.id, "w4d273qmn");
    assert!(
        !first.agents.is_empty(),
        "the early report's agent was kept: {first:#?}"
    );
    let named = seen
        .iter()
        .find(|workflow| workflow.name == "probe")
        .unwrap_or_else(|| panic!("the start never named it: {seen:#?}"));
    assert!(!named.agents.is_empty());
    let last = seen.last().unwrap_or_else(|| panic!("no workflow showed"));
    assert_eq!(last.status, SubagentStatus::Done);
}

/// 2.1.289's new lines (`command_lifecycle`, `control_response`, task events, `ui_invalidate`,
/// `tool_progress`) are all read, and `init` and `initialize` say what the CLI is.
#[test]
fn two_way_calls_parse_with_nothing_unknown() {
    for name in TWO_WAY_CALLS {
        assert_eq!(meta(TWO_WAY, name)["cli_version"], TWO_WAY, "{name}");
        let events = replay_version(TWO_WAY, name);
        let unknown: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Unknown { .. }))
            .collect();
        assert!(unknown.is_empty(), "{name}: {unknown:?}");

        // initialize's reply comes first: the commands, then the models.
        assert!(
            matches!(events.first(), Some(AgentEvent::CommandsChanged { .. })),
            "{name}: initialize's reply should come first"
        );
        let Some(AgentEvent::ModelsReported { models }) = events.get(1) else {
            panic!("{name}: initialize's models should follow its commands");
        };
        let runs = |value: &str| {
            models
                .iter()
                .find(|m| m.value == value)
                .and_then(|m| m.resolved.as_deref())
        };
        assert_eq!(runs("default"), Some("claude-opus-5-5"), "{name}");
        assert_eq!(runs("sonnet"), Some("claude-sonnet-5-5"), "{name}");

        let Some(AgentEvent::Init {
            version,
            capabilities,
            plugins,
            ..
        }) = events.iter().find(|e| matches!(e, AgentEvent::Init { .. }))
        else {
            panic!("{name}: no init");
        };
        assert_eq!(version.as_deref(), Some(TWO_WAY), "{name}");
        assert!(
            capabilities.iter().any(|c| c == "interrupt_send_now_v1"),
            "{name}: {capabilities:?}"
        );
        assert!(
            plugins.iter().any(|p| p == "cc-plugin-agents-md"),
            "{name}: {plugins:?}"
        );
        // Each turn's reading allowed, so nothing waits for the end.
        assert!(
            events.iter().all(|e| match e {
                AgentEvent::Quota { reading } => !reading.wrap_up,
                _ => true,
            }),
            "{name}"
        );
    }

    // The audit ran out of its budget before its report.
    let checkup = replay_version(TWO_WAY, "checkup");
    assert!(
        checkup.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                category: ErrorCategory::Budget,
                ..
            }
        )),
        "{checkup:?}"
    );
    // Two turns in one process, each with its own result.
    assert_eq!(
        results(&replay_version(TWO_WAY, "setting_sources_user")).len(),
        2
    );
}

/// The live check of the isolation keys, with a test plugin loaded by `--plugin-dir`.
const ISOLATION: &str = "2.1.289";

/// Replayed as an isolated session's, whose parser checks what the session loaded.
fn replay_isolated(version: &str, name: &str) -> Vec<AgentEvent> {
    let request = SessionRequest {
        isolated: true,
        ..SessionRequest::default()
    };
    let mut parser = ClaudeCodeAdapter.session_parser(&request);
    read_fixture(version, name, "stdout.jsonl")
        .lines()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect()
}

fn leaks(events: &[AgentEvent]) -> Vec<(Vec<String>, Vec<String>)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::IsolationLeak {
                plugins, skills, ..
            } => Some((plugins.clone(), skills.clone())),
            _ => None,
        })
        .collect()
}

/// `--setting-sources user` alone, as isolated sessions ran before: your marketplace plugins
/// and the synced skills load, and the check names them.
#[test]
fn an_isolated_session_without_the_keys_reports_your_plugins_and_synced_skills() {
    for (version, name) in [
        (TWO_WAY, "setting_sources_user"),
        (DIRECT, "readonly_estimate"),
    ] {
        let found = leaks(&replay_isolated(version, name));
        let [(plugins, skills)] = found.as_slice() else {
            panic!("{name}: expected one leak, got {found:?}");
        };
        assert!(
            plugins.contains(&"example-plugin-a".to_owned()),
            "{name}: {plugins:?}"
        );
        assert!(
            !plugins
                .iter()
                .any(|plugin| plugin.starts_with("cc-plugin-")),
            "{name}: the kept built-ins aren't a leak: {plugins:?}"
        );
        assert_eq!(skills.len(), 9, "{name}: {skills:?}");
        assert!(
            skills
                .iter()
                .all(|skill| skill.starts_with("anthropic-skills:"))
        );
        // The same stream in a session that isn't isolated is just your setup.
        assert!(leaks(&replay_version(version, name)).is_empty(), "{name}");
    }
}

/// With the keys, Claude Code loaded only the kept built-ins and the plugin the check passed
/// with `--plugin-dir`, which Argonauts never does. Its mod never ran: no pushes, and the model
/// saw neither code word.
#[test]
fn the_isolation_keys_keep_your_plugins_mods_and_synced_skills_out() {
    let events = replay_isolated(ISOLATION, "isolation_keys");
    assert_eq!(
        leaks(&events),
        [(vec!["argo-probe".to_owned()], Vec::new())]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ModOutput { .. })),
        "the probe's mod pushed something"
    );
    // The message's lifecycle closes after the result, so it isn't the last event.
    let (text, is_error) = results(&events).pop().expect("a result");
    let text = text.unwrap_or_default();
    assert!(!is_error);
    assert!(
        !text.contains("KESTREL") && !text.contains("HERON"),
        "{text}"
    );
}

/// The settings the adapter writes for an isolated read-only session hold every key the live
/// check ran with, given the same plugins enabled in your settings.
#[test]
fn the_adapter_writes_the_settings_the_isolation_check_ran_with() {
    let recorded = &meta(ISOLATION, "isolation_keys")["files"][0]["contents"];
    let recorded: Value = serde_json::from_str(recorded.as_str().unwrap()).unwrap();
    let yours = tempfile::tempdir().unwrap();
    let enabled: serde_json::Map<String, Value> = recorded["enabledPlugins"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|id| !id.ends_with("@builtin"))
        .map(|id| (id.clone(), Value::Bool(true)))
        .collect();
    let settings = serde_json::json!({ "enabledPlugins": enabled });
    fs::write(yours.path().join("settings.json"), settings.to_string()).unwrap();
    let request = SessionRequest {
        workdir: PathBuf::from("/tmp/worktree"),
        prompt: "Review the change".into(),
        role: SessionRole::ReadOnly,
        isolated: true,
        tool_sandbox_off: true,
        env_set: vec![(
            "CLAUDE_CONFIG_DIR".into(),
            yours.path().to_string_lossy().into_owned(),
        )],
        ..SessionRequest::default()
    };
    let spec = ClaudeCodeAdapter.command(&request).unwrap();
    let flag = spec
        .args
        .iter()
        .position(|arg| arg == "--settings")
        .unwrap();
    let written: Value = serde_json::from_str(&spec.args[flag + 1]).unwrap();
    for key in [
        "crossSessionInbound",
        "attribution",
        "sandbox",
        "disableAllHooks",
        "syncClaudeAiSkills",
        "syncClaudeAiPlugins",
    ] {
        assert_eq!(written[key], recorded[key], "{key}");
    }
    for (id, on) in recorded["enabledPlugins"].as_object().unwrap() {
        assert_eq!(&written["enabledPlugins"][id], on, "{id}");
    }
    for flag in ["--strict-mcp-config", "--setting-sources"] {
        assert!(spec.args.iter().any(|arg| arg == flag), "{flag}");
    }
}

/// 2.1.289 doesn't flag an rm whose empty variable can't reach a top-level folder, so the
/// check's command ran under both a Bash allow rule and a hook that allowed it. Neither turned
/// into an error or a denial.
#[test]
fn an_rm_claude_code_doesnt_flag_runs_with_no_denial() {
    for name in ["flagged_rm_allow_rule", "flagged_rm_hook"] {
        let events = replay_version(ISOLATION, name);
        assert!(
            !events.iter().any(|event| matches!(
                event,
                AgentEvent::Error {
                    category: ErrorCategory::Permission,
                    ..
                }
            )),
            "{name}"
        );
        let finished = events.iter().find_map(|event| match event {
            AgentEvent::ToolFinished { ok, output, .. } => Some((*ok, output.clone())),
            _ => None,
        });
        let (ok, output) = finished.unwrap_or_else(|| panic!("{name}: no tool finished"));
        assert!(ok, "{name}: {output:?}");
        assert!(
            !output.unwrap_or_default().contains("turned this down"),
            "{name}"
        );
    }
}

/// Two recorded sessions read into what the screen needs: each message's way through the
/// queue, the mod's command in the CLI's own list, and You should know as one line and three
/// buttons with the handles a press sends back.
#[test]
fn messages_commands_and_the_notice_read() {
    let events = replay_version(TWO_WAY, "you_should_know");
    let states: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageState { uuid, state } => {
                Some((uuid.get(..8).unwrap_or(uuid), *state))
            }
            _ => None,
        })
        .collect();
    // The recording's send now went with an id of its own, not the message's.
    assert!(
        states.contains(&("1", MessageStage::SendingNow)),
        "{states:?}"
    );
    assert!(
        states.contains(&("bdeb6d64", MessageStage::Started)),
        "{states:?}"
    );
    let drawn: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ScreenChanged {
                change: ScreenChange::Drawn { drawn },
            } if !drawn.is_empty() => Some(drawn),
            _ => None,
        })
        .collect();
    let first = drawn.first().expect("a heads-up drew");
    assert!(
        first.rows[0].text().contains("Heads up · Release 1.4.0"),
        "{first:?}"
    );
    let labels: Vec<_> = first.buttons.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels, ["Learn more", "Knew this already", "Dismiss"]);
    assert!(
        first
            .buttons
            .iter()
            .all(|b| b.plugin == "cc-plugin-you-should-know")
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ScreenChanged {
            change: ScreenChange::Stale
        }
    )));

    let events = replay_version(TWO_WAY, "mod_command");
    let lists: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::CommandsChanged { commands } => Some(commands),
            _ => None,
        })
        .collect();
    let has = |list: &Vec<ReportedCommand>, name: &str| {
        list.iter()
            .any(|command| command.name == name && !command.builtin)
    };
    assert!(
        has(lists[0], "probe-cmd"),
        "the initialize reply lists the mod's command"
    );
    assert!(
        lists.iter().skip(1).any(|list| has(list, "probe-late")),
        "a command registered mid-session comes with commands_changed"
    );
}

/// Checked live: send now moving a command to the background, across resumes of one session.
const SEND_NOW: &str = "2.1.289";

fn background_lists(events: &[AgentEvent]) -> Vec<(Vec<String>, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::BackgroundTasks { running, ended } => Some((running.clone(), *ended)),
            _ => None,
        })
        .collect()
}

fn position(events: &[AgentEvent], wanted: impl Fn(&AgentEvent) -> bool) -> usize {
    events.iter().position(wanted).expect("the event")
}

/// The command send now moved stays listed past the turn's result, until Claude Code says it
/// ended. Here that came only after the input closed, which killed it.
#[test]
fn a_command_moved_to_the_background_stays_listed_past_the_result() {
    let events = replay_version(SEND_NOW, "send_now_resume");
    assert_eq!(
        background_lists(&events),
        [(vec!["bmvwwjv0b".to_owned()], false), (Vec::new(), true)]
    );
    let result_at = position(&events, |e| matches!(e, AgentEvent::Result { .. }));
    let ended_at = position(&events, |e| {
        matches!(e, AgentEvent::BackgroundTasks { ended: true, .. })
    });
    assert!(result_at < ended_at);
    assert_eq!(results(&events), [(Some("PONG".to_owned()), false)]);
}

/// With the input left open, the command ran to its end, and Claude Code told the model in a
/// turn of its own, with no message of yours, whose result came second.
#[test]
fn a_background_command_that_ends_between_turns_brings_a_turn_of_its_own() {
    let events = replay_version(SEND_NOW, "send_now_resume_open");
    assert_eq!(
        background_lists(&events),
        [(vec!["bxzv2d82a".to_owned()], false), (Vec::new(), true)]
    );
    let ended_at = position(&events, |e| {
        matches!(e, AgentEvent::BackgroundTasks { ended: true, .. })
    });
    let first_result = position(&events, |e| matches!(e, AgentEvent::Result { .. }));
    assert!(first_result < ended_at);
    assert_eq!(
        results(&events),
        [
            (Some("PONG".to_owned()), false),
            (
                Some("The script printed that 12 tests passed.".to_owned()),
                false
            ),
        ]
    );
}

/// The next resume first closed out the task the last process killed: a notification for a
/// task this process never ran, then an empty result with no model call. That result answers
/// nothing you sent, so only the turn's own comes through.
#[test]
fn a_resume_drops_the_empty_result_left_over_from_the_last_process() {
    let events = replay_version(SEND_NOW, "send_now_resume_stale_task");
    assert_eq!(
        results(&events),
        [(
            Some("The script printed that 12 tests passed.".to_owned()),
            false
        )]
    );
    // That task isn't this process's, and the test ran in the foreground.
    assert!(background_lists(&events).is_empty());
    let started_at = position(&events, |e| {
        matches!(
            e,
            AgentEvent::MessageState {
                state: MessageStage::Started,
                ..
            }
        )
    });
    let result_at = position(&events, |e| matches!(e, AgentEvent::Result { .. }));
    assert!(started_at < result_at);
}
