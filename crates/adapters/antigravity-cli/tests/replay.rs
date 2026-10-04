use std::fs;
use std::path::{Path, PathBuf};

use argonauts_adapter_antigravity_cli::{AntigravityCli, TESTED_VERSIONS};
use argonauts_core::events::session::{SubagentKind, SubagentStatus, SubagentUpdate};
use argonauts_core::limits::{QuotaSource, QuotaStatus};
use argonauts_core::{
    Adapter, AgentEvent, BudgetLimit, BudgetMeter, ErrorCategory, Report, ReportStatus, RetryReason,
};
use serde_json::{Value, json};
use time::macros::datetime;

const VERSION: &str = "1.2.13";

struct Replay {
    events: Vec<AgentEvent>,
    meta: Value,
}

fn replay(name: &str) -> Replay {
    replay_at(VERSION, name)
}

fn replay_at(version: &str, name: &str) -> Replay {
    let dir = fixtures_dir().join(version);
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

/// Recordings of sessions, as opposed to the quota probe.
const SESSIONS: &[&str] = &[
    "readonly_ok",
    "build_hello_schema",
    "build_shell",
    "stdin_steer",
    "build_subagent",
    "readonly_write_refused",
];

/// The older replays check the other events in order; context fill has its own test.
fn without_fills(events: Vec<AgentEvent>) -> Vec<AgentEvent> {
    events
        .into_iter()
        .filter(|e| !matches!(e, AgentEvent::ContextFill { .. }))
        .collect()
}

fn fills(events: &[AgentEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ContextFill { used, size: None } => Some(*used),
            _ => None,
        })
        .collect()
}

fn subagents(events: &[AgentEvent]) -> Vec<&SubagentUpdate> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Subagent { update } => Some(update),
            _ => None,
        })
        .collect()
}

fn meta_args(meta: &Value) -> Vec<&str> {
    meta["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect()
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
    for name in SESSIONS {
        let meta = replay(name).meta;
        let args = meta_args(&meta);
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
    let events = without_fills(events);
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
    let events = without_fills(events);
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

/// Checked live on 1.2.16: with neither sandbox flag, agy refuses a command it would
/// ask about. The refused command comes only from its tool step, since the result's
/// `denied_actions` names the kind of action alone, and the turn may give no answer at all.
/// Direct solo offers the command after the turn from these two events.
#[test]
fn a_refused_command_is_named_by_its_tool_step() {
    let Replay { events, .. } = replay_at("1.2.16", "denied_command");
    let started = events.iter().find_map(|event| match event {
        AgentEvent::ToolStarted { id, input, .. } => Some((id.clone(), input.clone())),
        _ => None,
    });
    let (id, input) = started.expect("the command's tool step");
    assert_eq!(input, json!({"CommandLine": "echo hi > shell.txt"}));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolFinished { id: done, ok: false, output: Some(out), .. }
            if done == &id && out.starts_with("permission check failed")
    )));
    let answer: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text, .. } | AgentEvent::Message { text, .. } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(answer.trim().is_empty(), "{answer:?}");
}

#[test]
fn denied_command_then_capacity_error_fails_once() {
    let Replay { events, .. } = replay("build_shell");
    let events = without_fills(events);
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
    // The turn's result closes it. The stderr lines that repeat its error come later and are
    // dropped, error id and all.
    assert_eq!(
        events[6],
        AgentEvent::Error {
            category: ErrorCategory::Overloaded,
            message: "API error (attempt 2): UNAVAILABLE (code 503): No capacity available for model gemini-3.6-flash-low on the server".into(),
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
fn a_killed_run_still_gets_one_result_per_turn() {
    for name in SESSIONS {
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
        let turns = if *name == "stdin_steer" { 2 } else { 1 };
        assert_eq!(results, turns, "{name}");
    }
}

fn fixture_lines(name: &str) -> Vec<String> {
    let path = fixtures_dir()
        .join(VERSION)
        .join(format!("{name}.stdout.jsonl"));
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn is_result_line(line: &str) -> bool {
    line.contains(r#""event":"result""#)
}

/// Changes one result line, keeping the rest of the recording as it was.
fn edit_result(line: &mut String, edit: impl FnOnce(&mut Value)) {
    let mut value: Value = serde_json::from_str(line).unwrap();
    edit(&mut value["result"]);
    *line = value.to_string();
}

fn results(events: &[AgentEvent]) -> Vec<&AgentEvent> {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Result { .. }))
        .collect()
}

/// With open input, agy waits for the next line after each result, so every turn must close at
/// its own result.
#[test]
fn an_error_result_closes_its_turn_while_input_stays_open() {
    let capacity = "API error (attempt 2): UNAVAILABLE (code 503): No capacity available";
    let mut lines = fixture_lines("stdin_steer");
    let first = lines.iter().position(|l| is_result_line(l)).unwrap();
    edit_result(&mut lines[first], |result| {
        result["status"] = json!("ERROR");
        result["response"] = json!("");
        result["error"] = json!(capacity);
        result.as_object_mut().unwrap().remove("structured_output");
    });

    let mut parser = AntigravityCli.parser();
    let mut turns = Vec::new();
    let mut turn = Vec::new();
    for line in &lines {
        turn.extend(parser.parse_stdout_line(line));
        if is_result_line(line) {
            turns.push(std::mem::take(&mut turn));
        }
    }
    assert!(turn.is_empty(), "{turn:?}");
    assert_eq!(turns.len(), 2);

    let first_turn = &turns[0];
    let errors: Vec<&AgentEvent> = first_turn
        .iter()
        .filter(|e| matches!(e, AgentEvent::Error { .. }))
        .collect();
    assert_eq!(
        errors,
        [&AgentEvent::Error {
            category: ErrorCategory::Overloaded,
            message: capacity.into(),
            retryable: false,
        }]
    );
    assert!(matches!(
        first_turn.last(),
        Some(AgentEvent::Result {
            text: Some(text),
            is_error: true,
            ..
        }) if text.contains("Throwaway repo")
    ));

    let second_turn = &turns[1];
    assert!(
        !second_turn
            .iter()
            .any(|e| matches!(e, AgentEvent::Error { .. }))
    );
    assert!(matches!(
        results(second_turn)[..],
        [AgentEvent::Result {
            text: Some(text),
            is_error: false,
            ..
        }] if text.contains("2 lines")
    ));

    // The clean exit after the worker closes input adds nothing, least of all turn 1 again.
    assert!(parser.finish(Some(0)).is_empty());
}

#[test]
fn a_recovered_turn_closes_at_its_result() {
    let lines = fixture_lines("build_hello_schema");
    let mut parser = AntigravityCli.parser();
    let mut before = Vec::new();
    for line in &lines[..lines.len() - 1] {
        before.extend(parser.parse_stdout_line(line));
    }
    assert!(results(&before).is_empty());
    let at_result = parser.parse_stdout_line(lines.last().unwrap());
    assert_eq!(names(&at_result), ["usage", "retry", "result"]);
    assert!(matches!(
        &at_result[2],
        AgentEvent::Result {
            structured: Some(_),
            is_error: false,
            ..
        }
    ));
    assert!(parser.finish(Some(0)).is_empty());
}

#[test]
fn a_result_with_odd_usage_keeps_its_outcome() {
    let mut lines = fixture_lines("readonly_ok");
    let at = lines.iter().position(|l| is_result_line(l)).unwrap();
    edit_result(&mut lines[at], |result| {
        result["usage"]["input_tokens"] = json!(14432.0);
        result["usage"]["output_tokens"] = json!("1");
    });
    let mut parser = AntigravityCli.parser();
    let events: Vec<AgentEvent> = lines
        .iter()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Unknown { .. }))
    );
    assert!(events.contains(&usage(14_432, 0, true)));
    assert_eq!(
        results(&events),
        [&AgentEvent::Result {
            text: Some("ok\n".into()),
            structured: None,
            is_error: false,
        }]
    );
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

#[test]
fn agy_stream_json_parses() {
    let Replay { events, meta } = replay("stdin_steer");
    let args = meta_args(&meta);
    let stdin: Vec<Value> = meta["stdin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| serde_json::from_str(entry["line"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        stdin,
        [
            json!({"event": "user", "message": {"content": meta["prompt"]}}),
            json!({"event": "user", "message": {"content": "Also tell me how many lines notes.txt has."}}),
        ]
    );
    assert!(
        !args.iter().any(|a| a.contains("README")),
        "prompt leaked into args"
    );
    assert!(
        args.windows(2)
            .any(|w| w == ["--input-format", "stream-json"])
    );
    assert!(
        args.windows(2)
            .any(|w| w == ["--agent", "argonauts-read-only"])
    );
    assert!(args.windows(2).any(|w| w[0] == "--project"));
    assert_eq!(meta["exit_code"], 0);
    assert!(meta["env_set"][0] == json!(["AGY_CLI_DISABLE_AUTO_UPDATE", "true"]));

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Unknown { .. })),
        "every line is understood"
    );
    let AgentEvent::Init { session_id, .. } = &events[0] else {
        panic!("expected init first, got {:?}", events[0]);
    };
    assert_eq!(
        session_id.as_deref(),
        Some("e102fbe2-de7a-4739-9fc0-25af3db0ad11")
    );

    let results: Vec<&Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Result {
                text,
                is_error: false,
                ..
            } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "one result per turn");
    assert!(results[0].as_deref().unwrap().contains("Throwaway repo"));
    assert!(results[1].as_deref().unwrap().contains("2 lines"));

    let tools: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        ["find_by_name", "view_file", "find_by_name", "view_file"]
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::FileChanged { .. }))
    );

    // Each result's usage is the session total so far, so the second replaces the first.
    let cumulative: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Usage {
                input_tokens,
                cumulative: true,
                ..
            } => Some(*input_tokens),
            _ => None,
        })
        .collect();
    assert_eq!(cumulative.len(), 2);
    assert!(cumulative[1] > cumulative[0]);
    // The meter ends on the second result's session total, cache reads left out.
    assert_eq!(billable_tokens(&events), 50_880 + 1276);
}

#[test]
fn agy_subagent_info() {
    let Replay { events, .. } = replay("build_subagent");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Unknown { .. }))
    );
    let child = "1ed3b3e2-efb1-4f04-a33c-31ae210f726b";
    let started = SubagentUpdate {
        id: child.into(),
        parent: None,
        kind: SubagentKind::Subagent,
        label: Some("Notes Reader".into()),
        prompt: Some(
            "Read the file notes.txt in the current workspace directory and report its first line."
                .into(),
        ),
        status: SubagentStatus::Started,
        last_message: None,
        log_uri: Some(format!(
            "file://~/.gemini/antigravity-cli/brain/{child}/.system_generated/logs/transcript.jsonl"
        )),
        steps_visible: false,
    };
    let done = SubagentUpdate {
        status: SubagentStatus::Done,
        ..started.clone()
    };
    assert_eq!(subagents(&events), [&started, &done]);

    let invoke = events
        .iter()
        .position(
            |e| matches!(e, AgentEvent::ToolStarted { name, .. } if name == "invoke_subagent"),
        )
        .unwrap();
    assert!(matches!(
        &events[invoke + 1],
        AgentEvent::ToolFinished { id, ok: true, .. } if id == "step-2"
    ));
    assert!(matches!(&events[invoke + 2], AgentEvent::Subagent { update } if update.id == child));
    let result = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Result {
                    is_error: false,
                    ..
                }
            )
        })
        .unwrap();
    let closed = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Subagent { update } if update.status == SubagentStatus::Done))
        .unwrap();
    assert!(
        closed < result,
        "the turn's end closes the child before the result"
    );
}

#[test]
fn context_fill_from_each_vendor() {
    let expected: &[(&str, &[u64])] = &[
        ("readonly_ok", &[14_433]),
        ("build_hello_schema", &[14_995, 15_180, 15_345]),
        ("build_shell", &[14_567]),
        (
            "stdin_steer",
            &[5759, 6239, 6611, 6947, 7152, 7380, 7678, 7864],
        ),
        ("build_subagent", &[14_539, 14_890, 15_046]),
        ("readonly_write_refused", &[5671]),
    ];
    for (name, want) in expected {
        let Replay { events, .. } = replay(name);
        assert_eq!(fills(&events), *want, "{name}");
    }
}

#[test]
fn read_only_agent_had_no_write_tool() {
    let Replay { events, meta } = replay("readonly_write_refused");
    assert_eq!(meta["files_after"], json!(["README.md", "notes.txt"]));
    assert!(
        meta_args(&meta)
            .windows(2)
            .any(|w| w == ["--agent", "argonauts-read-only"])
    );
    let contents = meta["files"][0]["contents"].as_str().unwrap();
    assert!(!contents.contains("write_to_file"));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolStarted { .. }))
    );
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::Result { text: Some(t), is_error: false, .. } if t.trim() == "no write tool")
    ));
}

#[test]
fn agy_usage_probe_parses() {
    let dir = fixtures_dir().join(VERSION);
    let stdout = fs::read_to_string(dir.join("usage_probe.stdout.jsonl")).unwrap();
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("usage_probe.meta.json")).unwrap())
            .unwrap();
    let probe = AntigravityCli
        .quota_probe(Path::new("/work"), None)
        .expect("agy has a free probe");
    assert_eq!(meta_args(&meta), probe.args);

    let reading = AntigravityCli.parse_quota_probe(&stdout).unwrap();
    assert_eq!(reading.vendor.0, "antigravity-cli");
    assert_eq!(reading.source, QuotaSource::UsageProbe);
    assert_eq!(reading.status, QuotaStatus::Allowed);
    // The Gemini group's weekly window had the least left.
    assert_eq!(reading.window.as_deref(), Some("weekly"));
    let used = reading.used.unwrap();
    assert!(used > 0.001 && used < 0.002, "{used}");
    assert_eq!(reading.resets_at, Some(datetime!(2026-10-06 02:52:44 UTC)));
    let groups = reading.detail.unwrap()["groups"].as_array().unwrap().len();
    assert_eq!(groups, 2);
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
    for version in fs::read_dir(fixtures_dir()).unwrap() {
        for file in fs::read_dir(version.unwrap().path()).unwrap() {
            let path = file.unwrap().path();
            let text = fs::read_to_string(&path).unwrap();
            for leak in leaks {
                assert!(!text.contains(leak), "{} contains {leak:?}", path.display());
            }
        }
    }
}
