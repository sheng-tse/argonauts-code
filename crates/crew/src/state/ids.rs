//! Id counters, raised past every id the log holds, so a recovered run never reuses a session
//! key (and overwrites its raw logs), an approval, a message, a draft, or a decision number.

use serde::{Deserialize, Serialize};

use argonauts_core::events::approval::ApprovalEvent;
use argonauts_core::events::council::CouncilEventKind;
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::shared::BlackboardEvent;
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdKind {
    Session,
    Approval,
    Message,
    Draft,
    Decision,
    Workflow,
}

/// The highest number seen for each kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub sessions: u32,
    pub approvals: u32,
    pub messages: u32,
    pub drafts: u32,
    pub decisions: u32,
    pub workflows: u32,
}

impl Counters {
    /// Raises each counter past every id the event carries, in its envelope or its payload.
    pub fn observe(&mut self, event: &RunEvent) {
        if let Some(session) = &event.session {
            self.see(IdKind::Session, session.as_str());
        }
        match &event.kind {
            RunEventKind::Approval(approval) => {
                let id = match approval {
                    ApprovalEvent::Opened(opened) => &opened.id,
                    ApprovalEvent::Checked(check) => &check.id,
                    ApprovalEvent::Decided(decided) => &decided.id,
                    ApprovalEvent::Withdrawn { id, .. } | ApprovalEvent::SpotCheckQueued { id } => {
                        id
                    }
                };
                self.see(IdKind::Approval, id.as_str());
            }
            RunEventKind::Blackboard(BlackboardEvent::Posted(message)) => {
                self.see(IdKind::Message, message.id.as_str());
            }
            RunEventKind::Blackboard(BlackboardEvent::Delivered { message, to }) => {
                self.see(IdKind::Message, message.as_str());
                self.see(IdKind::Session, to.as_str());
            }
            RunEventKind::Session(SessionEvent::Queued(message)) => {
                self.see(IdKind::Message, message.id.as_str());
            }
            RunEventKind::Session(SessionEvent::Delivered { message, .. }) => {
                self.see(IdKind::Message, message.as_str());
            }
            RunEventKind::Goal(
                GoalEvent::ChangeRequested { id, .. } | GoalEvent::ChangeResolved { id, .. },
            ) => self.see(IdKind::Message, id.as_str()),
            RunEventKind::Council(council) => {
                if let CouncilEventKind::Message(message) = &council.kind {
                    self.see(IdKind::Message, message.id.as_str());
                }
            }
            RunEventKind::Plan(PlanEvent::Sealed { draft, .. }) => {
                self.see(IdKind::Draft, draft.as_str());
            }
            RunEventKind::Plan(PlanEvent::Decided(decision)) => {
                self.see(IdKind::Decision, decision.id.as_str());
            }
            RunEventKind::Workflow(WorkflowEvent::Started(started)) => {
                self.see(IdKind::Workflow, started.id.as_str());
            }
            RunEventKind::Workflow(WorkflowEvent::Changed(changed)) => {
                self.see(IdKind::Workflow, changed.id.as_str());
                if let Some(approval) = &changed.approval {
                    self.see(IdKind::Approval, approval.as_str());
                }
            }
            RunEventKind::Run(RunLifecycle::Waiting { waiting_on, .. }) => {
                for id in waiting_on {
                    self.see(IdKind::Approval, id.as_str());
                }
            }
            RunEventKind::Run(RunLifecycle::Snapshot(snapshot)) => {
                for session in &snapshot.running {
                    self.see(IdKind::Session, session.as_str());
                }
                for opened in &snapshot.open_approvals {
                    self.see(IdKind::Approval, opened.id.as_str());
                }
            }
            RunEventKind::Run(RunLifecycle::Resumed(resumed)) => {
                for session in &resumed.sessions {
                    self.see(IdKind::Session, session.session.as_str());
                }
            }
            _ => {}
        }
    }

    pub fn last(&self, kind: IdKind) -> u32 {
        match kind {
            IdKind::Session => self.sessions,
            IdKind::Approval => self.approvals,
            IdKind::Message => self.messages,
            IdKind::Draft => self.drafts,
            IdKind::Decision => self.decisions,
            IdKind::Workflow => self.workflows,
        }
    }

    /// Raises `kind` to at least `n`.
    pub fn raise(&mut self, kind: IdKind, n: u32) {
        let counter = match kind {
            IdKind::Session => &mut self.sessions,
            IdKind::Approval => &mut self.approvals,
            IdKind::Message => &mut self.messages,
            IdKind::Draft => &mut self.drafts,
            IdKind::Decision => &mut self.decisions,
            IdKind::Workflow => &mut self.workflows,
        };
        *counter = (*counter).max(n);
    }

    fn see(&mut self, kind: IdKind, id: &str) {
        if let Some(n) = number(id) {
            self.raise(kind, n);
        }
    }
}

/// The number at the end of an id such as `s-012` or `D-3`. Ids without one, such as a
/// parsed `ask-a`, never collide with numbered ones.
pub fn number(id: &str) -> Option<u32> {
    let (_, digits) = id.rsplit_once('-')?;
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::RunId;
    use argonauts_core::events::approval::ApprovalEvent;
    use argonauts_core::events::shared::BlackboardEvent;
    use argonauts_core::names::{ApprovalId, MessageId, SessionKey};
    use time::OffsetDateTime;

    fn event(session: Option<SessionKey>, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session,
            source: "argonauts-code".to_owned(),
            kind,
        }
    }

    #[test]
    fn counters_rise_past_every_id_seen_and_never_fall() {
        let mut counters = Counters::default();
        counters.observe(&event(
            Some(SessionKey::numbered(7)),
            RunEventKind::Approval(ApprovalEvent::Withdrawn {
                id: ApprovalId::numbered(3),
                reason: "moved".to_owned(),
            }),
        ));
        counters.observe(&event(
            Some(SessionKey::numbered(2)),
            RunEventKind::Blackboard(BlackboardEvent::Delivered {
                message: MessageId::numbered(11),
                to: SessionKey::numbered(9),
            }),
        ));
        assert_eq!(counters.last(IdKind::Session), 9);
        assert_eq!(counters.last(IdKind::Approval), 3);
        assert_eq!(counters.last(IdKind::Message), 11);
        assert_eq!(counters.last(IdKind::Draft), 0);
    }

    #[test]
    fn number_reads_the_digits_after_the_last_dash() {
        assert_eq!(number("s-012"), Some(12));
        assert_eq!(number("D-3"), Some(3));
        assert_eq!(number("wf-1000"), Some(1000));
        assert_eq!(number("custom"), None);
        assert_eq!(number("ask-a"), None);
    }
}
