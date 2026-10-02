//! What a finished step decided for the run, read back from the log. `StepFinished` records
//! only how a step ended, so a run that stopped right after a decision (you abandoned it, chose
//! to run it solo, or it landed) finds the decision here instead of carrying on without it.

use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind};
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::workflow::{StepKind, WorkflowRef};

use crate::engine::host::Host;
use crate::integrate::merge;
use crate::solo;
use crate::state::RunState;
use crate::state::workflow::{StepProgress, WorkflowRecord};
use crate::workflow::conditions::GOAL_APPROVED;
use crate::workflow::{Flow, runner};

/// What the workflow's finished top-level steps decided. The runner stops at the first step
/// that decides anything, so at most one of them did.
pub fn workflow(state: &RunState, host: &Host, record: &WorkflowRecord) -> Flow {
    first(
        record
            .steps
            .iter()
            .filter(|progress| progress.step.path.len() == 1)
            .map(|progress| step(state, host, record, progress)),
    )
}

/// The first top-level step of the workflow that failed.
pub fn failed(record: &WorkflowRecord) -> Option<&StepProgress> {
    record
        .steps
        .iter()
        .find(|progress| progress.step.path.len() == 1 && progress.end == Some(StepEnd::Failed))
}

/// What one finished round decided. A round that is still running, or one logged before
/// rounds were dated, decided nothing yet.
pub fn step(
    state: &RunState,
    host: &Host,
    record: &WorkflowRecord,
    progress: &StepProgress,
) -> Flow {
    let Some(ended_at) = progress.ended_at else {
        return Flow::Continue;
    };
    if progress.end == Some(StepEnd::Failed) && goal_out_of_rounds(state, record, progress) {
        return Flow::Finish(RunOutcome::Stopped);
    }
    if progress.end != Some(StepEnd::Done) {
        return Flow::Continue;
    }
    let within = |seq: u64| seq > progress.since && seq < ended_at;
    match progress.kind {
        StepKind::Checkpoint => checkpoint(state, host, within),
        // A merge step always ends the run: landed, a pull request, abandoned, or kept.
        StepKind::Merge => {
            Flow::Finish(merge::ended(&state.integration).unwrap_or(RunOutcome::BranchLeft))
        }
        StepKind::Build => build(state, host, record),
        StepKind::LoopUntil | StepKind::ForEach | StepKind::FanOut => {
            let depth = progress.step.path.len() + 1;
            first(
                record
                    .steps
                    .iter()
                    .filter(|inner| {
                        inner.step.phase == progress.step.phase
                            && inner.step.path.len() == depth
                            && inner.step.path.starts_with(&progress.step.path)
                            && within(inner.since)
                    })
                    .map(|inner| step(state, host, record, inner)),
            )
        }
        StepKind::Workflow => first(
            state
                .workflows
                .by_id
                .values()
                .filter(|child| {
                    child.parent.as_ref() == Some(&progress.step) && within(child.since)
                })
                .map(|child| workflow(state, host, child)),
        ),
        _ => Flow::Continue,
    }
}

/// A goal loop that ran out of rounds stops the run, since nothing after it can go on without
/// an approved goal. One that failed because a step inside it failed fails the run instead.
fn goal_out_of_rounds(state: &RunState, record: &WorkflowRecord, progress: &StepProgress) -> bool {
    let goal_loop = progress.kind == StepKind::LoopUntil
        && record
            .step_definition(&progress.step)
            .is_some_and(|step| step.until.as_deref() == Some(GOAL_APPROVED));
    if !goal_loop || state.goal.approved.is_some() {
        return false;
    }
    let depth = progress.step.path.len() + 1;
    !record.steps.iter().any(|inner| {
        inner.step.phase == progress.step.phase
            && inner.step.path.len() == depth
            && inner.step.path.starts_with(&progress.step.path)
            && inner.since > progress.since
            && inner.end == Some(StepEnd::Failed)
    })
}

fn first(mut flows: impl Iterator<Item = Flow>) -> Flow {
    flows
        .find(|flow| *flow != Flow::Continue)
        .unwrap_or(Flow::Continue)
}

/// The goal or plan checkpoint's last answer in the round.
fn checkpoint(state: &RunState, host: &Host, within: impl Fn(u64) -> bool) -> Flow {
    let answered = state
        .checkpoints
        .by_id
        .values()
        .filter(|record| {
            matches!(
                record.opened.kind,
                CheckpointKind::Goal | CheckpointKind::Plan
            ) && record.withdrawn.is_none()
                && within(record.since)
        })
        .filter_map(|record| Some((record.since, record.opened.kind, record.decided.as_ref()?)))
        .max_by_key(|(since, _, _)| *since);
    match answered {
        Some((_, _, decided)) if decided.action == AnswerAction::Abandon => {
            Flow::Finish(RunOutcome::Abandoned)
        }
        Some((_, CheckpointKind::Plan, decided))
            if decided.action == AnswerAction::SwitchToSolo =>
        {
            Flow::Switch(WorkflowRef::new(runner::solo_workflow(host.settings())))
        }
        _ => Flow::Continue,
    }
}

/// A solo workflow's build hands the run to the crew when its card escalated. The crew's own
/// build never switches again.
fn build(state: &RunState, host: &Host, record: &WorkflowRecord) -> Flow {
    let to = runner::crew_workflow(host);
    if record.from.as_ref() != Some(&to)
        && runner::in_solo_workflow(state, &record.id)
        && solo::escalates_to_crew(state, host)
    {
        Flow::Switch(to)
    } else {
        Flow::Continue
    }
}
