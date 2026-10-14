//! Subagents as the main session's stream reports them, replayed from a live check on
//! Claude Code 2.1.292: two background subagents, a message that
//! reaches one through the main agent's `SendMessage`, and one stopped alone with `stop_task`.
//! The recording keeps the task events, the `Agent` and `SendMessage` calls with their results,
//! and the control replies, with paths and the session id scrubbed.

use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_core::AgentEvent;
use argonauts_core::adapter::{Adapter, StreamParser};
use argonauts_core::events::session::{SubagentStatus, SubagentUpdate};

const RECORDING: &str = include_str!("data/subagents.stdout.jsonl");

const COUNT_SLOWLY: &str = "toolu_01RPtHvakNkAc1hxf21C6arN";
const SLEEP_LONG: &str = "toolu_01VuwvYaPkhY8qTDuZvnaJcZ";

fn replayed() -> (Box<dyn StreamParser>, Vec<AgentEvent>) {
    let mut parser = ClaudeCodeAdapter.parser();
    let events = RECORDING
        .lines()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect();
    (parser, events)
}

fn updates(events: &[AgentEvent]) -> Vec<&SubagentUpdate> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Subagent { update } => Some(update),
            _ => None,
        })
        .collect()
}

/// What the list shows of one subagent over time: its status and last line, without repeats.
fn story<'a>(updates: &[&'a SubagentUpdate], id: &str) -> Vec<(SubagentStatus, &'a str)> {
    let mut story: Vec<(SubagentStatus, &str)> = Vec::new();
    for update in updates.iter().filter(|update| update.id == id) {
        let now = (
            update.status,
            update.last_message.as_deref().unwrap_or_default(),
        );
        if story.last() != Some(&now) {
            story.push(now);
        }
    }
    story
}

/// Each row ends with the tokens its task last counted, as Claude Code's own list does. The
/// one a message resumed goes on counting from where it stood.
#[test]
fn each_subagent_carries_the_tokens_its_task_counted() {
    let (_, events) = replayed();
    let updates = updates(&events);
    let counts = |id: &str| -> Vec<u64> {
        let mut counts: Vec<u64> = updates
            .iter()
            .filter(|update| update.id == id)
            .filter_map(|update| update.tokens)
            .collect();
        counts.dedup();
        counts
    };
    assert_eq!(
        counts(COUNT_SLOWLY),
        [
            11_823, 12_812, 13_257, 13_841, 16_301, 16_784, 17_051, 17_191, 17_678
        ]
    );
    assert_eq!(counts(SLEEP_LONG), [12_016, 13_213, 13_732]);
}

#[test]
fn the_two_subagents_are_listed_and_the_shell_commands_they_left_running_are_not() {
    let (_, events) = replayed();
    let updates = updates(&events);
    let mut ids: Vec<&str> = updates.iter().map(|update| update.id.as_str()).collect();
    ids.dedup();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids, [COUNT_SLOWLY, SLEEP_LONG]);
    let label = |id: &str| {
        updates
            .iter()
            .find(|update| update.id == id)
            .and_then(|update| update.label.clone())
    };
    assert_eq!(label(COUNT_SLOWLY).as_deref(), Some("Count slowly"));
    assert_eq!(label(SLEEP_LONG).as_deref(), Some("Sleep long"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Unknown { .. } | AgentEvent::Error { .. })),
        "{events:#?}"
    );
}

/// The main agent passed your message on with `SendMessage`. The subagent had finished, so
/// the message resumed it: the same row runs again and ends with the word it was asked for.
#[test]
fn a_message_through_the_main_agent_resumes_a_finished_subagent_as_itself() {
    let (_, events) = replayed();
    let story = story(&updates(&events), COUNT_SLOWLY);
    let done = story
        .iter()
        .position(|step| *step == (SubagentStatus::Done, "DONE-A"))
        .unwrap_or_else(|| panic!("{story:#?}"));
    let after = &story[done + 1..];
    assert_eq!(after[0].0, SubagentStatus::Running, "{story:#?}");
    assert!(
        after.contains(&(SubagentStatus::Done, "DONE-A PINEAPPLE")),
        "{story:#?}"
    );
}

/// `stop_task` takes the task's id, which the parser knows for each subagent. The one stopped
/// here had finished and left a command running: stopping that doesn't change how the
/// subagent itself ended.
#[test]
fn a_subagent_stops_by_its_task_id_and_keeps_how_it_ended() {
    let (parser, events) = replayed();
    let line = parser.stop_subagent_line(SLEEP_LONG).expect("a stop line");
    let line: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(
        line["request"],
        serde_json::json!({"subtype": "stop_task", "task_id": "ae0f143a43d338f17"})
    );
    assert!(parser.stop_subagent_line("toolu_unknown").is_none());
    let story = story(&updates(&events), SLEEP_LONG);
    assert_eq!(
        story.last(),
        Some(&(SubagentStatus::Done, "Waiting for sleep to complete...")),
        "{story:#?}"
    );
}

/// Esc stops the main agent's turn and leaves its subagents running, which Claude Code does
/// only for a screen that said at `initialize` that it stops tasks one by one. A session that
/// never took that gets no line, and is stopped whole. In the recording the interrupt came
/// with both subagents going, and both went on to their own ends.
#[test]
fn a_turn_stops_alone_only_once_the_session_took_initialize() {
    let mut parser = ClaudeCodeAdapter.parser();
    assert!(parser.stop_turn_line().is_none(), "nothing was declared");
    parser.parse_stdout_line(
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"initialize","response":{"commands":[]}}}"#,
    );
    let line = parser.stop_turn_line().expect("a line that stops the turn");
    let line: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(line["type"], "control_request");
    assert_eq!(line["request"], serde_json::json!({"subtype": "interrupt"}));

    let (_, events) = replayed();
    let updates = updates(&events);
    for id in [COUNT_SLOWLY, SLEEP_LONG] {
        let story = story(&updates, id);
        assert_eq!(
            story.last().map(|(status, _)| *status),
            Some(SubagentStatus::Done),
            "{id}: {story:#?}"
        );
    }
}

/// The turn a stop ends says so in its result: `error_during_execution` with an aborted
/// terminal reason (2.1.292's program text; no turn was mid-flight when the recording's
/// interrupt merged). It reads as an interruption and then the turn's result, which is what
/// the screen's `Interrupted` line and the session's end as stopped go by.
///
/// The last case is the result 2.1.287 sent when a run of Argonauts Code's own stopped a session
/// in the middle of a tool call, with its ids and numbers cut: no `result` text, and an
/// `errors` list that is diagnostics rather than a failure to report.
#[test]
fn a_stopped_turns_result_reads_as_an_interruption() {
    let written = |reason: &str| {
        serde_json::json!({"type": "result", "subtype": "error_during_execution",
            "is_error": true, "terminal_reason": reason, "errors": [], "num_turns": 1,
            "duration_ms": 900, "total_cost_usd": 0.01})
    };
    let recorded = serde_json::json!({"type": "result", "subtype": "error_during_execution",
        "is_error": true, "terminal_reason": "aborted_tools", "stop_reason": "tool_use",
        "errors": ["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"],
        "permission_denials": [], "num_turns": 7, "duration_ms": 14250,
        "duration_api_ms": 14186, "total_cost_usd": 0.0268117, "queued_turn_count": 0,
        "result_index": 0, "fast_mode_state": "off"});
    let results = [
        ("aborted_streaming", written("aborted_streaming")),
        ("aborted_tools", written("aborted_tools")),
        ("recorded on 2.1.287", recorded),
    ];
    for (reason, result) in results {
        let mut parser = ClaudeCodeAdapter.parser();
        let events = parser.parse_stdout_line(&result.to_string());
        let read: Vec<String> = events
            .iter()
            .map(|event| match event {
                AgentEvent::Error { category, .. } => format!("error {category:?}"),
                AgentEvent::Result { is_error, .. } => format!("result error={is_error}"),
                other => other.type_name().to_owned(),
            })
            .filter(|read| read.starts_with("error") || read.starts_with("result"))
            .collect();
        assert_eq!(read, ["error Interrupted", "result error=true"], "{reason}");
    }
}
