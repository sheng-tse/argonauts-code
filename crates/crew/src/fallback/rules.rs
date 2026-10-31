//! Which fallback rules apply to a task, whether an agent is usable again, and the step a
//! fallback happens in.

use time::OffsetDateTime;

use argonauts_core::TaskId;
use argonauts_core::base::names::{AgentId, ModelRef, RoleName};
use argonauts_core::models::limits::{HealthState, PlanUsageReading, PlanUsageStatus};
use argonauts_core::runs::events::task::{TaskSpec, TaskState};
use argonauts_core::settings::fallback::AppliedFallback;

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::machine::Trigger;
use crate::task::pipeline;
use crate::workflow::StepAt;

/// The fallback rules for `role` when `agent` stops, walked from the most specific table.
pub fn applied(host: &Host, role: RoleName, agent_id: &AgentId) -> AppliedFallback {
    host.settings().fallback.applied_to(role, agent_id)
}

/// The rules for this task's worker role, with the agent of `worker`. A Captain task's worker fills
/// the worker role too, as its sessions and the recovery read it.
pub fn applied_for(host: &Host, _task: &TaskSpec, worker: &ModelRef) -> AppliedFallback {
    applied(host, RoleName::Worker, &worker.agent_id())
}

pub fn record(host: &Host, task: &TaskId) -> EngineResult<TaskEntry> {
    host.read(|state| state.tasks.get(task).cloned())
        .ok_or_else(|| EngineError::Refused(format!("{task} is not a task of this run")))
}

/// Who owns the task now: the latest reassignment, else the worker it was planned with.
pub fn worker(record: &TaskEntry) -> Option<ModelRef> {
    record.worker.clone().or_else(|| record.spec.worker.clone())
}

/// Healthy or degraded, and its plan usage not out.
pub fn agent_usable(host: &Host, agent_id: &AgentId) -> bool {
    let services = host.services();
    let health = services.health.state(agent_id).state;
    let plan_usage_ok = services
        .plan_usage
        .get(agent_id)
        .is_none_or(|reading| !plan_usage_out(&reading, host.now()));
    matches!(health, HealthState::Healthy | HealthState::Degraded) && plan_usage_ok
}

/// Whether work may go to `model` now, as a fallback or back to it: its agent is usable, and
/// the model itself isn't out or under test.
pub fn takes_work(host: &Host, model: &ModelRef) -> bool {
    agent_usable(host, &model.agent_id()) && host.services().health.model(model).takes_fallbacks()
}

/// A rejected reading counts until its reset has passed, as the fallback planner reads it.
pub fn plan_usage_out(reading: &PlanUsageReading, now: OffsetDateTime) -> bool {
    reading.status == PlanUsageStatus::Rejected && reading.resets_at.is_none_or(|at| at > now)
}

/// The newest step still running, which a fallback's sessions and approvals belong to.
pub fn current_step(state: &RunState) -> Option<StepAt> {
    state
        .workflows
        .by_id
        .values()
        .flat_map(|workflow| &workflow.steps)
        .filter(|progress| progress.end.is_none())
        .max_by_key(|progress| progress.since)
        .map(|progress| StepAt {
            step: progress.step.clone(),
            round: progress.round,
        })
}

pub fn step_now(host: &Host) -> EngineResult<StepAt> {
    host.read(current_step)
        .ok_or_else(|| EngineError::Refused("no step is running for this fallback".to_owned()))
}

/// Whether a session of this task is running now.
pub fn session_running(state: &RunState, task: &TaskId) -> bool {
    state
        .sessions
        .running()
        .any(|session| session.task.as_ref() == Some(task))
}

/// States where no pipeline is working on the task, so a change can apply at once.
pub fn at_rest(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::NeedsDecision
            | TaskState::NeedsYou
            | TaskState::Deferred
            | TaskState::Failed
            | TaskState::Waiting
            | TaskState::Blocked
    )
}

/// The move that puts a task with a new worker back in line, from where it waited.
pub fn ready_trigger(state: TaskState) -> Option<Trigger> {
    match state {
        TaskState::NeedsDecision => Some(Trigger::Decided),
        TaskState::Failed => Some(Trigger::Retry),
        TaskState::NeedsYou => Some(Trigger::NewWorker),
        _ => None,
    }
}

/// The move that puts a task back to work after its worker or branch changed: toward `Ready`
/// when it waited, back to `Running` when its checks or review were next, so they never run on
/// work the new worker hasn't done.
pub fn back_to_work_trigger(state: TaskState) -> Option<Trigger> {
    match state {
        TaskState::Checking | TaskState::InReview => Some(Trigger::NewWorker),
        other => ready_trigger(other),
    }
}

/// Moves the task back to work after a new worker or a reset branch.
pub fn back_in_line(host: &Host, task: &TaskId, reason: &str) -> EngineResult<()> {
    let state = record(host, task)?.state;
    if let Some(trigger) = back_to_work_trigger(state) {
        pipeline::transition(host, task, trigger, reason)?;
    }
    Ok(())
}

/// Why the task's branch can't be reset now, if it can't: only a task at rest with nothing
/// running on it, and not waiting for an answer its session resumes with.
pub fn refuse_reset(state: &RunState, task: &TaskId) -> Option<String> {
    let record = state.tasks.get(task)?;
    if session_running(state, task) {
        return Some(format!("{task} is running. Pause it, then rewind."));
    }
    match record.state {
        TaskState::Blocked => Some(format!(
            "{task} waits for an answer its session resumes with. Answer it, then rewind."
        )),
        TaskState::TakenOver => Some(format!(
            "You took {task} over. Give it back or finish it, then rewind."
        )),
        TaskState::Merging => Some(format!("{task} is being merged, so it can't be rewound.")),
        state if !at_rest(state) => Some(format!(
            "{task} is {state:?}. Rewind it once it stops and waits for you."
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_worker_puts_waiting_tasks_back_in_line() {
        assert_eq!(
            ready_trigger(TaskState::NeedsDecision),
            Some(Trigger::Decided)
        );
        assert_eq!(ready_trigger(TaskState::Failed), Some(Trigger::Retry));
        assert_eq!(ready_trigger(TaskState::NeedsYou), Some(Trigger::NewWorker));
        assert_eq!(ready_trigger(TaskState::Running), None);
        assert!(at_rest(TaskState::NeedsDecision));
        assert!(!at_rest(TaskState::Running));
        assert!(!at_rest(TaskState::Merged));
    }

    #[test]
    fn a_change_between_the_session_and_merge_goes_back_to_running() {
        for state in [TaskState::Checking, TaskState::InReview] {
            let trigger = back_to_work_trigger(state);
            assert_eq!(trigger, Some(Trigger::NewWorker), "{state:?}");
            let to = trigger.and_then(|trigger| crate::task::machine::next(state, trigger));
            assert_eq!(to, Some(TaskState::Running), "{state:?}");
        }
        assert_eq!(
            back_to_work_trigger(TaskState::NeedsYou),
            Some(Trigger::NewWorker)
        );
        assert_eq!(back_to_work_trigger(TaskState::Merging), None);
        assert_eq!(back_to_work_trigger(TaskState::Running), None);
    }
}
