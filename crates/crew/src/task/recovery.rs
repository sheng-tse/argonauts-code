//! After a session ends any way but a clean finish: `limits.after_stop` decides, and the task
//! applies the `RecoveryAction`. Fallbacks and races go through `fallback`.

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::agent_status::{FallbackEvent, FallbackPlan};
use argonauts_core::events::run::{Party, WaitReason};
use argonauts_core::events::session::{EndReason, SessionEnd};
use argonauts_core::events::task::{TaskEvent, TaskState};
use argonauts_core::layout::run_files;
use argonauts_core::limits::{CandidateQuery, RecoveryAction, StopContext};
use argonauts_core::names::{ModelRef, RoleName};

use crate::engine::host::Host;
use crate::engine::job_fallback;
use crate::error::EngineResult;
use crate::fallback;
use crate::roles;
use crate::state::tasks::TaskEntry;
use crate::task::limit;
use crate::task::machine::Trigger;
use crate::task::pipeline::transition;
use crate::task::support::{
    newest_worker_session, progress_of, record_of, record_task, state_of, worker_of,
};

pub async fn decide(host: &Host, task: &TaskId, end: &SessionEnd) -> EngineResult<RecoveryAction> {
    fallback::broken::check(host, task, &end.reason).await?;
    let record = record_of(host, task)?;
    let worker = worker_of(host, &record);
    let logged_in = job_fallback::login_check(host, &worker, &end.reason).await;
    let context = StopContext {
        run: host.run_id().clone(),
        task: record.spec.clone(),
        state: record.state,
        role: RoleName::Worker,
        worker,
        end: end.clone(),
        tries: try_lines(&record),
        reassigned_once: progress_of(host, task).ladder_retries >= 1,
        events: host.repo().run_dir.join(run_files::EVENTS),
        logged_in,
    };
    Ok(host.services().limits.after_stop(&context).await)
}

/// Applies the action: resume the same session, rotate (not a failure, no retry used), fall
/// back, ask, wait, apply `limits.when_reached`, leave it to you, or have the whole run wait.
///
/// Resuming and rotating leave the task running: its pipeline starts that session next.
/// Everything else moves a running task to `NeedsDecision` first, or to `Failed` when the
/// session crashed, got stuck, or looped.
pub async fn apply(host: &Host, task: &TaskId, action: RecoveryAction) -> EngineResult<()> {
    let failure = host.read(|state| {
        newest_worker_session(state, task)
            .and_then(|session| session.end.as_ref())
            .is_some_and(|end| is_failure(&end.reason))
    });
    match action {
        RecoveryAction::Proceed => {
            if state_of(host, task)? == TaskState::Running {
                if failure {
                    transition(host, task, Trigger::Broke, "the session broke")?;
                } else {
                    let reason = "its result stands; the checks decide";
                    transition(host, task, Trigger::ReportedDone, reason)?;
                }
            }
        }
        RecoveryAction::ResumeSame { .. } | RecoveryAction::Rotate { .. } => {
            if state_of(host, task)? == TaskState::NeedsDecision {
                transition(host, task, Trigger::Decided, "it continues")?;
            }
        }
        RecoveryAction::AskYou { reason } => ask_you(host, task, failure, &reason)?,
        RecoveryAction::LimitHit(on_hit) => {
            stop_for_decision(host, task, "the task hit its limit")?;
            limit::record_hit(host, task)?;
            limit::on_hit(host, task, on_hit).await?;
        }
        RecoveryAction::RunWaits(reason) => {
            stop_for_decision(host, task, "every agent is out")?;
            let record = record_of(host, task)?;
            let until = match &reason {
                WaitReason::AllAgentsOut { until }
                | WaitReason::WaitingForAgent { until, .. }
                | WaitReason::NoFallback { until, .. } => *until,
                WaitReason::Approval | WaitReason::Paused => None,
            };
            let agent_id = match reason {
                WaitReason::WaitingForAgent { agent_id, .. } => agent_id,
                _ => worker_of(host, &record).agent_id(),
            };
            let waiting = FallbackEvent::Waiting {
                task: task.clone(),
                agent_id,
                until,
                kept_off: None,
            };
            host.record(Some(task), None, RunEventKind::Fallback(waiting))?;
        }
        action @ (RecoveryAction::Fallback { .. }
        | RecoveryAction::Race { .. }
        | RecoveryAction::Ask(_)
        | RecoveryAction::Wait { .. }) => {
            stop_for_decision(host, task, "its agent stopped")?;
            if let Some(plan) = needed(host, task, &action)? {
                let event = FallbackEvent::Needed(plan);
                host.record(Some(task), None, RunEventKind::Fallback(event))?;
            }
            let before = worker_of(host, &record_of(host, task)?);
            fallback::apply(host, task, action).await?;
            settle(host, task, &before)?;
        }
    }
    Ok(())
}

/// The plan a fallback starts from: the one the fallback question asks with, or for one the rules
/// make on their own, who stopped and why, the work so far, and who else could take it.
fn needed(
    host: &Host,
    task: &TaskId,
    action: &RecoveryAction,
) -> EngineResult<Option<FallbackPlan>> {
    let note = match action {
        RecoveryAction::Ask(plan) => return Ok(Some((**plan).clone())),
        RecoveryAction::Fallback { note, .. } | RecoveryAction::Race { note, .. } => note,
        _ => return Ok(None),
    };
    let record = record_of(host, task)?;
    let stopped = worker_of(host, &record);
    let rules = fallback::rules::applied_for(host, &record.spec, &stopped);
    let query = fallback::choose::query(host, &record, &stopped, false);
    Ok(Some(FallbackPlan {
        task: task.clone(),
        title: record.spec.title.clone(),
        reason: note.why_stopped.clone(),
        resets_at: None,
        work: note.work.stat,
        checks_ran: fallback::broken::verdict(host, task)?.is_some(),
        candidates: host.services().fallback.candidates(&query),
        mode: rules.mode,
        strategy: rules.strategy,
        race_count: rules.together,
        also_moving: Vec::new(),
        stopped,
    }))
}

/// Crashed, stuck, looping, or never started: the retry ladder, not a fallback decision.
pub(crate) fn is_failure(reason: &EndReason) -> bool {
    matches!(
        reason,
        EndReason::Stuck { .. } | EndReason::Crashed { .. } | EndReason::StartFailed { .. }
    )
}

/// After a fallback decision: a task still waiting on a decision goes back to `Ready` unless it
/// waits for its agent.
pub(crate) fn settle(host: &Host, task: &TaskId, worker_before: &ModelRef) -> EngineResult<()> {
    let record = record_of(host, task)?;
    if record.state != TaskState::NeedsDecision || progress_of(host, task).waiting_for.is_some() {
        return Ok(());
    }
    let reason = if worker_of(host, &record) == *worker_before {
        "decided".to_owned()
    } else {
        format!("fell back to {}", worker_of(host, &record))
    };
    transition(host, task, Trigger::Decided, &reason)?;
    Ok(())
}

/// A running task stops to wait for a decision.
fn stop_for_decision(host: &Host, task: &TaskId, reason: &str) -> EngineResult<()> {
    if state_of(host, task)? == TaskState::Running {
        transition(host, task, Trigger::NeedsDecision, reason)?;
    }
    Ok(())
}

fn ask_you(host: &Host, task: &TaskId, failure: bool, reason: &str) -> EngineResult<()> {
    match state_of(host, task)? {
        TaskState::Running if failure => {
            transition(host, task, Trigger::Broke, reason)?;
            transition(host, task, Trigger::AskYou, reason)?;
        }
        TaskState::Running => {
            transition(host, task, Trigger::NeedsDecision, reason)?;
            transition(host, task, Trigger::AskYou, reason)?;
        }
        TaskState::NeedsDecision | TaskState::Failed => {
            transition(host, task, Trigger::AskYou, reason)?;
        }
        _ => {}
    }
    Ok(())
}

/// The retry ladder's step after a failure: a fresh session on another model provider when the
/// task hasn't had its retry on another agent yet, else it needs you. Returns the new worker.
pub(crate) fn retry_on_other_provider(
    host: &Host,
    task: &TaskId,
    why: &str,
) -> EngineResult<Option<ModelRef>> {
    let record = record_of(host, task)?;
    if progress_of(host, task).ladder_retries >= 1 {
        transition(
            host,
            task,
            Trigger::AskYou,
            &format!("{why}, after its retry on another agent"),
        )?;
        return Ok(None);
    }
    let Some(to) = other_provider(host, &record) else {
        let reason = format!("{why}, and no agent of another provider can take it");
        transition(host, task, Trigger::AskYou, &reason)?;
        return Ok(None);
    };
    let reassigned = TaskEvent::Reassigned {
        from: Some(worker_of(host, &record)),
        to: to.clone(),
        by: Party::ArgonautsCode,
        rule_override: None,
    };
    record_task(host, task, reassigned)?;
    transition(
        host,
        task,
        Trigger::Retry,
        &format!("{why}; a fresh session on {to}"),
    )?;
    Ok(Some(to))
}

/// The first valid candidate from another model provider than the worker's and the reviewers',
/// leaving out any the run knows is out, such as a CLI that crashed twice in a row.
pub(crate) fn other_provider(host: &Host, record: &TaskEntry) -> Option<ModelRef> {
    let worker = worker_of(host, record);
    let mut exclude = vec![roles::provider(host, &worker)];
    exclude.extend(
        record
            .spec
            .reviewers
            .iter()
            .map(|r| roles::provider(host, r)),
    );
    exclude.sort();
    exclude.dedup();
    let query = CandidateQuery {
        task: record.spec.clone(),
        role: RoleName::Worker,
        stopped: worker.clone(),
        exclude_providers: exclude.clone(),
        fallback: Vec::new(),
        all_valid: true,
    };
    host.services()
        .fallback
        .shortlist(&query)
        .candidates
        .into_iter()
        .map(|candidate| candidate.worker)
        .find(|model| {
            *model != worker
                && !exclude.contains(&roles::provider(host, model))
                && !job_fallback::known_out(host, model)
        })
}

/// Another worker for a limit fallback, of any provider but the reviewers', and none the run
/// knows is out.
pub(crate) fn any_other(host: &Host, record: &TaskEntry) -> Option<ModelRef> {
    let worker = worker_of(host, record);
    let reviewers: Vec<String> = record
        .spec
        .reviewers
        .iter()
        .map(|r| roles::provider(host, r))
        .collect();
    let query = CandidateQuery {
        task: record.spec.clone(),
        role: RoleName::Worker,
        stopped: worker.clone(),
        exclude_providers: reviewers.clone(),
        fallback: Vec::new(),
        all_valid: false,
    };
    host.services()
        .fallback
        .shortlist(&query)
        .candidates
        .into_iter()
        .map(|candidate| candidate.worker)
        .find(|model| {
            *model != worker
                && !reviewers.contains(&roles::provider(host, model))
                && !job_fallback::known_out(host, model)
        })
}

/// One line per try so far, oldest first, for `StopContext` and the stuck question.
pub(crate) fn try_lines(record: &TaskEntry) -> Vec<String> {
    let mut lines: Vec<String> = record
        .reports
        .iter()
        .map(|report| format!("{:?}: {}", report.status, report.summary))
        .collect();
    if let Some(check) = record.checks.iter().rev().find(|check| !check.passed) {
        lines.push(format!(
            "last failed check · {}: {}",
            check.name, check.summary
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::Watchdog;

    #[test]
    fn crashes_and_stuck_sessions_are_failures_and_agent_stops_are_not() {
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
