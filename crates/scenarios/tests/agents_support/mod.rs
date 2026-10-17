//! Shared setup for the workers scenarios: settings files in the test's private homes, a
//! launch that waits for the idle prompt, and waits on the run's journal.

// Each test file uses only some of these.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::TaskId;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::session::{EndReason, SessionEnd, SessionStarted};
use argonauts_core::events::task::TaskState;
use argonauts_core::events::{
    ApprovalEvent, ReviewEvent, RunEvent, RunEventKind, SessionEvent, TaskEvent,
};
use argonauts_core::layout::{self, session_files};
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::paths::{Paths, project_config_path};
use argonauts_core::steps::judgment::ReviewOutput;
use argonauts_scenarios::{COLS, ROWS, Scenario, WAIT, as_set_up};
use argonauts_testkit::{Invocation, Pty, PtyCommand, argonauts_bin, journal};

/// The prompt every task here gets. The recorded Claude Code build run answered this prompt.
pub const TASK: &str =
    "Create a file named hello.txt containing the word hi. Then reply with: done";

/// The footer shows this when the prompt is idle.
pub const IDLE: &str = "for shortcuts";

/// A whole task: the worker, its checks and review, and the merge approval.
pub const TASK_WAIT: Duration = Duration::from_secs(90);

/// `name` is a file in `scenarios/agents/`.
pub fn scenario(name: &str) -> anyhow::Result<Scenario> {
    Scenario::new(&format!("agents/{name}"))
}

pub fn scenario_dir() -> PathBuf {
    argonauts_scenarios::scenario_file("agents")
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
    write_file(
        &paths(scenario).global_config(),
        &as_set_up(&checked_captain(text)),
    )
}

/// These scenarios follow Captain runs, so `/captain` runs in a checked worktree unless the
/// settings say how Captain runs.
fn checked_captain(text: &str) -> String {
    if text.contains("[captain]") {
        text.to_owned()
    } else {
        format!("{text}\n[captain]\nkind = \"checked\"\n")
    }
}

/// `~/.argonauts/models.toml`, where `[[model]]` and `[[cli]]` entries need no trust.
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

/// Starts a Captain task on [`TASK`] from the prompt.
pub fn start_captain(pty: &mut Pty) -> anyhow::Result<()> {
    pty.send(&format!("/captain {TASK}\r"))
}

/// Polls the newest run's journal until `done` holds. Stops early when a task fails.
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
        if let Some(reason) = task_failure(&events) {
            bail!(
                "waiting for {what}, a task stopped: {reason}\n{}",
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

/// Waits until a task reaches its merge approval or is merged, whichever the merge
/// settings give.
pub fn wait_for_finished_task(scenario: &Scenario) -> anyhow::Result<Vec<RunEvent>> {
    wait_for_events(scenario, "finished task", TASK_WAIT, task_finished)
}

pub fn task_finished(events: &[RunEvent]) -> bool {
    events.iter().any(|event| match &event.kind {
        RunEventKind::Task(TaskEvent::State { to, .. }) => *to == TaskState::Merged,
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) => opened.kind == ApprovalKind::Merge,
        _ => false,
    })
}

fn task_failure(events: &[RunEvent]) -> Option<String> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Task(TaskEvent::State { to, reason, .. })
            if matches!(to, TaskState::Failed | TaskState::NeedsYou) =>
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

/// The workers of every worker try, in order.
pub fn try_workers(events: &[RunEvent]) -> Vec<&ModelRef> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Try { worker, .. }) => Some(worker),
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
                if started.worker.agent == worker =>
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
pub fn reviews(events: &[RunEvent]) -> Vec<(&ModelRef, &ReviewOutput)> {
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

/// The fake agent's invocations of `program`, `--version` checks left out.
pub fn invocations_of(scenario: &Scenario, program: &str) -> anyhow::Result<Vec<Invocation>> {
    Ok(scenario
        .clis
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

/// The per-session home made from a CLI's home template.
pub fn session_home(scenario: &Scenario, key: &SessionKey) -> anyhow::Result<PathBuf> {
    Ok(session_dir(scenario, key)?.join(session_files::HOME))
}

/// The home a `keep = "task"` CLI keeps on a task for one role.
pub fn task_home(
    scenario: &Scenario,
    task: &str,
    role: &str,
    cli: &str,
) -> anyhow::Result<PathBuf> {
    let run = journal::latest_run(scenario.repo.home())
        .with_context(|| format!("no run under {}", scenario.repo.home().display()))?;
    let task = TaskId::new(task)?;
    Ok(layout::task_dir(&run, &task)
        .join("homes")
        .join(role)
        .join(cli))
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

/// Waits for the screen to mention the merge approval or the finished task, then saves it
/// for the screenshot review.
pub fn finish_screen(scenario: &Scenario, pty: &Pty, shot: &str) -> anyhow::Result<()> {
    pty.wait_for("(?i)merge|merged", WAIT)?;
    scenario.shot(pty, shot)
}
