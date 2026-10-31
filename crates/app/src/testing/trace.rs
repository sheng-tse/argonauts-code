//! `ARGONAUTS_TEST_TRACE=<file>`: every command the controller receives and every event it
//! sends, one `TraceLine` per line, for the scenario tests.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use argonauts_core::app::{AppEvent, TraceLine, UiCommand};

pub(crate) const TRACE_ENV: &str = "ARGONAUTS_TEST_TRACE";

pub(crate) struct Trace {
    file: Option<Mutex<File>>,
    started: Instant,
}

impl Trace {
    /// Off unless the variable names a file.
    pub(crate) fn from_env() -> io::Result<Self> {
        match std::env::var_os(TRACE_ENV) {
            Some(path) if !path.is_empty() => Self::to_file(Path::new(&path)),
            _ => Ok(Self::off()),
        }
    }

    pub(crate) fn to_file(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Some(Mutex::new(file)),
            started: Instant::now(),
        })
    }

    pub(crate) fn off() -> Self {
        Self {
            file: None,
            started: Instant::now(),
        }
    }

    pub(crate) fn command(&self, command: &UiCommand) {
        if self.file.is_some() {
            self.write(&TraceLine::In {
                t_ms: self.elapsed_ms(),
                command: command.clone(),
            });
        }
    }

    pub(crate) fn event(&self, event: &AppEvent) {
        if self.file.is_some() {
            self.write(&TraceLine::Out {
                t_ms: self.elapsed_ms(),
                event: event.clone(),
            });
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn write(&self, line: &TraceLine) {
        let Some(file) = &self.file else {
            return;
        };
        let result = serde_json::to_vec(line)
            .map_err(io::Error::other)
            .and_then(|mut bytes| {
                bytes.push(b'\n');
                let mut file = file
                    .lock()
                    .map_err(|_| io::Error::other("trace lock poisoned"))?;
                file.write_all(&bytes)
            });
        // The trace only serves tests; a failed write must not take the app down.
        if let Err(err) = result {
            tracing::warn!("writing the test trace: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_commands_and_events_as_trace_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");
        let trace = Trace::to_file(&path).unwrap();
        trace.command(&UiCommand::CancelTurn);
        trace.event(&AppEvent::Exit);
        let lines: Vec<TraceLine> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(matches!(
            lines[0],
            TraceLine::In {
                command: UiCommand::CancelTurn,
                ..
            }
        ));
        assert!(matches!(
            lines[1],
            TraceLine::Out {
                event: AppEvent::Exit,
                ..
            }
        ));
    }
}
