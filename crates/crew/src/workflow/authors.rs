//! Whose work a step must not look at through its own provider, from `different_provider_from`:
//! `author` is whoever wrote the step's input, and a step's name stands for the workers of
//! that step's sessions.

use argonauts_core::names::{ModelRef, WorkflowId};
use argonauts_core::settings::Settings;

use crate::state::RunState;
use crate::workflow::StepCall;
use crate::workflow::kinds::{find_named, item_task};

/// The workers whose providers the step at `call` leaves out. Empty when the step doesn't set
/// `different_provider_from`.
pub fn excluded(state: &RunState, settings: &Settings, call: &StepCall) -> Vec<ModelRef> {
    let Some(from) = call.step.different_provider_from.as_deref() else {
        return Vec::new();
    };
    let mut workers = if from == "author" {
        input_authors(state, call)
    } else {
        step_workers(state, call.workflow(), from)
    };
    if workers.is_empty() {
        // Work with no recorded author is most likely the main agent's.
        workers.push(settings.crew.main.clone());
    }
    workers.sort();
    workers.dedup();
    workers
}

/// Who wrote the step's input: the current task's worker inside `for_each`, the workers of the
/// merged tasks for `diff`, the readers for `goal`, the plan's author for `plan`, or the
/// workers of the step the input names.
fn input_authors(state: &RunState, call: &StepCall) -> Vec<ModelRef> {
    let task = call.item.as_ref().and_then(item_task);
    if let Some(worker) = task
        .and_then(|task| state.tasks.get(&task))
        .and_then(|record| record.worker.clone())
    {
        return vec![worker];
    }
    match call.step.input.as_deref() {
        Some("diff") => state
            .merge
            .merged
            .iter()
            .filter_map(|(task, _)| state.tasks.get(task)?.worker.clone())
            .collect(),
        Some("goal") => state
            .goal
            .readings
            .iter()
            .map(|(_, model, _)| model.clone())
            .collect(),
        Some("experiments") => state
            .plan
            .experiment_reports
            .iter()
            .filter_map(|experiment| state.tasks.get(&experiment.task)?.worker.clone())
            .collect(),
        Some("plan") | None => match state.plan.chosen() {
            Some(chosen) => vec![chosen.worker.clone()],
            None => state
                .plan
                .latest()
                .iter()
                .map(|draft| draft.worker.clone())
                .collect(),
        },
        Some(name) => step_workers(state, call.workflow(), name),
    }
}

/// The workers of every session the step called `name` ran, loops and `for_each` included.
pub fn step_workers(state: &RunState, workflow: &WorkflowId, name: &str) -> Vec<ModelRef> {
    let Some((phase, path)) = state
        .workflows
        .by_id
        .get(workflow)
        .and_then(|record| find_named(&record.workflow, name))
    else {
        return Vec::new();
    };
    state
        .sessions
        .by_key
        .values()
        .filter(|record| {
            record.started.workflow_step.as_ref().is_some_and(|at| {
                at.workflow == *workflow && at.phase == phase && at.path.starts_with(&path)
            })
        })
        .map(|record| record.started.worker.clone())
        .collect()
}
