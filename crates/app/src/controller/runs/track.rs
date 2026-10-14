//! What one run event changes for the app: the mode, the lifecycle stage, the spend, and
//! whether something now needs you.

use argonauts_core::app::AppMode;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::run::{RunOutcome, WaitReason};
use argonauts_core::events::{
    ApprovalEvent, RunEvent, RunLifecycle, SessionEvent, SpendEvent, WorkflowEvent,
};
use argonauts_core::names::{ApprovalId, MessageId};
use argonauts_core::settings::ui::ConversationTrigger;

use crate::controller::attach::{Attached, Stage};

/// An approval that opened waiting for you.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Waiting {
    pub approval: ApprovalId,
    pub kind: ApprovalKind,
}

#[derive(Debug, Default)]
pub(super) struct Seen {
    /// The mode changed to this.
    pub mode: Option<AppMode>,
    /// Spend, limit, or the approval mode changed, so the bottom line is stale.
    pub status: bool,
    pub waiting: Option<Waiting>,
    pub finished: Option<RunOutcome>,
    /// What the run's end recorded, which says why a failed run failed.
    pub detail: Option<String>,
    pub delivered: Option<MessageId>,
    /// The run is under way: an agent started, a step got through, or it waits for you.
    pub began: bool,
    /// It waits for its CLIs, out or down, with nothing running: why, in words.
    pub held: Option<String>,
}

/// Why a run waits for its CLIs, or `None` when it waits for you.
fn held_words(reason: &WaitReason) -> Option<String> {
    match reason {
        WaitReason::Approval | WaitReason::Paused => None,
        WaitReason::AllAgentsOut { .. } => {
            Some("every agent's CLI is out of plan usage or down until one comes back".to_owned())
        }
        WaitReason::WaitingForAgent { agent_id, .. } => {
            Some(format!("its tasks wait for {agent_id} to come back"))
        }
        WaitReason::NoFallback { detail, .. } => Some(detail.clone()),
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
            RunLifecycle::Waiting { reason, .. } => {
                attached.stage = Stage::Waiting;
                // Waiting for its CLIs, nothing runs; waiting for you, it got under way.
                match held_words(reason) {
                    Some(words) => seen.held = Some(words),
                    None => seen.began = true,
                }
                Some(AppMode::Waiting {
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
        RunEventKind::Spend(SpendEvent::Run(limit)) => {
            seen.status = attached.spent != Some(limit.spent) || attached.limit != limit.limit;
            attached.spent = Some(limit.spent);
            attached.limit = limit.limit;
            None
        }
        RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.needs_you => {
            seen.waiting = Some(Waiting {
                approval: opened.id.clone(),
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

/// An agent-down question asks who continues; every other question needs your answer.
pub(super) fn trigger(kind: ApprovalKind) -> ConversationTrigger {
    match kind {
        ApprovalKind::AgentDown => ConversationTrigger::AgentDown,
        _ => ConversationTrigger::NeedsYou,
    }
}

pub(super) fn approval_label(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::Understand => "Understand approval",
        ApprovalKind::Plan => "plan approval",
        ApprovalKind::Merge => "merge approval",
        ApprovalKind::Fallback => "fallback question",
        ApprovalKind::AgentDown => "agent down",
        ApprovalKind::Stuck => "task out of tries",
        ApprovalKind::Limit => "spend limit",
        ApprovalKind::ChangeRequest => "change request",
        ApprovalKind::GoalChange => "goal change",
        ApprovalKind::WorkflowPromotion => "workflow promotion",
        ApprovalKind::TakeOverReturn => "return from a take-over",
        ApprovalKind::Question => "question from an agent",
        ApprovalKind::OriginalReturned => "original agent back",
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
        RunOutcome::Abandoned => format!("{run} was abandoned, and its work is removed."),
        RunOutcome::Stopped => format!("{run} stopped, and its work is committed."),
        RunOutcome::Failed => format!("{run} failed."),
    }
}
