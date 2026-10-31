use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use argonauts_adapter_claude_code::{ClaudeCodeAdapter, TESTED_VERSIONS};
use argonauts_core::agents::screen::ScreenChange;
use argonauts_core::models::limits::{PlanUsageSource, PlanUsageStatus};
use argonauts_core::runs::events::context::{ContextDetail, ContextKind};
use argonauts_core::runs::events::session::{
    CliAgentState, CliWorkflowRun, SubagentKind, SubagentStatus, SubagentUpdate,
};
use argonauts_core::{
    Adapter, AgentEvent, ErrorCategory, MessageStage, MeterAction, MeterLimit, Report,
    ReportStatus, ReportedCommand, SessionRequest, SessionRole, SpendMeter,
};
use serde_json::Value;
use time::OffsetDateTime;
use time::macros::datetime;

const WORKFLOW_CALL: &str = "toolu_01T9VznHYcM7UjRKWHQ4QH2c";

/// Live calls with stream-json both ways and `initialize` first, recorded without partial
/// messages, so they stay out of `every_fixture`.
const TWO_WAY_CALLS: &[&str] = &[
    "agents_md",
    "sync_off",
    "setting_sources_user",
    "you_should_know",
    "checkup",
];

const STATUS_TOOL: &str = "mcp__argonauts_status__argonauts_status";
const SUBAGENT_CALL: &str = "toolu_01V464aQnAvS82ukoevdYdMW";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_fixture(name: &str, ext: &str) -> String {
    let path = fixture_dir().join(format!("{name}.{ext}"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn meta(name: &str) -> Value {
    serde_json::from_str(&read_fixture(name, "meta.json")).unwrap()
}

fn exit_code(name: &str) -> Option<i32> {
    meta(name)["exit_code"].as_i64().map(|code| code as i32)
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
    normalized(events)
}

fn replay(name: &str) -> Vec<AgentEvent> {
    replay_lines(name, &read_fixture(name, "stdout.jsonl"))
}

/// Sessions recorded with partial messages: one-shot ones with the prompt on stdin, ones with
/// stdin kept open, a chosen id, the status server and a resume, and direct Captain's (calls
/// asked through the permission prompt tool, and a read-only estimate resumed with every
/// tool). A workflow Claude Code ran itself was recorded by hand without them, so it stays out.
fn every_fixture() -> Vec<&'static str> {
    vec![
        "read-only-untitled",
        "write",
        "shell",
        "stream-input",
        "resume",
        "status-tool-error",
        "permission_prompt_tool",
        "readonly_estimate",
        "direct_resume",
    ]
}

/// A plan usage reading is stamped when it is parsed, so replays pin that time to compare runs.
fn normalized(events: Vec<AgentEvent>) -> Vec<AgentEvent> {
    const PARSED_AT: OffsetDateTime = datetime!(2026-09-30 00:00 UTC);
    events
        .into_iter()
        .map(|event| match event {
            AgentEvent::PlanUsage { mut reading } => {
                reading.observed_at = PARSED_AT;
                AgentEvent::PlanUsage { reading }
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
                    | AgentEvent::PlanUsageSignal { .. }
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
    let events = replay("read-only-untitled");
    assert_eq!(
        shape(&events),
        [
            "init",
            "usage",
            "context_fill",
            "text_delta",
            "message",
            "usage",
            "plan_usage",
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
    // The usage after each call's start is the count going on while its input streams.
    assert_eq!(
        shape(&events),
        [
            "init",
            "usage",
            "context_fill",
            "tool_pending",
            "usage",
            "tool_started",
            "usage",
            "plan_usage",
            "tool_finished",
            "file_changed",
            "usage",
            "context_fill",
            "tool_pending",
            "usage",
            "tool_started",
            "usage",
            "tool_finished",
            "usage",
            "context_fill",
            "result",
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
    assert_eq!(input["content"].as_str().map(str::trim_end), Some("hi"));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished { id, ok: true, .. } if *id == write_id
    )));
    assert!(events.contains(&AgentEvent::FileChanged {
        path: "hello.txt".into(),
        kind: "add".into()
    }));

    let (text, structured, _) = result(&events);
    let report = Report::from_value(&structured.expect("structured output")).unwrap();
    assert_eq!(report.status, ReportStatus::Done);
    assert_eq!(report.files_changed, ["hello.txt"]);

    // The report also survives as text, for CLIs without a schema: the result's own text, as
    // this recording's session went from the write straight to its report.
    let from_text = Report::from_text(&text.expect("the result's text")).unwrap();
    assert_eq!(from_text.status, ReportStatus::Done);
}

/// A `session_title_changed` line is read and set aside, and an answer and a shell call come
/// through whole.
#[test]
fn a_session_that_names_itself_reads_an_answer_and_a_shell_call() {
    let answer = replay("read-only");
    assert_clean_session(&answer);
    assert_eq!(result(&answer).0.as_deref(), Some("ok"));

    let shell = replay("shell");
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
    let mut checked = 0;
    for name in every_fixture() {
        let events = replay(name);
        let mut streamed = String::new();
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
        assert!(
            streamed.is_empty(),
            "{name} streamed text never arrived whole"
        );
    }
    // A session may go from one tool straight to the next with no text of its own between, as
    // the write recording does, so the text is counted over every recording.
    assert!(checked > 0, "no recording has main-session text");
    // Subagent text arrives whole, with no deltas before it.
    let events = replay("stream-input");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Message { parent: Some(p), .. } if p == SUBAGENT_CALL
    )));
}

#[test]
fn claude_rate_limit_event_to_plan_usage() {
    // The fullest window counts, whichever one `rateLimitType` names.
    let cases = [
        ("read-only-untitled", "five_hour", 0.17, 1790678400),
        ("stream-input", "seven_day", 0.62, 1790820000),
    ];
    for (name, window, used, resets_at) in cases {
        let events = replay(name);
        let readings: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::PlanUsage { reading } => Some(reading),
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
        assert_eq!(reading.agent_id.0, "claude-code");
        assert_eq!(reading.status, PlanUsageStatus::Allowed);
        assert_eq!(reading.used, Some(used), "{name}");
        assert_eq!(
            reading.resets_at.map(|at| at.unix_timestamp()),
            Some(resets_at)
        );
        assert_eq!(reading.window.as_deref(), Some(window), "{name}");
        assert_eq!(reading.source, PlanUsageSource::RateLimitEvent);
        let detail = reading.detail.as_ref().unwrap();
        assert_eq!(detail["rateLimitType"], "five_hour");
        assert_eq!(detail["unifiedWindows"][window]["utilization"], used);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanUsageSignal { .. }))
        );
    }
}

#[test]
fn every_window_of_a_rate_limit_event_is_a_reading_of_its_own() {
    let events = replay("checkup");
    let readings: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::PlanUsage { reading } => Some(reading),
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
            ("five_hour", 0.54, 1791097200, PlanUsageStatus::Allowed),
            ("seven_day", 0.49, 1791424800, PlanUsageStatus::Allowed),
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
        let stdin = read_fixture(name, "stdin.jsonl");
        let recorded: Value = stdin
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|line| line["request"]["subtype"] == "get_context_usage")
            .unwrap();
        assert_eq!(recorded["request"], asked["request"], "{name}");

        let events = replay(name);
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
        let AgentEvent::ContextBreakdown { usage } = &events[at] else {
            unreachable!()
        };
        let rows: Vec<(&str, u64, ContextKind)> = usage
            .categories
            .iter()
            .map(|row| (row.name.as_str(), row.tokens, row.kind))
            .collect();
        // The deferred system tools are out of the window, and the buffer is held past the
        // compaction point.
        assert_eq!(
            rows,
            [
                ("System prompt", 6888, ContextKind::Used),
                ("System tools", 15135, ContextKind::Used),
                ("System tools (deferred)", 25213, ContextKind::Deferred),
                ("Memory files", 17, ContextKind::Used),
                ("Skills", 1969, ContextKind::Used),
                ("Messages", 1, ContextKind::Used),
                ("Autocompact buffer", 33000, ContextKind::Buffer),
                ("Free space", 142990, ContextKind::Free),
            ],
            "{name}"
        );
        assert_eq!(
            (usage.total_tokens, usage.percentage, usage.window),
            (24_010, 12, 200_000)
        );
        assert_eq!(usage.compacts_at(), Some(167_000));
        assert_eq!(usage.auto_compact_source, "settings");
        assert_eq!(usage.model.as_deref(), Some("claude-haiku-4-5-20251001"));
        // A 200k window draws ten rows of ten cells.
        assert_eq!(usage.grid.len(), 10);
        assert!(usage.grid.iter().all(|row| row.len() == 10));
        assert_eq!(usage.grid[0][0].category, "System prompt");
        assert_eq!(usage.memory_files.len(), 1);
        assert_eq!(usage.memory_files[0].scope, "Project");
        assert!(usage.mcp_tools.is_empty());
        let skills = usage.skills.as_ref().expect("skills");
        assert_eq!((skills.total, skills.items.len()), (23, 23));
        assert_eq!(
            usage.slash_commands.map(|commands| commands.total),
            Some(23)
        );
        // What the messages are made of and the last request come with the short count too.
        assert!(usage.messages.is_some(), "{name}");
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
    let events = replay("stream-input");
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
        Some("Run r-01: task-001 is building. No messages for you.")
    );

    let stdout = read_fixture("stream-input", "stdout.jsonl");
    let init: Value = serde_json::from_str(stdout.lines().nth(1).unwrap()).unwrap();
    assert_eq!(init["subtype"], "init");
    assert_eq!(init["mcp_servers"][0]["name"], "argonauts_status");
    assert_eq!(init["mcp_servers"][0]["status"], "connected");
    assert!(init["mcp_server_errors"].is_null());

    let args = &meta("stream-input")["args"];
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
    let events = replay("status-tool-error");
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
    let stdout = read_fixture("status-tool-error", "stdout.jsonl");
    let recorded: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(recorded["type"], "result");
    assert_eq!(recorded["is_error"], false);
    assert_eq!(exit_code("status-tool-error"), Some(0));
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
    let events = replay("stream-input");
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
fn context_fill_from_each_agent() {
    for name in every_fixture() {
        let events = replay(name);
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
        // The window comes from the first result's modelUsage. The shell recording ran with
        // the larger one, as Haiku 5.5, which the write recording ran on, has.
        let window = if matches!(name, "shell" | "write") {
            1_000_000
        } else {
            200_000
        };
        assert_eq!(fills.last().unwrap().1, Some(window), "{name}");
        for (used, size) in &fills {
            assert!(*used > 0 && size.is_none_or(|s| *used < s), "{name}");
        }
    }
    // The context holds input plus cache tokens: 10 + 4563 + 1798 in the read-only reply.
    let read_only = replay("read-only-untitled");
    assert!(read_only.contains(&AgentEvent::ContextFill {
        used: 6371,
        size: None,
        sent: None,
    }));
    // A resumed session starts from everything before it.
    let resumed = replay("resume");
    let first = resumed.iter().find_map(|e| match e {
        AgentEvent::ContextFill { used, .. } => Some(*used),
        _ => None,
    });
    assert!(first.unwrap() > 20_000);
}

#[test]
fn stream_input_takes_a_second_message_mid_run() {
    let events = replay("stream-input");
    assert_clean_session_but_totals(&events);
    // A subagent's messages carry the usage from their start, so its output shows in full only
    // in the result's totals, which replace the increments.
    let (input, output, cached, _) = cumulative_totals(&events);
    let (run_in, run_out, run_cached) = incremental_totals(&events);
    assert_eq!((run_in, run_cached), (input, cached));
    assert!(run_out < output);
    let meta = meta("stream-input");
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
    // Claude Code delivered the mid-turn message with the next tool result, not as its own turn.
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
    let first = replay("stream-input");
    let resumed = replay("resume");
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
        meta(name)["args"]
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
        meta("resume")["resume_session"],
        meta("stream-input")["session_id"]
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
fn limit_meter_counts_each_recording_once() {
    let one_session = [
        "read-only-untitled",
        "write",
        "shell",
        "stream-input",
        "status-tool-error",
        "permission_prompt_tool",
        "readonly_estimate",
        "direct_resume",
    ];
    for name in one_session {
        let events = replay(name);
        let mut meter = SpendMeter::new(MeterLimit::Usd(0.5), 0.85);
        for event in &events {
            assert_eq!(meter.observe(event), MeterAction::Continue, "{name}");
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
fn a_tight_token_limit_stops_mid_stream() {
    let mut meter = SpendMeter::new(MeterLimit::Tokens(300), 0.5);
    let actions: Vec<MeterAction> = replay("write").iter().map(|e| meter.observe(e)).collect();
    let first_stop = actions.iter().position(|a| *a == MeterAction::Stop);
    let last_event = actions.len() - 1;
    assert!(first_stop.is_some_and(|i| i < last_event), "{actions:?}");
}

#[test]
fn malformed_lines_become_unknown_without_disturbing_the_rest() {
    let clean = replay("read-only-untitled");
    let stdout = read_fixture("read-only-untitled", "stdout.jsonl");
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.insert(3, "{this is not json");
    lines.insert(5, r#"{"type":"from_the_future","x":1}"#);
    let events = replay_lines("read-only-untitled", &lines.join("\n"));
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
    // The call the model was writing when the stream stopped goes first.
    assert!(
        matches!(
            end.as_slice(),
            [
                AgentEvent::ToolDropped { .. },
                AgentEvent::Error { .. },
                AgentEvent::Result { is_error: true, .. }
            ] | [
                AgentEvent::Error { .. },
                AgentEvent::Result { is_error: true, .. }
            ]
        ),
        "{end:#?}"
    );
}

fn workflows(events: &[AgentEvent]) -> Vec<&CliWorkflowRun> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Workflow { workflow } => Some(&**workflow),
            _ => None,
        })
        .collect()
}

/// Claude Code runs a workflow script in the background and reports it through
/// `system/task_*` events. The replay mirrors it as it goes: the script's own name,
/// description, and phases first, then each agent under its phase, with its model, tokens, and
/// time, whatever the names.
#[test]
fn claude_runs_its_own_workflow_and_the_replay_mirrors_it() {
    let events = replay("workflow");
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
    assert_eq!(ask.state, CliAgentState::Running);
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
    type Row<'a> = (&'a str, Option<u32>, CliAgentState, u64, Option<&'a str>);
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
                CliAgentState::Done,
                10_311,
                Some("Ready")
            ),
            (
                "answer:one",
                Some(2),
                CliAgentState::Done,
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
fn tested_versions_are_the_ones_the_recordings_name() {
    assert_eq!(ClaudeCodeAdapter.tested_versions(), TESTED_VERSIONS);
    assert!(!TESTED_VERSIONS.is_empty());
    for file in fs::read_dir(fixture_dir()).unwrap() {
        let path = file.unwrap().path();
        if !path.to_string_lossy().ends_with(".meta.json") {
            continue;
        }
        let meta: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let recorded_with = meta["cli_version"].as_str().unwrap();
        let also_matches = meta["also_matches"].as_array().unwrap();
        for version in also_matches
            .iter()
            .map(|v| v.as_str().unwrap())
            .chain([recorded_with])
        {
            assert!(
                TESTED_VERSIONS.contains(&version),
                "{}: {version}",
                path.display()
            );
        }
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
    for file in fs::read_dir(fixture_dir()).unwrap() {
        let path = file.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        for leak in leaks {
            assert!(!text.contains(leak), "{} contains {leak:?}", path.display());
        }
    }
}

/// A stream cut mid-workflow, as when its session is stopped, paused, killed for a stall, or
/// past its limit: the workflow ends with it, and so does every agent still going, so nothing
/// stays running in the panel or the view.
#[test]
fn a_workflow_cut_off_mid_stream_ends_with_its_session() {
    let whole = read_fixture("workflow", "stdout.jsonl");
    let cut: Vec<&str> = whole.lines().take(20).collect();
    let events = replay_lines("workflow", &cut.join("\n"));
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
        last.agents
            .iter()
            .all(|agent| matches!(agent.state, CliAgentState::Done | CliAgentState::Failed)),
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
    let whole = read_fixture("workflow", "stdout.jsonl");
    let mut lines: Vec<&str> = whole.lines().collect();
    // Line 14 is the first progress report; line 11 its task's start.
    let started = lines.remove(10);
    lines.insert(13, started);
    let events = replay_lines("workflow", &lines.join("\n"));
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

/// The lines of a two-way session (`command_lifecycle`, `control_response`, task events,
/// `ui_invalidate`, `tool_progress`) are all read, and `init` and `initialize` say what the CLI
/// is.
#[test]
fn two_way_calls_parse_with_nothing_unknown() {
    for name in TWO_WAY_CALLS {
        let events = replay(name);
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
        assert_eq!(
            version.as_deref(),
            meta(name)["cli_version"].as_str(),
            "{name}"
        );
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
                AgentEvent::PlanUsage { reading } => !reading.wrap_up,
                _ => true,
            }),
            "{name}"
        );
    }

    // The audit reached its limit before its report.
    let checkup = replay("checkup");
    assert!(
        checkup.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                category: ErrorCategory::Limit,
                ..
            }
        )),
        "{checkup:?}"
    );
    // Two turns in one process, each with its own result.
    assert_eq!(results(&replay("setting_sources_user")).len(), 2);
}

/// Replayed as an isolated session's, whose parser checks what the session loaded.
fn replay_isolated(name: &str) -> Vec<AgentEvent> {
    let request = SessionRequest {
        isolated: true,
        ..SessionRequest::default()
    };
    let mut parser = ClaudeCodeAdapter.session_parser(&request);
    read_fixture(name, "stdout.jsonl")
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
    for name in ["setting_sources_user", "readonly_estimate"] {
        let found = leaks(&replay_isolated(name));
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
        assert!(leaks(&replay(name)).is_empty(), "{name}");
    }
}

/// With the keys, Claude Code loaded only the kept built-ins and the plugin the check passed with
/// `--plugin-dir`, which Argonauts Code never does. Its plugin never ran: no pushes, and the model
/// saw neither code word.
#[test]
fn the_isolation_keys_keep_your_plugins_and_synced_skills_out() {
    let events = replay_isolated("isolation_keys");
    assert_eq!(
        leaks(&events),
        [(vec!["argo-probe".to_owned()], Vec::new())]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::PluginOutput { .. })),
        "the probe's plugin pushed something"
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
    let recorded = &meta("isolation_keys")["files"][0]["contents"];
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

/// Claude Code doesn't flag an rm whose empty variable can't reach a top-level folder, so the
/// check's command ran under both a Bash allow rule and a hook that allowed it. Neither turned
/// into an error or a denial.
#[test]
fn an_rm_claude_code_doesnt_flag_runs_with_no_denial() {
    for name in ["flagged_rm_allow_rule", "flagged_rm_hook"] {
        let events = replay(name);
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
/// queue, the plugin's command in the CLI's own list, and You should know as one line and three
/// buttons with the handles a press sends back.
#[test]
fn messages_commands_and_the_notice_read() {
    let events = replay("you_should_know");
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
    let first = drawn.first().expect("a notice drew");
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

    let events = replay("plugin_command");
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
        "the initialize reply lists the plugin's command"
    );
    assert!(
        lists.iter().skip(1).any(|list| has(list, "probe-late")),
        "a command registered mid-session comes with commands_changed"
    );
}

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
/// ended. Here that came only after the input closed, which killed it, and a task Claude Code
/// stopped starts no turn of its own.
#[test]
fn a_command_moved_to_the_background_stays_listed_past_the_result() {
    let events = replay("send_now_resume");
    assert_eq!(
        background_lists(&events),
        [(vec!["bmvwwjv0b".to_owned()], false), (Vec::new(), false)]
    );
    let result_at = position(&events, |e| matches!(e, AgentEvent::Result { .. }));
    let emptied_at = position(
        &events,
        |e| matches!(e, AgentEvent::BackgroundTasks { running, .. } if running.is_empty()),
    );
    assert!(result_at < emptied_at);
    assert_eq!(results(&events), [(Some("PONG".to_owned()), false)]);
}

/// With the input left open, the command ran to its end, and Claude Code told the model in a
/// turn of its own, with no message of yours, whose result came second.
#[test]
fn a_background_command_that_ends_between_turns_brings_a_turn_of_its_own() {
    let events = replay("send_now_resume_open");
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
    let events = replay("send_now_resume_stale_task");
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
