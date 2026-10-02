//! A harness script: app events at their times, and waits for commands or for the test.

use argonauts_core::app::AppEvent;
use serde_json::{Value, json};

use super::chat::{mode_idle, status_line, welcome};

#[derive(Debug, Clone, Default)]
pub struct Script {
    lines: Vec<Value>,
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    /// The screen at rest: the header, the status line, and idle mode.
    pub fn idle() -> Self {
        Self::new()
            .at(0, welcome())
            .at(0, status_line())
            .at(0, mode_idle())
    }

    /// Sends `event` `ms` after the start, or after the last wait.
    pub fn at(mut self, ms: u64, event: AppEvent) -> Self {
        let event = serde_json::to_value(&event).expect("an AppEvent writes as JSON");
        self.lines.push(json!({ "at_ms": ms, "event": event }));
        self
    }

    /// Sends every event at the same time, in order.
    pub fn all(self, ms: u64, events: impl IntoIterator<Item = AppEvent>) -> Self {
        events
            .into_iter()
            .fold(self, |script, event| script.at(ms, event))
    }

    /// Sends each event at its own time.
    pub fn timed(self, events: impl IntoIterator<Item = (u64, AppEvent)>) -> Self {
        events
            .into_iter()
            .fold(self, |script, (ms, event)| script.at(ms, event))
    }

    /// Holds the rest until the screen sends a command of this `type`, such as `submit`.
    pub fn await_command(mut self, kind: &str) -> Self {
        self.lines.push(json!({ "await_command": kind }));
        self
    }

    /// Holds the rest until the test calls `Harness::signal(name)`.
    pub fn await_signal(mut self, name: &str) -> Self {
        self.lines.push(json!({ "await_signal": name }));
        self
    }

    /// The harness panics here, as a crash in the app would.
    pub fn panic(mut self, message: &str) -> Self {
        self.lines.push(json!({ "panic": message }));
        self
    }

    pub fn to_jsonl(&self) -> String {
        self.lines.iter().map(|line| format!("{line}\n")).collect()
    }
}
