//! Shared steps for the limits scenarios: a fake agent script with its reset times filled in,
//! your settings, starting a run, answering questions, and reading the journal and the fake log.

// Each test file uses only some of these.
#![allow(dead_code, unused_imports)]

mod events;
mod fakes;
mod probe;
mod questions;
mod render;

use std::fs;
use std::ops::Deref;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::events::RunEvent;
use argonauts_core::paths::Paths;
use argonauts_scenarios::{Scenario, WAIT, as_set_up};
use argonauts_testkit::Pty;

pub use events::*;
pub use fakes::*;
pub use probe::*;
pub use questions::*;
pub use render::{RESET_LATER, RESET_SOON, SOON, render as render_scenario, reset_in_soon};

/// The prompt every task here gets. The recorded build runs of all three agents answered it.
pub const TASK: &str =
    "Create a file named hello.txt containing the word hi. Then reply with: done";

/// The footer shows this when the prompt is idle.
pub const IDLE: &str = "for shortcuts";

/// A whole task: its sessions, a fallback, and the merge approval.
pub const LONG_WAIT: Duration = Duration::from_secs(90);

/// The app checks waiting runs on start and once a minute, so a wake can take that long after
/// the time it waits for.
pub const WAKE_WAIT: Duration = Duration::from_secs(150);

/// A scenario whose fake agent script lives in a temp folder, rendered from
/// `scenarios/limits/<name>.toml`.
pub struct Limits {
    pub scenario: Scenario,
    _rendered: tempfile::TempDir,
}

impl Deref for Limits {
    type Target = Scenario;

    fn deref(&self) -> &Scenario {
        &self.scenario
    }
}

/// Sets up `scenarios/limits/<name>.toml` with `settings` as your global config, the layer
/// that needs no trust, so safety keys such as `agents.*.binary` apply too. Writing the file
/// also skips the guided setup.
pub fn setup(name: &str, settings: &str) -> anyhow::Result<Limits> {
    let rendered = tempfile::tempdir().context("creating a folder for the fake script")?;
    let script = render::render(name, rendered.path())?;
    let scenario = Scenario::from_file(&script)?;
    let limits = Limits {
        scenario,
        _rendered: rendered,
    };
    limits.write_settings(settings)?;
    Ok(limits)
}

impl Limits {
    pub fn paths(&self) -> Paths {
        Paths::new(self.repo.home())
    }

    /// Replaces your global config, for settings that name files the scenario made. These
    /// scenarios follow Captain runs, so Captain is checked unless the settings say how it runs.
    /// Their fake CLIs write files directly, with none of their edit tools' events, so every
    /// new file counts as their work unless the settings say what a commit takes.
    pub fn write_settings(&self, settings: &str) -> anyhow::Result<()> {
        let mut settings = settings.to_owned();
        if !settings.contains("[captain]") {
            settings.push_str("\n[captain]\nkind = \"checked\"\n");
        }
        if !settings.contains("[commit]") {
            settings.push_str("\n[commit]\nunclaimed = \"keep\"\n");
        }
        write_file(&self.paths().global_config(), &as_set_up(&settings))
    }

    /// The interactive app, once its prompt is idle.
    pub fn open(&self) -> anyhow::Result<Pty> {
        let pty = self.launch(&[])?;
        pty.wait_for(IDLE, WAIT)?;
        Ok(pty)
    }

    /// The interactive app with a Captain run started on [`TASK`].
    pub fn start(&self) -> anyhow::Result<Pty> {
        let mut pty = self.open()?;
        pty.send(&format!("/captain {TASK}\r"))?;
        Ok(pty)
    }

    /// `argonauts run` on [`TASK`], which keeps the task's branch at the end instead of asking.
    pub fn run(&self, extra: &[&str]) -> anyhow::Result<Pty> {
        let mut args = vec!["run", "--merge", "leave"];
        args.extend_from_slice(extra);
        args.push(TASK);
        self.launch(&args)
    }

    /// The newest run's journal, empty before the first run starts.
    pub fn events(&self) -> Vec<RunEvent> {
        self.latest_run_events().unwrap_or_default()
    }

    /// Polls the newest run's journal until `done` holds.
    pub fn wait_for(
        &self,
        what: &str,
        timeout: Duration,
        done: impl Fn(&[RunEvent]) -> bool,
    ) -> anyhow::Result<Vec<RunEvent>> {
        let deadline = argonauts_testkit::deadline::after(timeout);
        loop {
            let events = self.events();
            if done(&events) {
                return Ok(events);
            }
            if Instant::now() >= deadline {
                bail!(
                    "no {what} in {timeout:?}; the journal has {} events, ending with\n{}",
                    events.len(),
                    last_events(&events)
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

pub fn write_file(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// A path or other text as a quoted TOML string.
pub fn toml_string(text: &str) -> String {
    // JSON's string escapes are all valid in a TOML basic string.
    serde_json::Value::String(text.to_owned()).to_string()
}

/// The last few events, one line each, for a failure message.
fn last_events(events: &[RunEvent]) -> String {
    let skip = events.len().saturating_sub(12);
    events[skip..]
        .iter()
        .map(|event| {
            let line = format!("{}: {:?}", event.seq, event.kind);
            line.chars().take(200).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
