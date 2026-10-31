//! Every control frame survives a JSON round trip unchanged.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{ControlReply, ControlRequest, Frame};

/// Parses `value` as `T`, writes it back, and checks nothing was lost or added.
fn round_trip<T: Serialize + DeserializeOwned + std::fmt::Debug + PartialEq>(value: Value) -> T {
    let parsed: T =
        serde_json::from_value(value.clone()).unwrap_or_else(|err| panic!("{err}\n{value:#}"));
    let written = serde_json::to_value(&parsed).unwrap();
    assert_eq!(written, value, "the JSON changed on the way through");
    let again: T = serde_json::from_value(written).unwrap();
    assert_eq!(again, parsed);
    parsed
}

fn envelope(kind: Value) -> Value {
    json!({
        "seq": 7,
        "ts": "2026-09-29T12:00:00Z",
        "run_id": "run-01",
        "task_id": "task-002",
        "session": "s-003",
        "source": "argonauts-code",
        "kind": kind,
    })
}

fn diff_stat() -> Value {
    json!({"files_changed": 2, "insertions": 18, "deletions": 4})
}

fn queued() -> Value {
    json!({"id": "m-001", "from": {"party": "user"}, "to": {"target": "task", "value": "task-002"},
           "text": "keep a fallback", "queued_at": "2026-09-29T12:00:00Z", "delivery": "next_turn"})
}

#[test]
fn control_frames_round_trip() {
    let answer = json!({"id": "approval-003", "action": {"action": "merge"}, "text": null,
                        "evidence_sha256": "e3b0", "tasks": []});
    let requests = vec![
        json!({"request": "hello", "client": "app", "version": "0.0.1"}),
        json!({"request": "subscribe", "from_seq": 12, "from_snapshot": false}),
        json!({"request": "subscribe", "from_seq": 0, "from_snapshot": true}),
        json!({"request": "steer", "session": "s-003", "text": "use EVAL"}),
        json!({"request": "mention", "target": {"target": "all"}, "text": "wrap up"}),
        json!({"request": "guidance", "text": "keep it small"}),
        json!({"request": "guidance", "text": "keep it small", "outside_agent": "conversation-bot"}),
        json!({"request": "pause", "now": false}),
        json!({"request": "pause", "now": true, "outside_agent": "conversation-bot"}),
        json!({"request": "cancel_pause"}),
        json!({"request": "stop"}),
        json!({"request": "stop_session", "session": "s-003"}),
        json!({"request": "take_back", "messages": ["m-001"]}),
        json!({"request": "approval_mode", "mode": "auto", "token": "t0k"}),
        json!({"request": "reassign", "task": "task-002", "to": "codex", "rule_override": null,
               "token": null}),
        json!({"request": "reassign", "task": "task-002", "to": "claude-code/opus",
               "rule_override": "review.other_provider", "token": "t0k"}),
        json!({"request": "take_over", "task": "task-002", "token": "t0k"}),
        json!({"request": "take_over_ended", "task": "task-002", "token": "t0k"}),
        json!({"request": "answer", "answer": answer, "token": "t0k"}),
        json!({"request": "status"}),
        json!({"request": "diff", "task": "task-002", "scope": "task"}),
        json!({"request": "raw", "session": "s-003"}),
        json!({"request": "rewind", "task": "task-002", "checkpoint": 2}),
        json!({"request": "spot_check", "id": "approval-003", "correct": false}),
    ];
    for (id, body) in requests.into_iter().enumerate() {
        round_trip::<Frame<ControlRequest>>(json!({"id": id, "body": body}));
    }
    let replies = vec![
        json!({"reply": "welcome", "run": "run-01", "pid": 4242, "version": "0.0.1"}),
        json!({"reply": "ok"}),
        json!({"reply": "error", "message": "no such session"}),
        json!({"reply": "event", "event": envelope(json!({"type": "note",
               "data": {"level": "info", "text": "x"}}))}),
        json!({"reply": "queued", "id": "m-004"}),
        json!({"reply": "taken_back", "messages": [queued()]}),
        json!({"reply": "steered", "ack": {"session": "s-003", "how": "interrupt_resume"}}),
        json!({"reply": "take_over", "ticket": {"task": "task-002", "command": null,
               "worktree": "/wt/task-002", "agent_id": "openrouter"}}),
        json!({"reply": "status", "summary": {"run": null, "goal": [], "plan": null,
               "decisions": [], "tasks": [], "recent": [], "spend": null, "plan_windows": [],
               "waiting": []}}),
        json!({"reply": "diff", "diff": {"task": "task-002", "stat": diff_stat(), "text": "",
               "path": "/d"}}),
        json!({"reply": "raw", "raw": {"session": "s-003", "lines": [], "truncated": true,
               "path": "/r", "cli_transcript": "/v"}}),
    ];
    for body in replies {
        round_trip::<Frame<ControlReply>>(json!({"id": 1, "body": body}));
    }
}
