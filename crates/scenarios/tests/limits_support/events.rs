//! Finding events in a run's journal.

use argonauts_core::CardId;
use argonauts_core::events::card::{AttemptCause, CardState};
use argonauts_core::events::checkpoint::{CheckpointKind, CheckpointOpened};
use argonauts_core::events::run::{ParkReason, RunResumed};
use argonauts_core::events::session::{EndReason, SessionEnd, SessionStarted};
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::events::{
    CardEvent, CheckpointEvent, HandoffEvent, RunEvent, RunEventKind, RunLifecycle, SessionEvent,
};
use argonauts_core::names::WorkerRef;

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

/// The first card of the run.
pub fn first_card(events: &[RunEvent]) -> Option<CardId> {
    find(events, |kind| match kind {
        RunEventKind::Card(CardEvent::Created(spec)) => Some(spec.id.clone()),
        _ => None,
    })
    .into_iter()
    .next()
}

/// The latest checkpoint of `kind` that opened.
pub fn opened(events: &[RunEvent], kind: CheckpointKind) -> Option<&CheckpointOpened> {
    find(events, |event| match event {
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind => {
            Some(opened.as_ref())
        }
        _ => None,
    })
    .pop()
}

/// The `seq` of the first checkpoint of `kind` that opened.
pub fn opened_at(events: &[RunEvent], kind: CheckpointKind) -> Option<u64> {
    seq_of(
        events,
        |event| matches!(event, RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.kind == kind),
    )
}

pub fn has_opened(kind: CheckpointKind) -> impl Fn(&[RunEvent]) -> bool {
    move |events| opened(events, kind).is_some()
}

pub fn handoff_plans(events: &[RunEvent]) -> Vec<&HandoffPlan> {
    find(events, |kind| match kind {
        RunEventKind::Handoff(HandoffEvent::Needed(plan)) => Some(plan),
        _ => None,
    })
}

/// Who each handoff went to, in order.
pub fn handed_off(events: &[RunEvent]) -> Vec<&HandoffEvent> {
    find(events, |kind| match kind {
        RunEventKind::Handoff(event @ HandoffEvent::HandedOff { .. }) => Some(event),
        _ => None,
    })
}

pub fn handed_off_to(worker: &'static str) -> impl Fn(&[RunEvent]) -> bool {
    move |events| {
        handed_off(events)
            .iter()
            .any(|event| matches!(event, HandoffEvent::HandedOff { to, .. } if to.worker == worker))
    }
}

/// Every owner attempt with its worker and cause.
pub fn attempts(events: &[RunEvent]) -> Vec<(&WorkerRef, AttemptCause)> {
    find(events, |kind| match kind {
        RunEventKind::Card(CardEvent::Attempt { worker, cause, .. }) => Some((worker, *cause)),
        _ => None,
    })
}

pub fn has_attempt(worker: &'static str, cause: AttemptCause) -> impl Fn(&[RunEvent]) -> bool {
    move |events| {
        attempts(events)
            .iter()
            .any(|(who, why)| who.worker == worker && *why == cause)
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
            RunEventKind::Session(SessionEvent::Started(started)) => {
                started.worker.worker == worker
            }
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

pub fn card_states(events: &[RunEvent]) -> Vec<CardState> {
    find(events, |kind| match kind {
        RunEventKind::Card(CardEvent::State { to, .. }) => Some(*to),
        _ => None,
    })
}

pub fn committed(kind: &RunEventKind) -> bool {
    matches!(kind, RunEventKind::Card(CardEvent::Committed(info)) if info.sha.is_some())
}

pub fn parks(events: &[RunEvent]) -> Vec<&ParkReason> {
    find(events, |kind| match kind {
        RunEventKind::Run(RunLifecycle::Parked { reason, .. }) => Some(reason),
        _ => None,
    })
}

pub fn is_parked(events: &[RunEvent]) -> bool {
    !parks(events).is_empty()
}

pub fn resumes(events: &[RunEvent]) -> Vec<&RunResumed> {
    find(events, |kind| match kind {
        RunEventKind::Run(RunLifecycle::Resumed(resumed)) => Some(resumed),
        _ => None,
    })
}

/// The card's branch, as its worktree event names it.
pub fn card_branch(events: &[RunEvent]) -> Option<String> {
    find(events, |kind| match kind {
        RunEventKind::Card(CardEvent::Worktree { branch, .. }) => Some(branch.clone()),
        _ => None,
    })
    .into_iter()
    .next()
}
