//! What one run event changes for the app: the mode, the lifecycle stage, the spend, and
//! whether something now needs you.

use argonauts_core::app::AppMode;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::events::run::{ParkReason, RunOutcome};
use argonauts_core::events::{
    BudgetEvent, CheckpointEvent, RunEvent, RunLifecycle, SessionEvent, WorkflowEvent,
};
use argonauts_core::names::{CheckpointId, MessageId};
use argonauts_core::settings::ui::ChatTrigger;

use crate::controller::attach::{Attached, Stage};

/// A checkpoint that opened waiting for you.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Waiting {
    pub checkpoint: CheckpointId,
    pub kind: CheckpointKind,
}

#[derive(Debug, Default)]
pub(super) struct Seen {
    /// The mode changed to this.
    pub mode: Option<AppMode>,
    /// Spend, limit, or the approval mode changed, so the status line is stale.
    pub status: bool,
    pub waiting: Option<Waiting>,
    pub finished: Option<RunOutcome>,
    /// What the run's end recorded, which says why a failed run failed.
    pub detail: Option<String>,
    pub delivered: Option<MessageId>,
    /// The run is under way: an agent started, a step got through, or it waits for you.
    pub began: bool,
    /// It parked for its CLIs, out or down, with nothing running: why, in words.
    pub held: Option<String>,
}

/// Why a run that parked for its CLIs waits, or `None` when it waits for you.
fn held_words(reason: &ParkReason) -> Option<String> {
    match reason {
        ParkReason::Checkpoint | ParkReason::Paused => None,
        ParkReason::AllVendorsOut { .. } => {
            Some("every agent's CLI is out of plan usage or down until one comes back".to_owned())
        }
        ParkReason::WaitingForVendor { vendor, .. } => {
            Some(format!("its tasks wait for {vendor} to come back"))
        }
        ParkReason::NoStandIn { detail, .. } => Some(detail.clone()),
    }
}

pub(super) fn track(attached: &mut Attached, event: &RunEvent) -> Seen {
    let mut seen = Seen::default();
    let run = attached.run.clone();
    let mode = match &event.kind {
        RunEventKind::Run(lifecycle) => match lifecycle {
            RunLifecycle::Named(_) => None,
            RunLifecycle::Started(_) | RunLifecycle::Resumed(_) | RunLifecycle::PauseCancelled => {
                attached.stage = Stage::Going;
                attached.crashed = false;
                Some(AppMode::Working { run })
            }
            RunLifecycle::PauseRequested { .. } => {
                attached.stage = Stage::Going;
                Some(AppMode::Pausing { run })
            }
            RunLifecycle::Parked { reason, .. } => {
                attached.stage = Stage::Parked;
                // Parked for its CLIs, nothing runs; parked for you, it got under way.
                match held_words(reason) {
                    Some(words) => seen.held = Some(words),
                    None => seen.began = true,
                }
                Some(AppMode::Parked {
                    run,
                    reason: reason.clone(),
                })
            }
            RunLifecycle::Finished { outcome, detail } => {
                attached.stage = Stage::Finished(*outcome);
                seen.finished = Some(*outcome);
                seen.detail = detail.clone();
                Some(AppMode::Idle)
            }
            RunLifecycle::ApprovalModeChanged { mode, .. } => {
                attached.approval_mode = Some(*mode);
                seen.status = true;
                None
            }
            RunLifecycle::Paused { .. }
            | RunLifecycle::StopRequested { .. }
            | RunLifecycle::Snapshot(_)
            | RunLifecycle::ClosingAnswer(_) => None,
        },
        RunEventKind::Budget(BudgetEvent::Run(budget)) => {
            seen.status = attached.spent != Some(budget.spent) || attached.limit != budget.limit;
            attached.spent = Some(budget.spent);
            attached.limit = budget.limit;
            None
        }
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.needs_you => {
            seen.waiting = Some(Waiting {
                checkpoint: opened.id.clone(),
                kind: opened.kind,
            });
            seen.began = true;
            None
        }
        RunEventKind::Session(SessionEvent::Delivered { message, .. }) => {
            seen.delivered = Some(message.clone());
            None
        }
        RunEventKind::Session(SessionEvent::Started(_))
        | RunEventKind::Workflow(WorkflowEvent::StepFinished(_)) => {
            seen.began = true;
            None
        }
        _ => None,
    };
    if let Some(mode) = mode
        && mode != attached.mode
    {
        attached.mode = mode.clone();
        seen.mode = Some(mode);
    }
    seen
}

/// A vendor-down card asks who continues; every other card needs your answer.
pub(super) fn trigger(kind: CheckpointKind) -> ChatTrigger {
    match kind {
        CheckpointKind::VendorDown => ChatTrigger::VendorDown,
        _ => ChatTrigger::NeedsYou,
    }
}

pub(super) fn checkpoint_label(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Goal => "goal approval",
        CheckpointKind::Plan => "plan approval",
        CheckpointKind::Merge => "merge approval",
        CheckpointKind::Handoff => "fallback question",
        CheckpointKind::VendorDown => "agent down",
        CheckpointKind::Escalation => "task out of attempts",
        CheckpointKind::Budget => "spend limit",
        CheckpointKind::ChangeRequest => "change request",
        CheckpointKind::GoalChange => "goal change",
        CheckpointKind::WorkflowPromotion => "workflow promotion",
        CheckpointKind::TakeOverReturn => "return from a take-over",
        CheckpointKind::Question => "question from an agent",
        CheckpointKind::OriginalReturned => "original agent back",
    }
}

pub(super) fn outcome_label(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Merged => "merged",
        RunOutcome::PullRequest => "pull request",
        RunOutcome::BranchLeft => "branches left",
        RunOutcome::Abandoned => "abandoned",
        RunOutcome::Stopped => "stopped",
        RunOutcome::Failed => "failed",
    }
}

pub(super) fn outcome_sentence(run: impl std::fmt::Display, outcome: RunOutcome) -> String {
    match outcome {
        RunOutcome::Merged => format!("{run} is merged."),
        RunOutcome::PullRequest => format!("{run} finished with a pull request."),
        RunOutcome::BranchLeft => format!("{run} finished with its branches left for later."),
        RunOutcome::Abandoned => format!("{run} was abandoned, and its branches are kept."),
        RunOutcome::Stopped => format!("{run} stopped, and its work is committed."),
        RunOutcome::Failed => format!("{run} failed."),
    }
}
