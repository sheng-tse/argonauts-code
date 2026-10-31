//! Writes run events to the event log and hands each one to the UI.

use std::fmt;
use std::path::Path;

use argonauts_core::{EventLog, EventLogError, RunEvent, RunEventKind, RunId, TaskId};

/// Gets each event right after it is logged. It runs on Argonauts Code's task, so it should only
/// hand the event off, for example on a channel.
pub type EventSink = Box<dyn FnMut(RunEvent) + Send>;

pub const CONDUCTOR_SOURCE: &str = "argonauts-code";

pub struct Recorder {
    log: EventLog,
    run_id: RunId,
    sink: Option<EventSink>,
}

impl fmt::Debug for Recorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recorder")
            .field("log", &self.log)
            .field("run_id", &self.run_id)
            .field("sink", &self.sink.is_some())
            .finish()
    }
}

impl Recorder {
    pub fn new(log: EventLog, run_id: RunId, sink: Option<EventSink>) -> Self {
        Self { log, run_id, sink }
    }

    /// The sink only sees events that made it into the log.
    pub fn record(
        &mut self,
        task: Option<&TaskId>,
        source: &str,
        kind: RunEventKind,
    ) -> Result<(), EventLogError> {
        let event = RunEvent::now(self.run_id.clone(), task.cloned(), source, kind);
        self.log.append(&event)?;
        if let Some(sink) = self.sink.as_mut() {
            sink(event);
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), EventLogError> {
        self.log.flush()
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn log_path(&self) -> &Path {
        self.log.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::{Note, NoteLevel};
    use std::sync::{Arc, Mutex};

    fn note(text: &str) -> RunEventKind {
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: text.into(),
        })
    }

    #[test]
    fn records_to_log_and_sink_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink: EventSink = Box::new({
            let seen = Arc::clone(&seen);
            move |event| seen.lock().unwrap().push(event)
        });
        let run = RunId::new("run-1").unwrap();
        let task = TaskId::numbered(1);
        let mut recorder = Recorder::new(EventLog::open(&path).unwrap(), run.clone(), Some(sink));
        recorder.record(None, CONDUCTOR_SOURCE, note("a")).unwrap();
        recorder.record(Some(&task), "fake", note("b")).unwrap();
        recorder.flush().unwrap();

        let back = EventLog::read(&path).unwrap();
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.events[1].task_id.as_ref(), Some(&task));
        assert_eq!(back.events[1].source, "fake");
        assert_eq!(*seen.lock().unwrap(), back.events);
        assert_eq!(recorder.run_id(), &run);
        assert_eq!(recorder.log_path(), path);
        assert!(format!("{recorder:?}").contains("sink: true"));
    }

    #[test]
    fn records_without_a_sink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut recorder = Recorder::new(
            EventLog::open(&path).unwrap(),
            RunId::new("run-1").unwrap(),
            None,
        );
        recorder.record(None, CONDUCTOR_SOURCE, note("a")).unwrap();
        assert_eq!(EventLog::read(&path).unwrap().events.len(), 1);
    }
}
