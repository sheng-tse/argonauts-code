//! Finding events in a run's journal.

use argonauts_core::TaskId;
use argonauts_core::events::agent_status::FallbackPlan;
use argonauts_core::events::approval::{ApprovalKind, ApprovalOpened};
use argonauts_core::events::run::{RunResumed, WaitReason};
use argonauts_core::events::session::{EndReason, SessionEnd, SessionStarted};
use argonauts_core::events::task::{TaskState, TryCause};
use argonauts_core::events::{
    ApprovalEvent, FallbackEvent, RunEvent, RunEventKind, RunLifecycle, SessionEvent, TaskEvent,
};
use argonauts_core::names::ModelRef;

/// Everything `pick` finds, in journal order.
pub fn find<'a, T>(events: &'a [RunEvent], pick: impl Fn(&'a RunEventKind) -> Option<T>) -> Vec<T> {
    events
        .iter()
        .filter_map(|event| pick(&event.kind))
        .collect()
}

/// The `seq` of the first event `pick` accepts.
pub fn seq_of(events: &[RunEvent], pick: impl Fn(&RunEventKind) -> bool) -> Option<u64> {
    events
        .iter()
        .find(|event| pick(&event.kind))
        .map(|event| event.seq)
}

/// Whether both happened, `first` before `then`.
pub fn before(first: Option<u64>, then: Option<u64>) -> bool {
    matches!((first, then), (Some(first), Some(then)) if first < then)
}

/// The first task of the run.
pub fn first_task(events: &[RunEvent]) -> Option<TaskId> {
    find(events, |kind| match kind {
        RunEventKind::Task(TaskEvent::Created(spec)) => Some(spec.id.clone()),
        _ => None,
    })
    .into_iter()
    .next()
}

/// The latest approval of `kind` that opened.
pub fn opened(events: &[RunEvent], kind: ApprovalKind) -> Option<&ApprovalOpened> {
    find(events, |event| match event {
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
            Some(opened.as_ref())
        }
        _ => None,
    })
    .pop()
}

/// The `seq` of the first approval of `kind` that opened.
pub fn opened_at(events: &[RunEvent], kind: ApprovalKind) -> Option<u64> {
    seq_of(
        events,
        |event| matches!(event, RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind),
    )
}

pub fn has_opened(kind: ApprovalKind) -> impl Fn(&[RunEvent]) -> bool {
    move |events| opened(events, kind).is_some()
}

pub fn fallback_plans(events: &[RunEvent]) -> Vec<&FallbackPlan> {
    find(events, |kind| match kind {
        RunEventKind::Fallback(FallbackEvent::Needed(plan)) => Some(plan),
        _ => None,
    })
}

/// Who each fallback went to, in order.
pub fn fell_back(events: &[RunEvent]) -> Vec<&FallbackEvent> {
    find(events, |kind| match kind {
        RunEventKind::Fallback(event @ FallbackEvent::FellBack { .. }) => Some(event),
        _ => None,
    })
}

pub fn fell_back_to(worker: &'static str) -> impl Fn(&[RunEvent]) -> bool {
    move |events| {
        fell_back(events)
            .iter()
            .any(|event| matches!(event, FallbackEvent::FellBack { to, .. } if to.agent == worker))
    }
}

/// Every worker try with its worker and cause.
pub fn tries(events: &[RunEvent]) -> Vec<(&ModelRef, TryCause)> {
    find(events, |kind| match kind {
        RunEventKind::Task(TaskEvent::Try { worker, cause, .. }) => Some((worker, *cause)),
        _ => None,
    })
}

pub fn has_try(worker: &'static str, cause: TryCause) -> impl Fn(&[RunEvent]) -> bool {
    move |events| {
        tries(events)
            .iter()
            .any(|(who, why)| who.agent == worker && *why == cause)
    }
}

pub fn sessions_started(events: &[RunEvent]) -> Vec<&SessionStarted> {
    find(events, |kind| match kind {
        RunEventKind::Session(SessionEvent::Started(started)) => Some(started.as_ref()),
        _ => None,
    })
}

pub fn session_ends(events: &[RunEvent]) -> Vec<&SessionEnd> {
    find(events, |kind| match kind {
        RunEventKind::Session(SessionEvent::Ended(end)) => Some(end.as_ref()),
        _ => None,
    })
}

pub fn ended_with(reason: impl Fn(&EndReason) -> bool) -> impl Fn(&[RunEvent]) -> bool {
    move |events| session_ends(events).iter().any(|end| reason(&end.reason))
}

/// How the sessions of `worker` ended, in order.
pub fn ends_of<'a>(events: &'a [RunEvent], worker: &str) -> Vec<&'a SessionEnd> {
    let keys: Vec<_> = events
        .iter()
        .filter(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => started.worker.agent == worker,
            _ => false,
        })
        .filter_map(|event| event.session.clone())
        .collect();
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Ended(end))
                if event.session.as_ref().is_some_and(|key| keys.contains(key)) =>
            {
                Some(end.as_ref())
            }
            _ => None,
        })
        .collect()
}

/// Whether a session of `worker` completed.
pub fn completed_by(worker: &'static str) -> impl Fn(&[RunEvent]) -> bool {
    move |events| {
        ends_of(events, worker)
            .iter()
            .any(|end| end.reason == EndReason::Completed)
    }
}

pub fn task_states(events: &[RunEvent]) -> Vec<TaskState> {
    find(events, |kind| match kind {
        RunEventKind::Task(TaskEvent::State { to, .. }) => Some(*to),
        _ => None,
    })
}

pub fn committed(kind: &RunEventKind) -> bool {
    matches!(kind, RunEventKind::Task(TaskEvent::Committed(info)) if info.sha.is_some())
}

pub fn waits(events: &[RunEvent]) -> Vec<&WaitReason> {
    find(events, |kind| match kind {
        RunEventKind::Run(RunLifecycle::Waiting { reason, .. }) => Some(reason),
        _ => None,
    })
}

pub fn is_waiting(events: &[RunEvent]) -> bool {
    !waits(events).is_empty()
}

pub fn resumes(events: &[RunEvent]) -> Vec<&RunResumed> {
    find(events, |kind| match kind {
        RunEventKind::Run(RunLifecycle::Resumed(resumed)) => Some(resumed),
        _ => None,
    })
}

/// The task's branch, as its worktree event names it.
pub fn task_branch(events: &[RunEvent]) -> Option<String> {
    find(events, |kind| match kind {
        RunEventKind::Task(TaskEvent::Worktree { branch, .. }) => Some(branch.clone()),
        _ => None,
    })
    .into_iter()
    .next()
}
