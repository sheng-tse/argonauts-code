//! After a session ends any way but a clean finish: `limits.after_stop` decides, and the card
//! applies the `RecoveryAction`. Handoffs and races go through `handoff`.

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardEvent, CardState};
use argonauts_core::events::run::{ParkReason, Party};
use argonauts_core::events::session::{EndReason, SessionEnd};
use argonauts_core::events::vendor::{HandoffEvent, HandoffPlan};
use argonauts_core::layout::run_files;
use argonauts_core::limits::{CandidateQuery, RecoveryAction, StopContext};
use argonauts_core::names::{RoleName, WorkerRef};

use crate::card::budget;
use crate::card::machine::Trigger;
use crate::card::pipeline::transition;
use crate::card::support::{
    newest_owner_session, owner_of, progress_of, record_card, record_of, state_of,
};
use crate::engine::host::Host;
use crate::engine::stand_in;
use crate::error::EngineResult;
use crate::handoff;
use crate::roles;
use crate::state::cards::CardRecord;

pub async fn decide(host: &Host, card: &CardId, end: &SessionEnd) -> EngineResult<RecoveryAction> {
    handoff::broken::gate(host, card, &end.reason).await?;
    let record = record_of(host, card)?;
    let worker = owner_of(host, &record);
    let logged_in = stand_in::login_check(host, &worker, &end.reason).await;
    let context = StopContext {
        run: host.run_id().clone(),
        card: record.spec.clone(),
        state: record.state,
        role: RoleName::Worker,
        worker,
        end: end.clone(),
        attempts: attempt_lines(&record),
        reassigned_once: progress_of(host, card).ladder_retries >= 1,
        events: host.repo().run_dir.join(run_files::EVENTS),
        logged_in,
    };
    Ok(host.services().limits.after_stop(&context).await)
}

/// Applies the action: resume the same session, rotate (not a failure, no retry used), hand
/// off, ask, wait, apply `limits.when_reached`, escalate, or park.
///
/// Resuming and rotating leave the card running: its pipeline starts that session next.
/// Everything else moves a running card to `NeedsDecision` first, or to `Failed` when the
/// session crashed, got stuck, or looped.
pub async fn apply(host: &Host, card: &CardId, action: RecoveryAction) -> EngineResult<()> {
    let failure = host.read(|state| {
        newest_owner_session(state, card)
            .and_then(|session| session.end.as_ref())
            .is_some_and(|end| is_failure(&end.reason))
    });
    match action {
        RecoveryAction::Proceed => {
            if state_of(host, card)? == CardState::Running {
                if failure {
                    transition(host, card, Trigger::Broke, "the session broke")?;
                } else {
                    let reason = "its result stands; the checks decide";
                    transition(host, card, Trigger::ReportedDone, reason)?;
                }
            }
        }
        RecoveryAction::ResumeSame { .. } | RecoveryAction::Rotate { .. } => {
            if state_of(host, card)? == CardState::NeedsDecision {
                transition(host, card, Trigger::Decided, "it continues")?;
            }
        }
        RecoveryAction::Escalate { reason } => escalate(host, card, failure, &reason)?,
        RecoveryAction::BudgetHit(on_hit) => {
            stop_for_decision(host, card, "the task hit its limit")?;
            budget::record_hit(host, card)?;
            budget::on_hit(host, card, on_hit).await?;
        }
        RecoveryAction::Park(reason) => {
            stop_for_decision(host, card, "every agent is out")?;
            let record = record_of(host, card)?;
            let until = match &reason {
                ParkReason::AllVendorsOut { until }
                | ParkReason::WaitingForVendor { until, .. }
                | ParkReason::NoStandIn { until, .. } => *until,
                ParkReason::Checkpoint | ParkReason::Paused => None,
            };
            let vendor = match reason {
                ParkReason::WaitingForVendor { vendor, .. } => vendor,
                _ => owner_of(host, &record).vendor(),
            };
            let waiting = HandoffEvent::Waiting {
                card: card.clone(),
                vendor,
                until,
            };
            host.record(Some(card), None, RunEventKind::Handoff(waiting))?;
        }
        action @ (RecoveryAction::HandOff { .. }
        | RecoveryAction::Race { .. }
        | RecoveryAction::Ask(_)
        | RecoveryAction::Wait { .. }) => {
            stop_for_decision(host, card, "its agent stopped")?;
            if let Some(plan) = needed(host, card, &action)? {
                let event = HandoffEvent::Needed(plan);
                host.record(Some(card), None, RunEventKind::Handoff(event))?;
            }
            let before = owner_of(host, &record_of(host, card)?);
            handoff::apply(host, card, action).await?;
            settle(host, card, &before)?;
        }
    }
    Ok(())
}

/// The plan a handoff starts from: the one the handoff card asks with, or for one the rules
/// make on their own, who stopped and why, the work so far, and who else could take it.
fn needed(
    host: &Host,
    card: &CardId,
    action: &RecoveryAction,
) -> EngineResult<Option<HandoffPlan>> {
    let packet = match action {
        RecoveryAction::Ask(plan) => return Ok(Some((**plan).clone())),
        RecoveryAction::HandOff { packet, .. } | RecoveryAction::Race { packet, .. } => packet,
        _ => return Ok(None),
    };
    let record = record_of(host, card)?;
    let stopped = owner_of(host, &record);
    let rules = handoff::rules::applied_for(host, &record.spec, &stopped);
    let query = handoff::choose::query(host, &record, &stopped, false);
    Ok(Some(HandoffPlan {
        card: card.clone(),
        title: record.spec.title.clone(),
        reason: packet.why_stopped.clone(),
        resets_at: None,
        work: packet.work.stat,
        gates_ran: handoff::broken::verdict(host, card)?.is_some(),
        candidates: host.services().handoff.candidates(&query),
        mode: rules.mode,
        strategy: rules.strategy,
        race_count: rules.together,
        also_moving: Vec::new(),
        stopped,
    }))
}

/// Crashed, stuck, looping, or never started: the retry ladder, not a handoff decision.
pub(crate) fn is_failure(reason: &EndReason) -> bool {
    matches!(
        reason,
        EndReason::Stuck { .. } | EndReason::Crashed { .. } | EndReason::StartFailed { .. }
    )
}

/// After a handoff decision: a card still waiting on a decision goes back to `Ready` unless it
/// waits for its vendor.
pub(crate) fn settle(host: &Host, card: &CardId, owner_before: &WorkerRef) -> EngineResult<()> {
    let record = record_of(host, card)?;
    if record.state != CardState::NeedsDecision || progress_of(host, card).waiting_for.is_some() {
        return Ok(());
    }
    let reason = if owner_of(host, &record) == *owner_before {
        "decided".to_owned()
    } else {
        format!("handed off to {}", owner_of(host, &record))
    };
    transition(host, card, Trigger::Decided, &reason)?;
    Ok(())
}

/// A running card stops to wait for a decision.
fn stop_for_decision(host: &Host, card: &CardId, reason: &str) -> EngineResult<()> {
    if state_of(host, card)? == CardState::Running {
        transition(host, card, Trigger::NeedsDecision, reason)?;
    }
    Ok(())
}

fn escalate(host: &Host, card: &CardId, failure: bool, reason: &str) -> EngineResult<()> {
    match state_of(host, card)? {
        CardState::Running if failure => {
            transition(host, card, Trigger::Broke, reason)?;
            transition(host, card, Trigger::Escalate, reason)?;
        }
        CardState::Running => {
            transition(host, card, Trigger::NeedsDecision, reason)?;
            transition(host, card, Trigger::Escalate, reason)?;
        }
        CardState::NeedsDecision | CardState::Failed => {
            transition(host, card, Trigger::Escalate, reason)?;
        }
        _ => {}
    }
    Ok(())
}

/// The retry ladder's step after a failure: a fresh session on another model family when the
/// card hasn't had its retry on another agent yet, else escalation. Returns the new owner.
pub(crate) fn retry_on_other_provider(
    host: &Host,
    card: &CardId,
    why: &str,
) -> EngineResult<Option<WorkerRef>> {
    let record = record_of(host, card)?;
    if progress_of(host, card).ladder_retries >= 1 {
        transition(
            host,
            card,
            Trigger::Escalate,
            &format!("{why}, after its retry on another agent"),
        )?;
        return Ok(None);
    }
    let Some(to) = other_provider(host, &record) else {
        let reason = format!("{why}, and no agent of another provider can take it");
        transition(host, card, Trigger::Escalate, &reason)?;
        return Ok(None);
    };
    let reassigned = CardEvent::Reassigned {
        from: Some(owner_of(host, &record)),
        to: to.clone(),
        by: Party::Conductor,
        rule_override: None,
    };
    record_card(host, card, reassigned)?;
    transition(
        host,
        card,
        Trigger::Retry,
        &format!("{why}; a fresh session on {to}"),
    )?;
    Ok(Some(to))
}

/// The first valid candidate from another model family than the owner's and the reviewers'.
pub(crate) fn other_provider(host: &Host, record: &CardRecord) -> Option<WorkerRef> {
    let owner = owner_of(host, record);
    let mut exclude = vec![roles::family(host, &owner)];
    exclude.extend(record.spec.reviewers.iter().map(|r| roles::family(host, r)));
    exclude.sort();
    exclude.dedup();
    let query = CandidateQuery {
        card: record.spec.clone(),
        role: RoleName::Worker,
        stopped: owner.clone(),
        exclude_families: exclude.clone(),
        fallback: Vec::new(),
        all_valid: true,
    };
    host.services()
        .handoff
        .shortlist(&query)
        .candidates
        .into_iter()
        .map(|candidate| candidate.worker)
        .find(|worker| *worker != owner && !exclude.contains(&roles::family(host, worker)))
}

/// Another worker for a budget handoff, of any family but the reviewers'.
pub(crate) fn any_other(host: &Host, record: &CardRecord) -> Option<WorkerRef> {
    let owner = owner_of(host, record);
    let reviewers: Vec<String> = record
        .spec
        .reviewers
        .iter()
        .map(|r| roles::family(host, r))
        .collect();
    let query = CandidateQuery {
        card: record.spec.clone(),
        role: RoleName::Worker,
        stopped: owner.clone(),
        exclude_families: reviewers.clone(),
        fallback: Vec::new(),
        all_valid: false,
    };
    host.services()
        .handoff
        .shortlist(&query)
        .candidates
        .into_iter()
        .map(|candidate| candidate.worker)
        .find(|worker| *worker != owner && !reviewers.contains(&roles::family(host, worker)))
}

/// One line per attempt so far, oldest first, for `StopContext` and the escalation card.
pub(crate) fn attempt_lines(record: &CardRecord) -> Vec<String> {
    let mut lines: Vec<String> = record
        .reports
        .iter()
        .map(|report| format!("{:?}: {}", report.status, report.summary))
        .collect();
    if let Some(gate) = record.gates.iter().rev().find(|gate| !gate.passed) {
        lines.push(format!(
            "last failed check · {}: {}",
            gate.name, gate.summary
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::Watchdog;

    #[test]
    fn crashes_and_stuck_sessions_are_failures_and_vendor_stops_are_not() {
        assert!(is_failure(&EndReason::Stuck {
            watchdog: Watchdog::Inactivity
        }));
        assert!(is_failure(&EndReason::Crashed {
            message: "exit 1".into()
        }));
        assert!(is_failure(&EndReason::StartFailed {
            message: "no binary".into()
        }));
        assert!(!is_failure(&EndReason::UsageLimit { resets_at: None }));
        assert!(!is_failure(&EndReason::Stopped));
        assert!(!is_failure(&EndReason::ContextFull));
    }
}
