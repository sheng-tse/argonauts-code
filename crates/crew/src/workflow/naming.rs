//! The run's name, description, and style, recorded as `Run::Named` before anything runs. A
//! run the lead designs takes its brief's. Any other (solo, or a template that runs as
//! written) takes intake's one-line summary as its description and its first words as its
//! name, or your request's when the mode was forced and intake never ran. Its workflow carries
//! the same name, so no view shows a template's.
//!
//! Intake's schema stays as it is: its answers are checked against it strictly, so a new
//! field would turn every recorded triage answer into a mismatch.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::Path;

use argonauts_core::events::run::{RunLifecycle, RunMode, RunNamed};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::workflow::{Workflow, WorkflowRef};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::workflow::{brief, runner};

/// The style a new run takes: `workflows.style` (whose layers already hold the preset's), then
/// what the lead's CLI does. A solo run is a workflow of one.
pub fn style(host: &Host) -> RunStyle {
    if host.read(runner::run_mode) == Some(RunMode::Solo) {
        return RunStyle::Workflow;
    }
    let settings = host.settings();
    settings
        .workflows
        .style_for_lead(&settings.default_agent.worker)
}

/// Names a new run from intake, or from your request, unless it's named already.
pub fn name_from_intake(host: &Host) -> EngineResult<()> {
    let Some((name, description)) = host.read(|state| {
        if state.run.named.is_some() {
            return None;
        }
        let started = state.run.started.as_ref()?;
        let words = started
            .estimate
            .as_ref()
            .map(|estimate| estimate.summary.as_str())
            .filter(|summary| !summary.trim().is_empty())
            .unwrap_or(&started.request);
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

/// How many of this repo's newest runs a new run's name must differ from.
const RECENT_RUNS: usize = 30;

/// `name`, or `name-2`, `name-3`, … when one of this repo's recent runs goes by it, so the
/// panel and `/resume` never show two runs by one name.
pub fn unique_name(host: &Host, name: &str) -> String {
    let taken = recent_names(host);
    if !taken.contains(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| brief::with_number(name, n))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or_else(|| name.to_owned())
}

/// The names of this repo's newest runs but this one.
fn recent_names(host: &Host) -> BTreeSet<String> {
    let run_dir = &host.repo().run_dir;
    let Some(runs) = run_dir.parent() else {
        return BTreeSet::new();
    };
    let Ok(entries) = std::fs::read_dir(runs) else {
        return BTreeSet::new();
    };
    // Run folders are named for when they started, so the newest sort last.
    let mut dirs: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path != run_dir)
        .collect();
    dirs.sort();
    dirs.iter()
        .rev()
        .take(RECENT_RUNS)
        .filter_map(|dir| name_in(&dir.join(layout::run_files::EVENTS)))
        .collect()
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
