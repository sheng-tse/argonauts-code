//! The run's name, description, and style, recorded as `Run::Named` before anything runs. A
//! run the main agent designs takes its brief's. Any other (Captain, or a template that runs as
//! written) takes intake's one-line summary as its description and its first words as its
//! name. Where intake never ran (the mode was forced, or your words chose it), they come from
//! the main session's account of the work when it handed the request over, else from your
//! request, whose first words may say how to run it and not what it is. Its workflow carries
//! the same name, so no view shows a template's.
//!
//! Intake's schema stays as it is: its answers are checked against it strictly, so a new
//! field would turn every recorded estimate into a mismatch.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use argonauts_core::base::layout;
use argonauts_core::runs::events::run::{RunLifecycle, RunNamed, RunStarted, account_in};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::workflows::workflow::{Workflow, WorkflowRef};

use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::state::RunState;
use crate::workflow::brief;

/// Every run is a workflow. A goal is a condition around it, which the run takes from how it
/// was started (`goal::pursue`), and no longer a kind of run the main agent's CLI picks.
pub fn style(_host: &Host) -> RunStyle {
    RunStyle::Workflow
}

/// The words a run is named from: intake's estimate, else the main session's account of the
/// work in a request it handed over, else your request. Your own words may say how to run the
/// work rather than what it is ("with Codex as the reviewer…").
pub fn words_of(started: &RunStarted) -> &str {
    started
        .estimate
        .as_ref()
        .map(|estimate| estimate.summary.as_str())
        .filter(|summary| !summary.trim().is_empty())
        .or_else(|| account_in(&started.request))
        .unwrap_or(&started.request)
}

/// Names a new run from intake, or from your request, unless it's named already.
pub fn name_from_intake(host: &Host) -> EngineResult<()> {
    let Some((name, description)) = host.read(|state| {
        if state.run.named.is_some() {
            return None;
        }
        let words = words_of(state.run.started.as_ref()?);
        Some((brief::name_from(words), brief::fit_description(words)))
    }) else {
        return Ok(());
    };
    let name = unique_name(host, &name);
    let style = style(host);
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::Named(RunNamed {
            name,
            description,
            style,
            based_on: None,
            done_when: Vec::new(),
        })),
    )?;
    Ok(())
}

/// The folder beside the runs folder where each name a run took is kept: a file per name,
/// holding the folder name of the run that took it. Beside it rather than in it, since
/// whatever lists the runs reads every folder there as a run.
const NAMES: &str = "run-names";

/// `name`, or `name-2`, `name-3`, … when another of this repo's runs goes by it, so the panel
/// and `/resume` never show two runs by one name.
pub fn unique_name(host: &Host, name: &str) -> String {
    let run_dir = &host.repo().run_dir;
    let this_run = run_dir.file_name().and_then(|run| run.to_str());
    match (run_dir.parent(), this_run) {
        (Some(runs), Some(this_run)) => {
            unique_in(runs, &runs.with_file_name(NAMES), this_run, name)
        }
        _ => name.to_owned(),
    }
}

/// [`unique_name`] among the runs in `runs`, for the run whose folder is `this_run`. A name is
/// taken by creating its file in `names`, which only one run can do, so two runs naming
/// themselves at once never both get it. A run from before names were kept there is read
/// from its log.
fn unique_in(runs: &Path, names: &Path, this_run: &str, name: &str) -> String {
    let older = older_names(runs, names, this_run);
    let numbered = (2..10_000).map(|n| brief::with_number(name, n));
    for candidate in std::iter::once(name.to_owned()).chain(numbered) {
        if older.contains(&candidate) {
            continue;
        }
        match take(names, &candidate, this_run) {
            Taken::Ours | Taken::Unknown => return candidate,
            Taken::Others => {}
        }
    }
    name.to_owned()
}

enum Taken {
    Ours,
    Others,
    /// There's nowhere to keep it, so the logs alone decide.
    Unknown,
}

/// Takes `name` for `run` unless another run has it. Creating a file that exists fails, so of
/// two runs taking one name at once, only one does.
fn take(names: &Path, name: &str, run: &str) -> Taken {
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) {
        return Taken::Unknown;
    }
    if std::fs::create_dir_all(names).is_err() {
        return Taken::Unknown;
    }
    let path = names.join(name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            let _ = file.write_all(run.as_bytes());
            Taken::Ours
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::read_to_string(&path) {
                Ok(worker) if worker == run => Taken::Ours,
                _ => Taken::Others,
            }
        }
        Err(_) => Taken::Unknown,
    }
}

/// The name for a workflow an agent's own CLI runs inside this run, `id` the CLI's: its own,
/// or with a number added when the run or another such workflow goes by it, so no list shows
/// two by one name. Its session, on the event that carries it, says which agent runs it.
pub fn inner_workflow_name(state: &RunState, id: &str, name: &str) -> String {
    let mut taken: BTreeSet<&str> = state
        .sessions
        .by_key
        .values()
        .filter_map(|session| session.workflow.as_deref())
        .filter(|workflow| workflow.id != id)
        .map(|workflow| workflow.name.as_str())
        .collect();
    if let Some(named) = &state.run.named {
        taken.insert(named.name.as_str());
    }
    if !taken.contains(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| brief::with_number(name, n))
        .find(|candidate| !taken.contains(candidate.as_str()))
        .unwrap_or_else(|| name.to_owned())
}

/// The names of every run from before names were kept under [`NAMES`], read from their logs.
/// Each is kept there now, so the next run needn't read that log again.
fn older_names(runs: &Path, names: &Path, this_run: &str) -> BTreeSet<String> {
    let kept: BTreeSet<String> = std::fs::read_dir(names)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .collect();
    let Ok(entries) = std::fs::read_dir(runs) else {
        return BTreeSet::new();
    };
    let mut older = BTreeSet::new();
    for dir in entries.filter_map(Result::ok).map(|entry| entry.path()) {
        let Some(run) = dir.file_name().and_then(|run| run.to_str()) else {
            continue;
        };
        if !dir.is_dir() || run == this_run || kept.contains(run) {
            continue;
        }
        if let Some(name) = name_in(&dir.join(layout::run_files::EVENTS)) {
            let _ = take(names, &name, run);
            older.insert(name);
        }
    }
    older
}

/// The newest name in a run's log, from its first lines, where a run is named.
fn name_in(log: &Path) -> Option<String> {
    let file = std::fs::File::open(log).ok()?;
    BufReader::new(file)
        .lines()
        .take(2_000)
        .map_while(Result::ok)
        .filter(|line| line.contains("\"named\""))
        .filter_map(|line| serde_json::from_str::<RunEvent>(&line).ok())
        .filter_map(|event| match event.kind {
            RunEventKind::Run(RunLifecycle::Named(named)) => Some(named.name),
            _ => None,
        })
        .last()
}

/// A template run as written, under the run's own name: the template shows only as
/// `based_on`.
pub fn as_run(host: &Host, from: Option<&WorkflowRef>, mut workflow: Workflow) -> Workflow {
    let Some(named) = host.read(|state| state.run.named.clone()) else {
        return workflow;
    };
    if workflow.based_on.is_none() {
        workflow.based_on = from.map(|from| from.as_str().to_owned());
    }
    workflow.name = named.name;
    workflow.description = named.description;
    workflow
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run folder whose log names the run, as one from before names were kept aside.
    fn old_run(runs: &Path, folder: &str, name: &str) {
        let dir = runs.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        let named = RunEvent {
            seq: 0,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: argonauts_core::RunId::new(folder).unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Run(RunLifecycle::Named(RunNamed {
                name: name.to_owned(),
                description: String::new(),
                style: RunStyle::Workflow,
                based_on: None,
                done_when: Vec::new(),
            })),
        };
        let line = serde_json::to_string(&named).unwrap();
        std::fs::write(dir.join(layout::run_files::EVENTS), format!("{line}\n")).unwrap();
    }

    /// However old the run that took a name, the name stays its own.
    #[test]
    fn an_old_runs_name_is_still_avoided() {
        let state = tempfile::tempdir().unwrap();
        let (runs, names) = (state.path().join("runs"), state.path().join(NAMES));
        old_run(&runs, "run-20260901-000000-aaaa", "add-due-dates");
        for n in 0..31 {
            old_run(
                &runs,
                &format!("run-20261001-0000{n:02}-bbbb"),
                &format!("other-{n}"),
            );
        }
        let unique = |run: &str| unique_in(&runs, &names, run, "add-due-dates");
        assert_eq!(unique("run-20261009-000000-cccc"), "add-due-dates-2");
        // The same run asking again keeps the name it took.
        assert_eq!(unique("run-20261009-000000-cccc"), "add-due-dates-2");
        assert_eq!(unique("run-20261009-000001-dddd"), "add-due-dates-3");
        // Nothing but the runs is left in the runs folder.
        assert_eq!(std::fs::read_dir(&runs).unwrap().count(), 32);
    }

    /// Two runs naming themselves the same words at the same moment get two names.
    #[test]
    fn two_runs_naming_themselves_at_once_get_two_names() {
        let state = tempfile::tempdir().unwrap();
        let (runs, names) = (state.path().join("runs"), state.path().join(NAMES));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let named: Vec<String> = ["run-20261009-000000-aaaa", "run-20261009-000000-bbbb"]
            .into_iter()
            .map(|run| {
                let (runs, names, barrier) = (runs.clone(), names.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    unique_in(&runs, &names, run, "add-due-dates")
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        let mut sorted = named.clone();
        sorted.sort();
        assert_eq!(sorted, ["add-due-dates", "add-due-dates-2"], "{named:?}");
    }

    /// A worker's own workflow named like the run it works in is listed with a number.
    #[test]
    fn an_inner_workflow_never_shares_its_runs_name() {
        let mut state = RunState::default();
        assert_eq!(
            inner_workflow_name(&state, "w1", "add-due-dates-tasks"),
            "add-due-dates-tasks"
        );
        state.run.named = Some(RunNamed {
            name: "add-due-dates-tasks".to_owned(),
            description: "Add due dates to tasks".to_owned(),
            style: RunStyle::Workflow,
            based_on: None,
            done_when: Vec::new(),
        });
        assert_eq!(
            inner_workflow_name(&state, "w1", "add-due-dates-tasks"),
            "add-due-dates-tasks-2"
        );
        assert_eq!(inner_workflow_name(&state, "w1", "other"), "other");
    }
}
