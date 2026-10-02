//! Whose work a step must not look at through its own family, from `different_family_from`:
//! `author` is whoever wrote the step's input, and a step's name stands for the workers of
//! that step's sessions.

use argonauts_core::names::{WorkerRef, WorkflowId};
use argonauts_core::settings::Settings;

use crate::state::RunState;
use crate::workflow::StepCall;
use crate::workflow::kinds::{find_named, item_card};

/// The workers whose families the step at `call` leaves out. Empty when the step doesn't set
/// `different_family_from`.
pub fn excluded(state: &RunState, settings: &Settings, call: &StepCall) -> Vec<WorkerRef> {
    let Some(from) = call.step.different_family_from.as_deref() else {
        return Vec::new();
    };
    let mut workers = if from == "author" {
        input_authors(state, call)
    } else {
        step_workers(state, call.workflow(), from)
    };
    if workers.is_empty() {
        // Work with no recorded author is most likely the default agent's.
        workers.push(settings.default_agent.clone());
    }
    workers.sort();
    workers.dedup();
    workers
}

/// Who wrote the step's input: the current card's owner inside `for_each`, the owners of the
/// merged cards for `diff`, the restaters for `goal`, the plan's author for `plan`, or the
/// workers of the step the input names.
fn input_authors(state: &RunState, call: &StepCall) -> Vec<WorkerRef> {
    let card = call.item.as_ref().and_then(item_card);
    if let Some(owner) = card
        .and_then(|card| state.cards.get(&card))
        .and_then(|record| record.owner.clone())
    {
        return vec![owner];
    }
    match call.step.input.as_deref() {
        Some("diff") => state
            .integration
            .merged
            .iter()
            .filter_map(|(card, _)| state.cards.get(card)?.owner.clone())
            .collect(),
        Some("goal") => state
            .goal
            .restated
            .iter()
            .map(|(_, worker, _)| worker.clone())
            .collect(),
        Some("spikes") => state
            .plan
            .spike_reports
            .iter()
            .filter_map(|spike| state.cards.get(&spike.card)?.owner.clone())
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
pub fn step_workers(state: &RunState, workflow: &WorkflowId, name: &str) -> Vec<WorkerRef> {
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
