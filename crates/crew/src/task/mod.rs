//! Tasks: the scheduler over the task graph, and each task's pipeline from its worker's session
//! through checks, review, and merge.

use argonauts_core::PhaseStatus;
use argonauts_core::TaskId;
use argonauts_core::events::run::RunPhase;
use argonauts_core::events::task::{TaskEvent, TaskSpec, TaskState};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::workflow::{Count, Step, StepKind};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::goal::pursue;
use crate::workflow::runner;
use crate::workflow::{Flow, StepCall, StepDone};
use crate::{captain, roles};

pub mod at_start;
pub mod checks;
pub mod machine;
pub mod pipeline;
pub mod prompt;
pub mod recovery;
pub mod scheduler;
pub mod take_over;
pub mod worktree;

mod asked_anyway;
pub(crate) mod claims;
mod contracts;
pub(crate) mod later_try;
mod limit;
pub(crate) mod session;
mod stopped_work;
mod stuck;
pub(crate) mod support;
pub(crate) mod todos;

/// The step input that asks for experiment tasks, which the judge step runs itself.
pub const EXPERIMENTS: &str = "experiments";

/// The step input that builds the goal as one Captain task, owned by the main agent.
pub const GOAL: &str = "goal";

/// The `different_provider_from` value that makes a Captain task's review come from another model
/// provider than its worker.
pub const AUTHOR: &str = "author";

/// `build`: with `input: goal`, the Captain task (`count` independent tries of it when above
/// 1, reviewed by another provider with `different_provider_from: author`); with `input: plan`,
/// every task of the approved graph through its pipeline, in dependency order, within
/// `concurrency` and the limits. `input: experiments` is left to the judge step.
pub async fn build_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    match call.step.input.as_deref() {
        Some(EXPERIMENTS) => Ok(StepDone::skipped(
            "experiment tasks run in the judge step and are never merged into the run",
        )),
        Some(GOAL) => build_goal(host, call).await,
        _ => scheduler::run_tasks(host, call).await,
    }
}

/// Whether a step builds the Captain task rather than the plan's tasks.
pub fn builds_goal(step: &Step) -> bool {
    step.kind == StepKind::Build && step.input.as_deref() == Some(GOAL)
}

async fn build_goal(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    // With a goal around the run, each round builds its own task on what the earlier ones
    // merged.
    let mut spec = {
        let snapshot = host.read(Clone::clone);
        if snapshot.goal.pursuit.is_some() {
            pursue::try_task(&snapshot, host)?
        } else {
            captain::task(&snapshot, host)?
        }
    };
    // `captain.review` may already have given the task its reviewer, and a workflow never takes
    // a review away.
    if call.step.different_provider_from.as_deref() == Some(AUTHOR) && spec.reviewers.is_empty() {
        let worker = spec
            .worker
            .clone()
            .unwrap_or_else(|| host.settings().crew.main.clone());
        spec.reviewers = vec![other_provider_reviewer(host, &worker)?];
    }
    let tries = match call.step.count {
        Some(Count::Fixed(count)) => count.max(1),
        _ => 1,
    };
    if tries == 1 {
        create(host, spec)?;
        return scheduler::run_tasks(host, call).await;
    }
    best_of(host, call, spec, tries).await
}

/// The first reviewer candidate (the reviewer role's, else every available worker) from another
/// provider than the worker, with no role conflict.
fn other_provider_reviewer(host: &Host, worker: &ModelRef) -> EngineResult<ModelRef> {
    let provider = |model: &ModelRef| roles::provider(host, model);
    let worker_provider = provider(worker);
    let mut pool = roles::candidates(host.settings(), RoleName::Reviewer);
    pool.extend(host.services().workers.available());
    pool.into_iter()
        .find(|model| {
            model != worker
                && provider(model) != worker_provider
                && roles::conflict(RoleName::Reviewer, model, Some(worker), &provider).is_none()
        })
        .ok_or_else(|| {
            EngineError::Refused(format!(
                "this workflow reviews the Captain task by another provider than {worker} \
                 ({worker_provider}), and no reviewer from another provider is available"
            ))
        })
}

/// Best of n: independent tries, each its own task, run until their visible checks pass;
/// `captain::pick_try` keeps one, which goes on through review and merge. The others
/// stay on their branches, unmerged.
async fn best_of(
    host: &Host,
    call: &StepCall,
    spec: TaskSpec,
    tries: u32,
) -> EngineResult<StepDone> {
    let mut ids = Vec::new();
    for n in 1..=tries {
        let id = TaskId::new(format!("{}-try-{n}", spec.id))
            .map_err(|err| EngineError::Refused(err.to_string()))?;
        let try_spec = TaskSpec {
            id: id.clone(),
            title: format!("{} (try {n})", spec.title),
            ..spec.clone()
        };
        create(host, try_spec)?;
        ids.push(id);
    }
    contracts::record_phase(host, RunPhase::Build, PhaseStatus::Started, None)?;
    let runs = ids
        .iter()
        .map(|id| pipeline::drive(host, id, &call.at, pipeline::Until::ChecksPassed));
    for result in futures::future::join_all(runs).await {
        result?;
    }
    let snapshot = host.read(Clone::clone);
    let Some(kept) = captain::pick_try(&snapshot, &ids) else {
        if captain::moves_to_crew(&snapshot, host) {
            return Ok(StepDone {
                flow: Flow::Switch(runner::crew_workflow(host)),
                ..StepDone::done("no try passed its checks, so it goes to the crew")
            });
        }
        let summary = format!("none of {tries} tries passed its checks");
        contracts::record_phase(
            host,
            RunPhase::Build,
            PhaseStatus::Failed,
            Some(summary.clone()),
        )?;
        return Ok(StepDone {
            end: argonauts_core::events::workflow::StepEnd::Failed,
            ..StepDone::done(summary)
        });
    };
    let state = pipeline::run_task(host, &kept, &call.at).await?;
    let summary = match state {
        TaskState::Merged => format!("kept {kept} of {tries} tries, merged into the run"),
        other => format!("kept {kept} of {tries} tries, now {other:?}"),
    };
    contracts::record_phase(
        host,
        RunPhase::Build,
        PhaseStatus::Finished,
        Some(summary.clone()),
    )?;
    Ok(StepDone::done(summary))
}

/// Records the task once, as `Waiting`.
pub(crate) fn create(host: &Host, spec: TaskSpec) -> EngineResult<()> {
    let id = spec.id.clone();
    if host.read(|state| state.tasks.get(&id).is_some()) {
        return Ok(());
    }
    support::record_task(host, &id, TaskEvent::Created(Box::new(spec)))?;
    let first = TaskEvent::State {
        from: None,
        to: TaskState::Waiting,
        reason: "created".to_owned(),
    };
    support::record_task(host, &id, first)?;
    Ok(())
}
