//! The engine's `EventSink`: redacts secrets from every string of an event, appends it to the
//! journal, and broadcasts it to the socket's subscribers.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError, mpsc};

use argonauts_core::engine::EventSink;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{Journal, JournalError};
use argonauts_core::names::SessionKey;
use argonauts_core::redact::{redact_json, redact_secrets};
use argonauts_core::{Paths, RunId, TaskId};
use tokio::sync::broadcast;

use crate::live_registry;

pub(crate) struct JournalSink {
    journal: Mutex<Journal>,
    path: PathBuf,
    run: RunId,
    events: broadcast::Sender<RunEvent>,
    sessions: Option<mpsc::Sender<SessionChange>>,
}

/// A session starting or ending, for the thread that keeps the count in `registry.json`.
struct SessionChange {
    key: SessionKey,
    started: bool,
}

impl JournalSink {
    pub(crate) fn new(journal: Journal, run: RunId, events: broadcast::Sender<RunEvent>) -> Self {
        Self {
            path: journal.path().to_owned(),
            journal: Mutex::new(journal),
            run,
            events,
            sessions: None,
        }
    }

    /// Counts sessions into this run host's `registry.json` entry. The count is written on
    /// its own thread, since that takes a file lock and scans processes.
    pub(crate) fn counting_sessions(mut self, home: Paths, pid: u32) -> Self {
        let (changes, received) = mpsc::channel();
        let counter = std::thread::Builder::new()
            .name("argonauts-session-count".to_owned())
            .spawn(move || count_sessions(&home, pid, &received));
        match counter {
            Ok(_) => self.sessions = Some(changes),
            Err(err) => tracing::warn!("starting the session counter: {err}"),
        }
        self
    }

    fn redacted(&self, kind: RunEventKind) -> Result<RunEventKind, JournalError> {
        let failed = |message: String| JournalError::Io {
            path: self.path.clone(),
            message,
        };
        let mut value = serde_json::to_value(&kind)
            .map_err(|err| failed(format!("encoding an event: {err}")))?;
        redact_json(&mut value);
        serde_json::from_value(value)
            .map_err(|err| failed(format!("an event didn't survive redaction: {err}")))
    }

    fn count(&self, event: &RunEvent) {
        let (Some(changes), RunEventKind::Session(change), Some(key)) =
            (&self.sessions, &event.kind, &event.session)
        else {
            return;
        };
        let started = match change {
            SessionEvent::Started(_) => true,
            SessionEvent::Ended(_) => false,
            _ => return,
        };
        let change = SessionChange {
            key: key.clone(),
            started,
        };
        if changes.send(change).is_err() {
            tracing::warn!("the session counter stopped, so registry.json keeps an old count");
        }
    }
}

impl EventSink for JournalSink {
    fn record(
        &self,
        task: Option<&TaskId>,
        session: Option<&SessionKey>,
        source: &str,
        kind: RunEventKind,
    ) -> Result<RunEvent, JournalError> {
        let kind = self.redacted(kind)?;
        let source = redact_secrets(source);
        // Appending and broadcasting under one lock keeps the broadcast in `seq` order, which
        // subscribers rely on: they drop anything below the next `seq` they expect.
        let mut journal = self.journal.lock().unwrap_or_else(PoisonError::into_inner);
        let event = journal.append(&self.run, task, session, &source, kind)?;
        // No subscriber is fine: the journal already has it.
        let _ = self.events.send(event.clone());
        self.count(&event);
        drop(journal);
        Ok(event)
    }
}

/// Runs until the sink is dropped.
fn count_sessions(home: &Paths, pid: u32, changes: &mpsc::Receiver<SessionChange>) {
    let mut running = HashSet::new();
    while let Ok(change) = changes.recv() {
        let changed = if change.started {
            running.insert(change.key)
        } else {
            running.remove(&change.key)
        };
        if !changed {
            continue;
        }
        let count = u32::try_from(running.len()).unwrap_or(u32::MAX);
        if let Err(err) = live_registry::set_sessions(home, pid, count) {
            tracing::warn!("updating the session count in registry.json: {err:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::journal::{RunLock, read_all};
    use argonauts_core::layout::run_files;
    use argonauts_core::{Note, NoteLevel};

    use super::*;

    #[test]
    fn sink_redacts_secrets_before_journal() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let journal = Journal::open(&path, &lock).unwrap();
        let (events, mut subscriber) = broadcast::channel(8);
        let run = RunId::new("run-20260930-010000-0001").unwrap();
        let sink = JournalSink::new(journal, run, events);

        let key = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let kind = RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: format!("the agent printed OPENAI_API_KEY={key} in its output"),
        });
        let recorded = sink.record(None, None, "argonauts-code", kind).unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains(key), "{on_disk}");
        assert!(on_disk.contains("[redacted"), "{on_disk}");
        let broadcast = subscriber.try_recv().unwrap();
        assert_eq!(broadcast, recorded);
        let RunEventKind::Note(note) = &recorded.kind else {
            panic!("{recorded:?}");
        };
        assert!(!note.text.contains(key));
        assert!(note.text.starts_with("the agent printed"));
        assert_eq!(read_all(&path).unwrap().events, vec![recorded]);
    }

    #[test]
    fn sink_records_without_subscribers() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let (events, receiver) = broadcast::channel(8);
        drop(receiver);
        let sink = JournalSink::new(
            Journal::open(&path, &lock).unwrap(),
            RunId::new("run-20260930-010000-0002").unwrap(),
            events,
        );
        let note = |text: &str| {
            RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text: text.to_owned(),
            })
        };
        assert_eq!(
            sink.record(None, None, "argonauts-code", note("a"))
                .unwrap()
                .seq,
            0
        );
        assert_eq!(
            sink.record(None, None, "argonauts-code", note("b"))
                .unwrap()
                .seq,
            1
        );
    }

    #[test]
    fn subscribers_see_concurrent_records_in_seq_order() {
        let dir = tempfile::tempdir().unwrap();
        let lock = RunLock::try_acquire(dir.path()).unwrap().unwrap();
        let path = dir.path().join(run_files::EVENTS);
        let (events, mut subscriber) = broadcast::channel(1024);
        let sink = std::sync::Arc::new(JournalSink::new(
            Journal::open(&path, &lock).unwrap(),
            RunId::new("run-20260930-010000-0003").unwrap(),
            events,
        ));
        let threads: Vec<_> = (0..4)
            .map(|thread| {
                let sink = sink.clone();
                std::thread::spawn(move || {
                    for n in 0..50 {
                        let kind = RunEventKind::Note(Note {
                            level: NoteLevel::Info,
                            text: format!("thread {thread} note {n}"),
                        });
                        sink.record(None, None, "argonauts-code", kind).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let seqs: Vec<u64> = std::iter::from_fn(|| subscriber.try_recv().ok())
            .map(|event| event.seq)
            .collect();
        assert_eq!(seqs, (0..200).collect::<Vec<u64>>());
    }
}
