//! The run journal, an append-only JSONL log of [`RunEvent`]s, and the run lock. Only the lock
//! holder writes; readers never need the lock.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::mpsc;

use crate::base::clock;
use crate::base::layout::run_files;
use crate::base::names::SessionKey;
use crate::base::paths::create_private_dir_all;
use crate::runs::events::run::RunLifecycle;
use crate::runs::events::{RunEvent, RunEventKind};
use crate::{Note, NoteLevel, RunId, TaskId};

const TAIL_POLL: Duration = Duration::from_millis(50);

const TAIL_BUFFER: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("run journal {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("run {0} is locked by another run host")]
    Locked(RunId),
}

fn io_error(path: &Path, err: impl std::fmt::Display) -> JournalError {
    JournalError::Io {
        path: path.to_owned(),
        message: err.to_string(),
    }
}

/// The format of the journals this build writes and reads. A journal's first line carries it
/// as `format`, beside the first event's own fields, and it goes up whenever a build can no
/// longer read what the one before wrote.
pub const FORMAT: u64 = 2;

/// The key of the format in a journal's first line.
const FORMAT_KEY: &str = "format";

/// Who wrote a journal, by its first line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrittenBy {
    /// Nothing is written yet, or there is no journal.
    NoOne,
    /// A build whose journals this one reads.
    ThisFormat,
    /// An older Argonauts Code, in a format this build can't read. Nothing converts it.
    AnOlderBuild,
    /// A newer Argonauts Code.
    ANewerBuild,
}

/// Who wrote the journal at `path`. A first line with no format number is from before
/// journals carried one, and counts as this build's when its event still reads.
pub fn written_by(path: &Path) -> WrittenBy {
    let Ok(file) = File::open(path) else {
        return WrittenBy::NoOne;
    };
    let first = BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
        .find(|line| !line.trim_ascii().is_empty());
    let Some(first) = first else {
        return WrittenBy::NoOne;
    };
    let format = serde_json::from_slice::<serde_json::Value>(&first)
        .ok()
        .and_then(|line| line.get(FORMAT_KEY).and_then(serde_json::Value::as_u64));
    let reads = || serde_json::from_slice::<RunEvent>(&first).is_ok();
    match format {
        Some(format) if format > FORMAT => WrittenBy::ANewerBuild,
        Some(format) if format < FORMAT => WrittenBy::AnOlderBuild,
        _ if reads() => WrittenBy::ThisFormat,
        _ => WrittenBy::AnOlderBuild,
    }
}

pub struct Journal {
    file: File,
    path: PathBuf,
    next_seq: u64,
}

impl Journal {
    /// Continues the sequence after the last good line. `lock` is proof this process may write
    /// and must outlive the journal.
    pub fn open(path: &Path, lock: &RunLock) -> Result<Self, JournalError> {
        let _ = lock;
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent).map_err(|err| io_error(path, err))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|err| io_error(path, err))?;
        // A crash can leave a torn last line. Start a fresh one so the next event reads back.
        if ends_without_newline(&mut file).map_err(|err| io_error(path, err))? {
            file.write_all(b"\n").map_err(|err| io_error(path, err))?;
        }
        let next_seq = read_all(path)?
            .events
            .last()
            .map_or(0, |event| event.seq + 1);
        Ok(Self {
            file,
            path: path.to_owned(),
            next_seq,
        })
    }

    pub fn append(
        &mut self,
        run: &RunId,
        task: Option<&TaskId>,
        session: Option<&SessionKey>,
        source: &str,
        kind: RunEventKind,
    ) -> Result<RunEvent, JournalError> {
        let event = RunEvent {
            seq: self.next_seq,
            ts: clock::now_utc(),
            run_id: run.clone(),
            task_id: task.cloned(),
            session: session.cloned(),
            source: source.to_owned(),
            kind,
        };
        let mut line = if event.seq == 0 {
            // The journal's first line says which format wrote it.
            let mut first =
                serde_json::to_value(&event).map_err(|err| io_error(&self.path, err))?;
            if let Some(fields) = first.as_object_mut() {
                fields.insert(FORMAT_KEY.to_owned(), FORMAT.into());
            }
            serde_json::to_vec(&first)
        } else {
            serde_json::to_vec(&event)
        }
        .map_err(|err| io_error(&self.path, err))?;
        line.push(b'\n');
        // One write per line, so a crash tears at most the line being written.
        self.file
            .write_all(&line)
            .map_err(|err| io_error(&self.path, err))?;
        self.next_seq += 1;
        Ok(event)
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn ends_without_newline(file: &mut File) -> io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

pub struct ReadBack {
    pub events: Vec<RunEvent>,
    /// Lines that didn't parse, such as a torn last line after a crash, as the 1-based line
    /// number and the parse error. Never fatal.
    pub skipped: Vec<(u64, String)>,
}

pub fn read_all(path: &Path) -> Result<ReadBack, JournalError> {
    let file = File::open(path).map_err(|err| io_error(path, err))?;
    let mut back = ReadBack {
        events: Vec::new(),
        skipped: Vec::new(),
    };
    for (index, line) in BufReader::new(file).split(b'\n').enumerate() {
        let line = line.map_err(|err| io_error(path, err))?;
        if line.trim_ascii().is_empty() {
            continue;
        }
        match serde_json::from_slice::<RunEvent>(&line) {
            Ok(event) => back.events.push(event),
            Err(err) => back.skipped.push((index as u64 + 1, err.to_string())),
        }
    }
    Ok(back)
}

/// From the latest `Run::Snapshot` on, or the whole journal when there is none. Only those lines
/// are parsed, so a resume or reattach costs what happened since the snapshot.
pub fn read_since_snapshot(path: &Path) -> Result<ReadBack, JournalError> {
    let bytes = std::fs::read(path).map_err(|err| io_error(path, err))?;
    let lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    let start = lines
        .iter()
        .rposition(|line| {
            contains(line, br#""event":"snapshot""#)
                && serde_json::from_slice::<RunEvent>(line).is_ok_and(|event| {
                    matches!(event.kind, RunEventKind::Run(RunLifecycle::Snapshot(_)))
                })
        })
        .unwrap_or(0);
    let mut back = ReadBack {
        events: Vec::new(),
        skipped: Vec::new(),
    };
    for (index, line) in lines.iter().enumerate().skip(start) {
        if line.trim_ascii().is_empty() {
            continue;
        }
        match serde_json::from_slice::<RunEvent>(line) {
            Ok(event) => back.events.push(event),
            Err(err) => back.skipped.push((index as u64 + 1, err.to_string())),
        }
    }
    Ok(back)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Follows the journal from `from_seq`. A complete line that doesn't parse, such as an event
/// from a newer Argonauts Code, comes through as a warning `Note`; a torn last line is waited for.
/// A thread does the reading, so no Tokio runtime is needed. It stops when the receiver drops,
/// or at the end of the log once it passed a `Finished` that no `Resumed` follows: a stopped run
/// can be reopened, so a `Finished` is the last event only until then.
pub fn tail(path: &Path, from_seq: u64) -> mpsc::Receiver<RunEvent> {
    let (sender, receiver) = mpsc::channel(TAIL_BUFFER);
    let path = path.to_owned();
    // If the thread can't start, the sender drops with it and the receiver sees the log end.
    let _detached = thread::Builder::new()
        .name("argonauts-journal-tail".to_owned())
        .spawn(move || follow(&path, from_seq, &sender));
    receiver
}

fn follow(path: &Path, from_seq: u64, sender: &mpsc::Sender<RunEvent>) {
    let mut reader: Option<BufReader<File>> = None;
    // Bytes of a line whose newline hasn't been written yet.
    let mut partial = Vec::new();
    let mut finished = false;
    loop {
        if sender.is_closed() {
            return;
        }
        if reader.is_none() {
            reader = File::open(path).ok().map(BufReader::new);
        }
        let mut read_any = false;
        if let Some(file) = reader.as_mut() {
            loop {
                let mut chunk = Vec::new();
                match file.read_until(b'\n', &mut chunk) {
                    Ok(0) => break,
                    Ok(_) => {
                        read_any = true;
                        partial.extend_from_slice(&chunk);
                        if partial.last() != Some(&b'\n') {
                            continue;
                        }
                        let line = std::mem::take(&mut partial);
                        let Some(event) = parse_or_note(&line) else {
                            continue;
                        };
                        if event.seq < from_seq {
                            continue;
                        }
                        match event.kind {
                            RunEventKind::Run(RunLifecycle::Finished { .. }) => finished = true,
                            RunEventKind::Run(RunLifecycle::Resumed(_)) => finished = false,
                            _ => {}
                        }
                        if sender.blocking_send(event).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        reader = None;
                        break;
                    }
                }
            }
        }
        if finished {
            return;
        }
        if !read_any {
            thread::sleep(TAIL_POLL);
        }
    }
}

/// A readable envelope around an unknown event becomes a warning note. `None` when even the
/// envelope is unreadable.
fn parse_or_note(line: &[u8]) -> Option<RunEvent> {
    if let Ok(event) = serde_json::from_slice::<RunEvent>(line) {
        return Some(event);
    }
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    let seq = value.get("seq")?.as_u64()?;
    let run_id = serde_json::from_value(value.get("run_id")?.clone()).ok()?;
    let ts = value
        .get("ts")
        .and_then(serde_json::Value::as_str)
        .and_then(|ts| OffsetDateTime::parse(ts, &Rfc3339).ok())
        .unwrap_or_else(OffsetDateTime::now_utc);
    let kind = value
        .pointer("/kind/type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    Some(RunEvent {
        seq,
        ts,
        run_id,
        task_id: None,
        session: None,
        source: "journal".to_owned(),
        kind: RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: format!(
                "event {seq} ({kind}) was written by a newer Argonauts Code and can't be shown here"
            ),
        }),
    })
}

/// Held for as long as a process may write the journal. Released on drop or process exit.
pub struct RunLock {
    _file: File,
}

impl RunLock {
    /// `None` when another process holds it.
    pub fn try_acquire(run_dir: &Path) -> Result<Option<Self>, JournalError> {
        let path = run_dir.join(run_files::LOCK);
        create_private_dir_all(run_dir).map_err(|err| io_error(&path, err))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| io_error(&path, err))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(io_error(&path, err)),
        }
    }

    /// A held lock means a run host is alive.
    pub fn is_held(run_dir: &Path) -> bool {
        let path = run_dir.join(run_files::LOCK);
        let Ok(file) = File::open(&path) else {
            return false;
        };
        matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::events::run::{ResumeCause, RunOutcome, RunResumed, WaitReason};
    use crate::{Note, NoteLevel};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    fn run() -> RunId {
        RunId::new("run-01").unwrap()
    }

    fn note(text: &str) -> RunEventKind {
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: text.to_owned(),
        })
    }

    fn finished() -> RunEventKind {
        RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        })
    }

    fn texts(events: &[RunEvent]) -> Vec<String> {
        events
            .iter()
            .map(|e| match &e.kind {
                RunEventKind::Note(note) => note.text.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn appends_number_events_from_zero_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let mut journal = Journal::open(&path, &lock).unwrap();
        let task = TaskId::numbered(2);
        let session = SessionKey::numbered(7);
        let first = journal
            .append(
                &run(),
                Some(&task),
                Some(&session),
                "argonauts-code",
                note("a"),
            )
            .unwrap();
        let second = journal
            .append(&run(), None, None, "user", note("b"))
            .unwrap();
        assert_eq!((first.seq, second.seq), (0, 1));
        assert_eq!(first.task_id, Some(task));
        assert_eq!(first.session, Some(session));
        let back = read_all(&path).unwrap();
        assert_eq!(back.events, vec![first, second]);
        assert!(back.skipped.is_empty());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn reopening_continues_after_the_last_good_line_and_skips_torn_ones() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let mut journal = Journal::open(&path, &lock).unwrap();
        journal
            .append(&run(), None, None, "argonauts-code", note("a"))
            .unwrap();
        journal
            .append(&run(), None, None, "argonauts-code", note("b"))
            .unwrap();
        drop(journal);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"seq":2,"ts":"#).unwrap();
        drop(file);

        let mut journal = Journal::open(&path, &lock).unwrap();
        assert_eq!(journal.next_seq(), 2);
        let next = journal
            .append(&run(), None, None, "argonauts-code", note("c"))
            .unwrap();
        assert_eq!(next.seq, 2);
        let back = read_all(&path).unwrap();
        assert_eq!(texts(&back.events), ["a", "b", "c"]);
        assert_eq!(back.skipped.len(), 1);
        assert_eq!(back.skipped[0].0, 3);
    }

    #[test]
    fn reading_a_missing_journal_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_all(&dir.path().join("nope.jsonl")).err().unwrap();
        assert!(matches!(err, JournalError::Io { .. }), "{err}");
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!RunLock::is_held(dir.path()));
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        assert!(RunLock::is_held(dir.path()));
        assert!(RunLock::try_acquire(dir.path()).unwrap().is_none());
        drop(lock);
        assert!(!RunLock::is_held(dir.path()));
        assert!(RunLock::try_acquire(dir.path()).unwrap().is_some());
    }

    fn receive(receiver: &mut mpsc::Receiver<RunEvent>) -> Option<RunEvent> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match receiver.try_recv() {
                Ok(event) => return Some(event),
                Err(mpsc::error::TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
                Err(mpsc::error::TryRecvError::Disconnected) => return None,
            }
        }
        panic!("no event within 5 s");
    }

    #[test]
    fn tail_replays_from_a_seq_then_follows_until_finished() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let mut journal = Journal::open(&path, &lock).unwrap();
        for text in ["a", "b", "c"] {
            journal
                .append(&run(), None, None, "argonauts-code", note(text))
                .unwrap();
        }
        let mut receiver = tail(&path, 1);
        assert_eq!(receive(&mut receiver).unwrap().seq, 1);
        assert_eq!(receive(&mut receiver).unwrap().seq, 2);

        journal
            .append(&run(), None, None, "argonauts-code", note("d"))
            .unwrap();
        let waiting = RunEventKind::Run(RunLifecycle::Waiting {
            reason: WaitReason::Paused,
            waiting_on: Vec::new(),
        });
        journal
            .append(&run(), None, None, "argonauts-code", waiting)
            .unwrap();
        journal
            .append(&run(), None, None, "argonauts-code", finished())
            .unwrap();
        assert_eq!(receive(&mut receiver).unwrap().seq, 3);
        assert_eq!(receive(&mut receiver).unwrap().seq, 4);
        assert_eq!(receive(&mut receiver).unwrap().seq, 5);
        assert!(
            receive(&mut receiver).is_none(),
            "the tail ends after Finished"
        );
    }

    #[test]
    fn tail_follows_a_stopped_run_that_was_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let mut journal = Journal::open(&path, &lock).unwrap();
        let stopped = RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Stopped,
            detail: None,
        });
        let resumed = RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
            cause: ResumeCause::User,
            settings_changed: false,
            cli_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        }));
        for kind in [note("a"), stopped, resumed, note("b")] {
            journal
                .append(&run(), None, None, "argonauts-code", kind)
                .unwrap();
        }
        let mut receiver = tail(&path, 0);
        for seq in 0..4 {
            assert_eq!(receive(&mut receiver).unwrap().seq, seq);
        }

        journal
            .append(&run(), None, None, "argonauts-code", finished())
            .unwrap();
        assert_eq!(receive(&mut receiver).unwrap().seq, 4);
        assert!(
            receive(&mut receiver).is_none(),
            "the tail ends after the last Finished"
        );
    }

    #[test]
    fn a_journal_says_which_format_wrote_it() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        assert_eq!(written_by(&path), WrittenBy::NoOne);
        let mut journal = Journal::open(&path, &lock).unwrap();
        assert_eq!(written_by(&path), WrittenBy::NoOne);
        for kind in [note("a"), note("b")] {
            journal
                .append(&run(), None, None, "argonauts-code", kind)
                .unwrap();
        }
        assert_eq!(written_by(&path), WrittenBy::ThisFormat);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["format"], FORMAT);
        assert!(lines[1].get("format").is_none(), "only the first line says");
        // The number changes nothing for a reader of events.
        assert_eq!(read_all(&path).unwrap().events.len(), 2);
    }

    #[test]
    fn a_journal_in_another_format_is_told_from_one_that_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let event = RunEvent {
            seq: 0,
            ts: clock::now_utc(),
            run_id: run(),
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind: note("a"),
        };
        let mut first = serde_json::to_value(&event).unwrap();
        let write = |first: &serde_json::Value| {
            std::fs::write(&path, format!("\n{first}\n")).unwrap();
            written_by(&path)
        };
        // From before journals carried a number, and its event still reads.
        assert_eq!(write(&first), WrittenBy::ThisFormat);
        first["format"] = (FORMAT - 1).into();
        assert_eq!(write(&first), WrittenBy::AnOlderBuild);
        first["format"] = (FORMAT + 1).into();
        assert_eq!(write(&first), WrittenBy::ANewerBuild);
        // No number, and an event of a kind this build never wrote.
        let old = r#"{"seq":0,"ts":"2026-10-04T07:23:29Z","run_id":"run-01","source":"x","kind":{"type":"hologram","data":{}}}"#;
        std::fs::write(&path, format!("{old}\n")).unwrap();
        assert_eq!(written_by(&path), WrittenBy::AnOlderBuild);
    }

    #[test]
    fn tail_reports_events_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let future = r#"{"seq":1,"ts":"2026-09-29T10:00:00Z","run_id":"run-01","source":"x","kind":{"type":"hologram","data":{}}}"#;
        let known = |seq: u64, kind: RunEventKind| {
            let event = RunEvent {
                seq,
                ts: clock::now_utc(),
                run_id: run(),
                task_id: None,
                session: None,
                source: "argonauts-code".into(),
                kind,
            };
            serde_json::to_string(&event).unwrap()
        };
        let text = format!(
            "{}\n{future}\nnot json at all\n{}\n",
            known(0, note("a")),
            known(2, finished())
        );
        std::fs::write(&path, text).unwrap();
        let mut events = tail(&path, 0);
        let got: Vec<RunEvent> = std::iter::from_fn(|| receive(&mut events)).collect();
        assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![0, 1, 2]);
        let warning = &texts(&got)[1];
        assert!(warning.contains("event 1 (hologram)"), "{warning}");
        assert_eq!(got[1].source, "journal");
    }

    fn snapshot(upto: u64) -> RunEventKind {
        RunEventKind::Run(RunLifecycle::Snapshot(Box::new(
            crate::runs::events::run::RunSnapshot {
                upto,
                engine: serde_json::json!({"next_session": 4}),
                summary: crate::runs::api::StatusSummary::default(),
                open_approvals: Vec::new(),
                running: Vec::new(),
            },
        )))
    }

    #[test]
    fn read_since_snapshot_starts_at_the_latest_one() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let mut journal = Journal::open(&path, &lock).unwrap();
        for kind in [note("a"), snapshot(0), note("b"), snapshot(2), note("c")] {
            journal
                .append(&run(), None, None, "argonauts-code", kind)
                .unwrap();
        }
        let back = read_since_snapshot(&path).unwrap();
        assert_eq!(
            back.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4]
        );

        let plain = dir.path().join("plain.jsonl");
        let mut journal = Journal::open(&plain, &lock).unwrap();
        journal
            .append(&run(), None, None, "argonauts-code", note("a"))
            .unwrap();
        assert_eq!(read_since_snapshot(&plain).unwrap().events.len(), 1);
    }

    #[test]
    fn journal_replay_200k_events_under_2s() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let line = |seq: u64, kind: RunEventKind| {
            serde_json::to_string(&RunEvent {
                seq,
                ts: clock::now_utc(),
                run_id: run(),
                task_id: None,
                session: None,
                source: "argonauts-code".into(),
                kind,
            })
            .unwrap()
        };
        // One serialized note, renumbered, keeps writing the fixture cheap.
        let template = line(0, note("a tool call finished"));
        let mut text = String::with_capacity(200_000 * (template.len() + 8));
        for seq in 0..199_990u64 {
            text.push_str(&template.replacen("\"seq\":0,", &format!("\"seq\":{seq},"), 1));
            text.push('\n');
        }
        text.push_str(&line(199_990, snapshot(199_989)));
        text.push('\n');
        for seq in 199_991..200_000u64 {
            text.push_str(&line(seq, note("after")));
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();

        let started = Instant::now();
        let back = read_since_snapshot(&path).unwrap();
        let took = started.elapsed();
        assert_eq!(back.events.len(), 10);
        assert_eq!(back.events[0].seq, 199_990);
        assert!(took < Duration::from_secs(2), "took {took:?}");
    }

    #[test]
    fn tail_waits_for_a_journal_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let mut receiver = tail(&path, 0);
        thread::sleep(Duration::from_millis(120));
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let mut journal = Journal::open(&path, &lock).unwrap();
        journal
            .append(&run(), None, None, "argonauts-code", note("late"))
            .unwrap();
        assert_eq!(receive(&mut receiver).unwrap().seq, 0);
    }
}
