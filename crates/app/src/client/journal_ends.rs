//! The first and last lines of a run's journal, read without parsing the rest, so listing and
//! wake checks stay cheap however long a run's log grows.

use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::{RunEvent, RunEventKind};
use serde::Deserialize;

/// How far back from the end to look for the last complete line before reading more.
const TAIL_WINDOW: u64 = 64 * 1024;

/// The run's first event, `RunStarted` in a healthy log.
pub(crate) fn first_event(events: &Path) -> Option<RunEvent> {
    let file = File::open(events).ok()?;
    let mut line = Vec::new();
    BufReader::new(file).read_until(b'\n', &mut line).ok()?;
    serde_json::from_slice(&line).ok()
}

/// The last event that parses. A torn last line after a crash is skipped.
pub(crate) fn last_event(events: &Path) -> Option<RunEvent> {
    last_lines(events)
        .iter()
        .rev()
        .find_map(|line| serde_json::from_slice(line).ok())
}

/// The latest event that moves the run between going, parked, and finished. `Parked` and
/// `Finished` are always among the last few lines, so the end of the log is enough.
pub(crate) fn last_stage_event(events: &Path) -> Option<RunEvent> {
    last_lines(events)
        .iter()
        .rev()
        .filter_map(|line| serde_json::from_slice::<RunEvent>(line).ok())
        .find(|event| {
            matches!(
                event.kind,
                RunEventKind::Run(
                    RunLifecycle::Started(_)
                        | RunLifecycle::Resumed(_)
                        | RunLifecycle::PauseRequested { .. }
                        | RunLifecycle::PauseCancelled
                        | RunLifecycle::Parked { .. }
                        | RunLifecycle::Finished { .. }
                )
            )
        })
}

/// Whether a `Resumed` follows the event at `seq`, as one does the `Finished` of a stopped run
/// you reopened. Reads back from the end only as far as `seq`.
pub(crate) fn resumed_after(events: &Path, seq: u64) -> bool {
    let Ok(bytes) = std::fs::read(events) else {
        return false;
    };
    for line in bytes.rsplit(|byte| *byte == b'\n') {
        let Ok(event) = serde_json::from_slice::<RunEvent>(line) else {
            continue;
        };
        if event.seq <= seq {
            return false;
        }
        if matches!(event.kind, RunEventKind::Run(RunLifecycle::Resumed(_))) {
            return true;
        }
    }
    false
}

/// True when the log ends parked or finished, so a supervisor that left did so on purpose.
pub(crate) fn ends_settled(events: &Path) -> bool {
    last_stage_event(events).is_some_and(|event| {
        matches!(
            event.kind,
            RunEventKind::Run(RunLifecycle::Parked { .. } | RunLifecycle::Finished { .. })
        )
    })
}

/// The `seq` of the last complete line, even one from a newer Argonauts that doesn't parse.
pub(crate) fn last_seq(events: &Path) -> Option<u64> {
    #[derive(Deserialize)]
    struct Envelope {
        seq: u64,
    }
    last_lines(events)
        .iter()
        .rev()
        .find_map(|line| serde_json::from_slice::<Envelope>(line).ok())
        .map(|envelope| envelope.seq)
}

/// Complete lines near the end of the file, oldest first.
fn last_lines(events: &Path) -> Vec<Vec<u8>> {
    let Ok(mut file) = File::open(events) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return Vec::new();
    };
    let mut window = TAIL_WINDOW;
    loop {
        let start = len.saturating_sub(window);
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        let mut lines: Vec<Vec<u8>> = bytes
            .split(|byte| *byte == b'\n')
            .map(<[u8]>::to_vec)
            .collect();
        // The first piece may be the cut-off end of an earlier line.
        if start > 0 {
            lines.remove(0);
        }
        // The last piece has no newline yet: a line still being written.
        lines.pop();
        lines.retain(|line| !line.trim_ascii().is_empty());
        if !lines.is_empty() || start == 0 {
            return lines;
        }
        window = window.saturating_mul(4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{event, write_journal};
    use argonauts_core::events::run::{ParkReason, ResumeCause, RunOutcome, RunResumed};
    use std::io::Write as _;

    #[test]
    fn ends_skip_a_torn_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        write_journal(
            &path,
            &[
                event(0, RunEventKind::Run(RunLifecycle::PauseCancelled)),
                event(
                    1,
                    RunEventKind::Run(RunLifecycle::Parked {
                        reason: ParkReason::Paused,
                        waiting_on: Vec::new(),
                    }),
                ),
            ],
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"seq\":2,\"ts\"").unwrap();
        assert_eq!(first_event(&path).unwrap().seq, 0);
        assert_eq!(last_event(&path).unwrap().seq, 1);
        assert_eq!(last_seq(&path), Some(1));
    }

    #[test]
    fn only_a_resumed_after_a_finished_reopens_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let stopped = RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Stopped,
            detail: None,
        });
        let note = RunEventKind::Note(argonauts_core::Note {
            level: argonauts_core::NoteLevel::Warn,
            text: "your guidance was refused".to_owned(),
        });
        write_journal(&path, &[event(0, stopped), event(1, note)]);
        assert!(!resumed_after(&path, 0), "a note doesn't reopen it");

        let resumed = RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
            cause: ResumeCause::User,
            settings_changed: false,
            harness_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        }));
        write_journal(&path, &[event(2, resumed)]);
        assert!(resumed_after(&path, 0));
        assert!(!resumed_after(&path, 2));
    }

    #[test]
    fn a_newer_event_still_counts_for_the_last_seq() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        write_journal(
            &path,
            &[event(
                0,
                RunEventKind::Run(RunLifecycle::Finished {
                    outcome: RunOutcome::Merged,
                    detail: None,
                }),
            )],
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"seq\":1,\"kind\":{\"type\":\"from_the_future\"}}\n")
            .unwrap();
        assert_eq!(last_event(&path).unwrap().seq, 0);
        assert_eq!(last_seq(&path), Some(1));
        assert_eq!(last_seq(&dir.path().join("missing.jsonl")), None);
    }

    #[test]
    fn the_stage_comes_from_the_last_lifecycle_event_that_moves_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        assert!(last_stage_event(&path).is_none());
        assert!(!ends_settled(&path));
        let parked = RunEventKind::Run(RunLifecycle::Parked {
            reason: ParkReason::Paused,
            waiting_on: Vec::new(),
        });
        write_journal(
            &path,
            &[
                event(0, RunEventKind::Run(RunLifecycle::PauseCancelled)),
                event(1, parked),
                event(
                    2,
                    RunEventKind::Note(argonauts_core::Note {
                        level: argonauts_core::NoteLevel::Info,
                        text: "a note after parking".to_owned(),
                    }),
                ),
            ],
        );
        assert_eq!(last_stage_event(&path).unwrap().seq, 1);
        assert!(ends_settled(&path));
        write_journal(
            &path,
            &[event(3, RunEventKind::Run(RunLifecycle::PauseCancelled))],
        );
        assert!(!ends_settled(&path), "a run that carried on isn't settled");
    }
}
