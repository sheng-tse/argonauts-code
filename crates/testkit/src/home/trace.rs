//! The app controller's test trace: with `ARGONAUTS_TEST_TRACE=<file>`, it appends every
//! `UiCommand` it receives and every `AppEvent` it sends as a `TraceLine`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::app::{AppEvent, TraceLine, UiCommand};

pub const TRACE_ENV: &str = "ARGONAUTS_TEST_TRACE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace {
    path: PathBuf,
}

impl Trace {
    /// A trace at `path`, which the app creates.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn envs(&self) -> Vec<(String, OsString)> {
        vec![(TRACE_ENV.to_owned(), self.path.clone().into_os_string())]
    }

    /// A missing file is empty; a torn last line is skipped.
    pub fn lines(&self) -> anyhow::Result<Vec<TraceLine>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", self.path.display()));
            }
        };
        Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }

    pub fn commands(&self) -> anyhow::Result<Vec<UiCommand>> {
        Ok(self
            .lines()?
            .into_iter()
            .filter_map(|line| match line {
                TraceLine::In { command, .. } => Some(command),
                TraceLine::Out { .. } => None,
            })
            .collect())
    }

    pub fn events(&self) -> anyhow::Result<Vec<AppEvent>> {
        Ok(self
            .lines()?
            .into_iter()
            .filter_map(|line| match line {
                TraceLine::Out { event, .. } => Some(event),
                TraceLine::In { .. } => None,
            })
            .collect())
    }

    pub fn wait_for_event(
        &self,
        matches: impl Fn(&AppEvent) -> bool,
        timeout: Duration,
    ) -> anyhow::Result<AppEvent> {
        let deadline = crate::pty::deadline::after(timeout);
        loop {
            if let Some(event) = self.events()?.into_iter().find(|event| matches(event)) {
                return Ok(event);
            }
            if Instant::now() >= deadline {
                bail!(
                    "no matching event in {:?}; the trace has {} lines",
                    crate::pty::deadline::scaled(timeout),
                    self.lines()?.len()
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn wait_for_command(
        &self,
        matches: impl Fn(&UiCommand) -> bool,
        timeout: Duration,
    ) -> anyhow::Result<UiCommand> {
        let deadline = crate::pty::deadline::after(timeout);
        loop {
            if let Some(command) = self.commands()?.into_iter().find(|c| matches(c)) {
                return Ok(command);
            }
            if Instant::now() >= deadline {
                bail!(
                    "no matching command in {:?}",
                    crate::pty::deadline::scaled(timeout)
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn reads_commands_and_events_and_skips_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let trace = Trace::new(dir.path().join("trace.jsonl"));
        assert!(trace.lines().unwrap().is_empty());
        let mut file = fs::File::create(trace.path()).unwrap();
        let lines = [
            TraceLine::In {
                t_ms: 1,
                command: UiCommand::Submit {
                    text: "hi".into(),
                    workflow_requested: false,
                    pictures: Vec::new(),
                },
            },
            TraceLine::Out {
                t_ms: 2,
                event: AppEvent::Exit,
            },
        ];
        for line in &lines {
            writeln!(file, "{}", serde_json::to_string(line).unwrap()).unwrap();
        }
        write!(file, "{{\"dir\":\"in\",").unwrap();
        assert_eq!(trace.lines().unwrap(), lines);
        assert_eq!(
            trace.commands().unwrap(),
            [UiCommand::Submit {
                text: "hi".into(),
                workflow_requested: false,
                pictures: Vec::new(),
            }]
        );
        let exit = trace
            .wait_for_event(|e| matches!(e, AppEvent::Exit), Duration::from_millis(50))
            .unwrap();
        assert_eq!(exit, AppEvent::Exit);
        assert!(
            trace
                .wait_for_command(|c| matches!(c, UiCommand::Stop), Duration::from_millis(30))
                .is_err()
        );
    }
}
