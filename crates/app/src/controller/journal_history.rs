//! The part of a run's whole journal the condensed replay reads. A reattached stream starts at
//! the latest snapshot, but your request, the chat turns, and the milestones go back to the
//! run's start.

use std::io;
use std::path::Path;

use argonauts_core::events::RunEvent;

/// The envelope kinds the replay reads. Agent output, vendor records, and budget lines, which
/// make up most of a journal, are never parsed.
const KINDS: [&[u8]; 8] = [
    b"run",
    b"goal",
    b"card",
    b"session",
    b"integration",
    b"checkpoint",
    b"handoff",
    b"workflow",
];

/// The session events the replay reads. The rest, such as each session's whole input, are
/// skipped unparsed.
const SESSION_EVENTS: [&[u8]; 2] = [br#""event":"started""#, br#""event":"queued""#];

const SNAPSHOT: &[u8] = br#""event":"snapshot""#;

/// Events up to `upto` of the kinds the condensed replay reads, oldest first. A line that
/// doesn't parse, such as a torn last line, is skipped as the stream skips it.
pub(super) fn replay_history(events: &Path, upto: u64) -> io::Result<Vec<RunEvent>> {
    let bytes = std::fs::read(events)?;
    let mut history = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if !wanted(line) {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<RunEvent>(line) else {
            continue;
        };
        if event.seq > upto {
            break;
        }
        history.push(event);
    }
    Ok(history)
}

fn wanted(line: &[u8]) -> bool {
    match kind_of(line) {
        Some(b"session") => SESSION_EVENTS.iter().any(|tag| contains(line, tag)),
        // Snapshots are large, and the whole journal already holds what they sum up.
        Some(b"run") => !contains(line, SNAPSHOT),
        Some(kind) => KINDS.contains(&kind),
        None => false,
    }
}

/// The envelope's `kind.type`. Every field before it is a number or a string, and a JSON
/// string can't hold an unescaped quote, so the first match is the envelope's own.
fn kind_of(line: &[u8]) -> Option<&[u8]> {
    const TAG: &[u8] = br#""kind":{"type":""#;
    let start = find(line, TAG)? + TAG.len();
    let len = line[start..].iter().position(|byte| *byte == b'"')?;
    Some(&line[start..start + len])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

#[cfg(test)]
mod tests {
    use argonauts_core::api::StatusSummary;
    use argonauts_core::event::AgentEvent;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::checkpoint::AnswerAction;
    use argonauts_core::events::run::{Party, RunLifecycle, RunSnapshot};
    use argonauts_core::names::CheckpointId;

    use super::*;
    use crate::client::test_support::{
        checkpoint_decided, event, plan_checkpoint, run_id, run_started,
    };
    use crate::views::condensed_replay;

    fn journal(events: &[RunEvent]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut text = String::new();
        for event in events {
            text.push_str(&serde_json::to_string(event).unwrap());
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    fn delta(seq: u64) -> RunEvent {
        event(
            seq,
            RunEventKind::Agent(AgentEvent::TextDelta {
                text: "\"kind\":{\"type\":\"run\"} thinking".to_owned(),
                parent: None,
            }),
        )
    }

    fn snapshot(seq: u64) -> RunEvent {
        event(
            seq,
            RunEventKind::Run(RunLifecycle::Snapshot(Box::new(RunSnapshot {
                upto: seq - 1,
                engine: serde_json::Value::Null,
                summary: StatusSummary::default(),
                open_checkpoints: Vec::new(),
                running: Vec::new(),
            }))),
        )
    }

    /// Your request and a plan opened before the snapshot still reach the replay, and agent
    /// output is never read.
    #[test]
    fn the_replay_reaches_back_past_the_latest_snapshot() {
        let plan = CheckpointId::numbered(1);
        let (_dir, path) = journal(&[
            run_started(0, "Add --json to export", None),
            plan_checkpoint(1, &plan, 5),
            delta(2),
            snapshot(3),
            checkpoint_decided(4, &plan, AnswerAction::Approve, Party::User, None),
            delta(5),
            checkpoint_decided(6, &plan, AnswerAction::Approve, Party::User, None),
        ]);
        let history = replay_history(&path, 5).unwrap();
        let seqs: Vec<u64> = history.iter().map(|event| event.seq).collect();
        assert_eq!(seqs, [0, 1, 4]);
        let Some(argonauts_core::app::AppEvent::CondensedReplay {
            turns, milestones, ..
        }) = condensed_replay(&history, &run_id(), "※ recap".to_owned())
        else {
            panic!("a replay");
        };
        assert_eq!(turns[0].text, "Add --json to export");
        assert_eq!(milestones, ["Plan approved · 5 tasks"]);
    }

    #[test]
    fn a_missing_journal_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = replay_history(&dir.path().join("events.jsonl"), 3).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
