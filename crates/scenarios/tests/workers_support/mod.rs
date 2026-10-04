//! Shared setup for the workers scenarios: settings files in the test's private homes, a
//! launch that waits for the idle prompt, and waits on the run's journal.

// Each test file uses only some of these.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::CardId;
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::events::session::{EndReason, SessionEnd, SessionStarted};
use argonauts_core::events::{
    CardEvent, CheckpointEvent, ReviewEvent, RunEvent, RunEventKind, SessionEvent,
};
use argonauts_core::layout::{self, session_files};
use argonauts_core::names::{SessionKey, WorkerRef};
use argonauts_core::paths::{Paths, project_config_path};
use argonauts_core::steps::judgment::ReviewOutput;
use argonauts_scenarios::{COLS, ROWS, Scenario, WAIT};
use argonauts_testkit::{Invocation, Pty, PtyCommand, argonauts_bin, journal};

/// The task every card here gets. The recorded Claude Code build run answered this prompt.
pub const TASK: &str =
    "Create a file named hello.txt containing the word hi. Then reply with: done";

/// The footer shows this when the prompt is idle.
pub const IDLE: &str = "for shortcuts";

/// A whole card: the owner, its gates and review, and the merge checkpoint.
pub const CARD_WAIT: Duration = Duration::from_secs(90);

/// `name` is a file in `scenarios/workers/`.
pub fn scenario(name: &str) -> anyhow::Result<Scenario> {
    Scenario::new(&format!("workers/{name}"))
}

pub fn scenario_dir() -> PathBuf {
    argonauts_scenarios::scenario_file("workers")
}

/// A path or other text as a quoted TOML string.
pub fn toml_string(text: &str) -> String {
    // JSON's string escapes are all valid in a TOML basic string.
    serde_json::Value::String(text.to_owned()).to_string()
}

pub fn toml_path(path: &Path) -> String {
    toml_string(&path.display().to_string())
}

pub fn write_file(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

pub fn paths(scenario: &Scenario) -> Paths {
    Paths::new(scenario.repo.home())
}

/// `~/.argonauts/config.toml`, the layer the app calls "everywhere". It needs no trust, so
/// safety keys such as `endpoints.*.base_url` and `agents.*.binary` go here.
pub fn write_global_config(scenario: &Scenario, text: &str) -> anyhow::Result<()> {
    write_file(&paths(scenario).global_config(), &checked_solo(text))
}

/// These scenarios follow solo runs, so `/solo` runs in a checked copy unless the settings say
/// how solo runs.
fn checked_solo(text: &str) -> String {
    if text.contains("[solo]") {
        text.to_owned()
    } else {
        format!("{text}\n[solo]\nstyle = \"checked\"\n")
    }
}

/// `~/.argonauts/models.toml`, where `[[model]]` and `[[harness]]` entries need no trust.
pub fn write_global_models(scenario: &Scenario, text: &str) -> anyhow::Result<()> {
    write_file(&paths(scenario).global_models(), text)
}

/// `<repo>/.argonauts/config.toml`, the layer the app calls "this project".
pub fn write_project_config(scenario: &Scenario, text: &str) -> anyhow::Result<()> {
    write_file(&project_config_path(scenario.repo.root()), text)
}

/// Launches the app with `extra` added to its environment, and waits for the idle prompt.
pub fn launch_ready(
    scenario: &Scenario,
    args: &[&str],
    extra: &[(&str, &str)],
) -> anyhow::Result<Pty> {
    let binary = argonauts_bin().context("finding the argonauts binary")?;
    let mut command = PtyCommand::new(binary)
        .args(args.iter().copied())
        .cwd(scenario.repo.root())
        .envs(scenario.envs())
        .env_remove("ARGONAUTS_SESSION")
        .size(ROWS, COLS);
    for (name, value) in extra {
        command = command.env(name, *value);
    }
    let pty = Pty::spawn(command)?;
    pty.wait_for(IDLE, WAIT)?;
    Ok(pty)
}

/// Starts a solo card on [`TASK`] from the prompt.
pub fn start_solo(pty: &mut Pty) -> anyhow::Result<()> {
    pty.send(&format!("/solo {TASK}\r"))
}

/// Polls the newest run's journal until `done` holds. Stops early when a card fails.
pub fn wait_for_events(
    scenario: &Scenario,
    what: &str,
    timeout: Duration,
    done: impl Fn(&[RunEvent]) -> bool,
) -> anyhow::Result<Vec<RunEvent>> {
    let deadline = Instant::now() + timeout;
    loop {
        // No run yet reads as no events.
        let events = scenario.latest_run_events().unwrap_or_default();
        if done(&events) {
            return Ok(events);
        }
        if let Some(reason) = card_failure(&events) {
            bail!(
                "waiting for {what}, a card stopped: {reason}\n{}",
                last_events(&events)
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "no {what} in {timeout:?}; the journal has {} events\n{}",
                events.len(),
                last_events(&events)
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Waits until a card reaches its merge checkpoint or is integrated, whichever the merge
/// settings lead to.
pub fn wait_for_finished_card(scenario: &Scenario) -> anyhow::Result<Vec<RunEvent>> {
    wait_for_events(scenario, "finished card", CARD_WAIT, card_finished)
}

pub fn card_finished(events: &[RunEvent]) -> bool {
    events.iter().any(|event| match &event.kind {
        RunEventKind::Card(CardEvent::State { to, .. }) => *to == CardState::Integrated,
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
            opened.kind == CheckpointKind::Merge
        }
        _ => false,
    })
}

fn card_failure(events: &[RunEvent]) -> Option<String> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Card(CardEvent::State { to, reason, .. })
            if matches!(to, CardState::Failed | CardState::Escalated) =>
        {
            Some(format!("{to:?}: {reason}"))
        }
        _ => None,
    })
}

/// The last few events, one line each, for a failure message.
fn last_events(events: &[RunEvent]) -> String {
    events
        .iter()
        .rev()
        .take(12)
        .rev()
        .map(|event| {
            let line = format!("{:?}", event.kind);
            line.chars().take(160).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The workers of every owner attempt, in order.
pub fn attempt_workers(events: &[RunEvent]) -> Vec<&WorkerRef> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Card(CardEvent::Attempt { worker, .. }) => Some(worker),
            _ => None,
        })
        .collect()
}

/// Every session started for `worker` (the part before any `/`), with its key.
pub fn sessions_of<'a>(
    events: &'a [RunEvent],
    worker: &str,
) -> Vec<(Option<&'a SessionKey>, &'a SessionStarted)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.worker.worker == worker =>
            {
                Some((event.session.as_ref(), started.as_ref()))
            }
            _ => None,
        })
        .collect()
}

/// How the session `key` ended, if it has.
pub fn session_end<'a>(events: &'a [RunEvent], key: &SessionKey) -> Option<&'a SessionEnd> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Ended(end)) if event.session.as_ref() == Some(key) => {
            Some(end.as_ref())
        }
        _ => None,
    })
}

/// Asserts `worker` ran at least one session and every one of them completed.
pub fn assert_sessions_completed(events: &[RunEvent], worker: &str) {
    let sessions = sessions_of(events, worker);
    assert!(!sessions.is_empty(), "no {worker} session was started");
    for (key, _) in sessions {
        let key = key.unwrap_or_else(|| panic!("a {worker} session has no key"));
        let end = session_end(events, key)
            .unwrap_or_else(|| panic!("the {worker} session {key:?} never ended"));
        assert_eq!(end.reason, EndReason::Completed, "{end:?}");
    }
}

/// Every review, with its reviewer.
pub fn reviews(events: &[RunEvent]) -> Vec<(&WorkerRef, &ReviewOutput)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer, review, ..
            }) => Some((reviewer, review)),
            _ => None,
        })
        .collect()
}

/// The fake vendor's invocations of `program`, `--version` checks left out.
pub fn invocations_of(scenario: &Scenario, program: &str) -> anyhow::Result<Vec<Invocation>> {
    Ok(scenario
        .vendors
        .invocations()?
        .into_iter()
        .filter(|call| call.argv0 == program && !call.args.iter().any(|arg| arg == "--version"))
        .collect())
}

/// The folder of the session `key` in the newest run.
pub fn session_dir(scenario: &Scenario, key: &SessionKey) -> anyhow::Result<PathBuf> {
    let run = journal::latest_run(scenario.repo.home())
        .with_context(|| format!("no run under {}", scenario.repo.home().display()))?;
    Ok(layout::session_dir(&run, key))
}

/// The per-session home made from a harness's home template.
pub fn session_home(scenario: &Scenario, key: &SessionKey) -> anyhow::Result<PathBuf> {
    Ok(session_dir(scenario, key)?.join(session_files::HOME))
}

/// The home a `keep = "card"` harness keeps on a card for one role.
pub fn card_home(
    scenario: &Scenario,
    card: &str,
    role: &str,
    harness: &str,
) -> anyhow::Result<PathBuf> {
    let run = journal::latest_run(scenario.repo.home())
        .with_context(|| format!("no run under {}", scenario.repo.home().display()))?;
    let card = CardId::new(card)?;
    Ok(layout::card_dir(&run, &card)
        .join("homes")
        .join(role)
        .join(harness))
}

/// Every file under `dir` with its text, sorted by path. A file that isn't UTF-8 reads empty.
pub fn files_under(dir: &Path) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in
            fs::read_dir(&folder).with_context(|| format!("listing {}", folder.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let text = fs::read_to_string(&path).unwrap_or_default();
                files.push((path, text));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Waits for the screen to mention the merge checkpoint or the finished card, then saves it
/// for the screenshot review.
pub fn finish_screen(scenario: &Scenario, pty: &Pty, shot: &str) -> anyhow::Result<()> {
    pty.wait_for("(?i)merge|integrated", WAIT)?;
    scenario.shot(pty, shot)
}
