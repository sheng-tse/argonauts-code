//! The blackboard: typed messages and which session each was delivered to.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::run::Party;
use argonauts_core::events::shared::{
    BlackboardEvent, BlackboardKind, BlackboardMessage, Recipient,
};
use argonauts_core::names::{MessageId, ModelRef, RoleName, SessionKey};
use argonauts_core::steps::StepName;

/// In a take-back's `refs`. Until the event model has its own event for it, taking a message
/// back is recorded as your empty reply to it carrying this mark, addressed to you.
pub const TAKEN_BACK: &str = "taken-back";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub messages: Vec<BlackboardMessage>,
    pub delivered: Vec<(MessageId, SessionKey)>,
    /// The task of each session a message was delivered to, so a task's next session doesn't
    /// get what its earlier one did.
    pub delivered_to_tasks: Vec<(MessageId, TaskId)>,
    /// The task each message was posted from, where it came from one.
    pub posted_from: BTreeMap<MessageId, TaskId>,
    pub taken_back: Vec<MessageId>,
}

/// A session as the blackboard addresses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressee {
    pub session: SessionKey,
    pub worker: ModelRef,
    pub role: RoleName,
    /// The step its start named, which tells the judge deciding an approval from the crew's.
    pub step: Option<StepName>,
    pub task: Option<TaskId>,
}

impl Addressee {
    pub fn is(&self, to: &Recipient) -> bool {
        match to {
            // The narrator and the judge deciding an approval aren't working agents.
            Recipient::All => {
                self.role != RoleName::Narrator && self.step != Some(StepName::ModelJudge)
            }
            Recipient::Task { task } => self.task.as_ref() == Some(task) && self.owns_task(),
            Recipient::Role { role } => self.role == *role,
            Recipient::Worker { worker } => same_worker(worker, &self.worker),
            Recipient::Session { session } => self.session == *session,
            Recipient::User => false,
        }
    }

    /// The session doing the task's work, not a reviewer of it.
    fn owns_task(&self) -> bool {
        matches!(
            self.role,
            RoleName::Worker | RoleName::Main | RoleName::Experiment
        )
    }
}

/// `@codex` names every model of the worker; `@codex/<model>` only that one.
pub fn same_worker(named: &ModelRef, worker: &ModelRef) -> bool {
    named.agent == worker.agent && (named.model.is_none() || named.model == worker.model)
}

impl Board {
    pub fn apply(&mut self, event: &BlackboardEvent, envelope: &RunEvent) {
        match event {
            BlackboardEvent::Posted(message) => {
                if self.get(&message.id).is_some() {
                    return;
                }
                if message.refs.iter().any(|mark| mark == TAKEN_BACK)
                    && message.from == Party::User
                    && let Some(original) = &message.in_reply_to
                {
                    self.taken_back.push(original.clone());
                }
                if let Some(task) = &envelope.task_id {
                    self.posted_from.insert(message.id.clone(), task.clone());
                }
                self.messages.push(message.clone());
            }
            BlackboardEvent::Delivered { message, to } => {
                if self.was_delivered(message, to) {
                    return;
                }
                self.delivered.push((message.clone(), to.clone()));
                if let Some(task) = &envelope.task_id {
                    self.delivered_to_tasks
                        .push((message.clone(), task.clone()));
                }
            }
        }
    }

    pub fn get(&self, id: &MessageId) -> Option<&BlackboardMessage> {
        self.messages.iter().find(|message| &message.id == id)
    }

    pub fn was_delivered(&self, message: &MessageId, session: &SessionKey) -> bool {
        self.delivered
            .iter()
            .any(|(id, to)| id == message && to == session)
    }

    pub fn delivered_anywhere(&self, message: &MessageId) -> bool {
        self.delivered.iter().any(|(id, _)| id == message)
    }

    /// Messages replying to this one, oldest first.
    pub fn replies<'a>(
        &'a self,
        message: &MessageId,
    ) -> impl Iterator<Item = &'a BlackboardMessage> + use<'a> {
        let message = message.clone();
        self.messages
            .iter()
            .filter(move |reply| reply.in_reply_to.as_ref() == Some(&message))
    }

    /// The record that settled a question or change request: the first answer to a question, or
    /// the decision on a request, whoever decided it for the run: you, your rules, the judge,
    /// autopilot or Argonauts Code. Workers' votes don't settle anything.
    pub fn settled(&self, message: &MessageId) -> Option<&BlackboardMessage> {
        let asked = self.get(message)?;
        self.replies(message).find(|reply| match asked.kind {
            BlackboardKind::Question => reply.kind == BlackboardKind::Answer,
            BlackboardKind::ChangeRequest => {
                reply.kind == BlackboardKind::Decision && !matches!(reply.from, Party::Agent { .. })
            }
            _ => false,
        })
    }

    /// Messages for this session that it hasn't been given yet, oldest first.
    pub fn pending_for(&self, who: &Addressee) -> Vec<&BlackboardMessage> {
        self.messages
            .iter()
            .filter(|message| {
                who.is(&message.to)
                    && !self.taken_back.contains(&message.id)
                    && !self.was_delivered(&message.id, &who.session)
                    && !self.task_had(message, who)
                    && !from_itself(message, who)
                    && self.settled(&message.id).is_none()
            })
            .collect()
    }

    /// A message for a task reaches the task once, whichever of its sessions takes it.
    fn task_had(&self, message: &BlackboardMessage, who: &Addressee) -> bool {
        let (Recipient::Task { .. }, Some(task)) = (&message.to, &who.task) else {
            return false;
        };
        self.delivered_to_tasks
            .iter()
            .any(|(id, to)| id == &message.id && to == task)
    }
}

fn from_itself(message: &BlackboardMessage, who: &Addressee) -> bool {
    matches!(&message.from, Party::Agent { session, .. } if *session == who.session)
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use argonauts_core::RunId;
    use argonauts_core::events::RunEventKind;

    use super::*;

    fn envelope(task: Option<&str>) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: task.map(|task| TaskId::new(task).unwrap()),
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Note(argonauts_core::Note {
                level: argonauts_core::NoteLevel::Info,
                text: String::new(),
            }),
        }
    }

    fn message(n: u32, kind: BlackboardKind, from: Party, to: Recipient) -> BlackboardMessage {
        BlackboardMessage {
            id: MessageId::numbered(n),
            kind,
            from,
            to,
            text: format!("message {n}"),
            in_reply_to: None,
            refs: Vec::new(),
        }
    }

    fn worker(session: u32, task: &str) -> Addressee {
        Addressee {
            session: SessionKey::numbered(session),
            worker: ModelRef::new("codex", Some("gpt-5.6".to_owned())),
            role: RoleName::Worker,
            step: Some(StepName::Build),
            task: Some(TaskId::new(task).unwrap()),
        }
    }

    #[test]
    fn pending_skips_delivered_taken_back_settled_and_own_messages() {
        let mut board = Board::default();
        let task = Recipient::Task {
            task: TaskId::new("task-002").unwrap(),
        };
        let post = |board: &mut Board, message: BlackboardMessage| {
            board.apply(&BlackboardEvent::Posted(message), &envelope(None));
        };
        post(
            &mut board,
            message(1, BlackboardKind::Guidance, Party::User, Recipient::All),
        );
        post(
            &mut board,
            message(
                2,
                BlackboardKind::Question,
                Party::ArgonautsCode,
                task.clone(),
            ),
        );
        post(
            &mut board,
            message(3, BlackboardKind::Mention, Party::User, Recipient::All),
        );
        let mut own = message(
            4,
            BlackboardKind::Question,
            Party::ArgonautsCode,
            Recipient::All,
        );
        own.from = Party::Agent {
            session: SessionKey::numbered(7),
            worker: ModelRef::new("codex", None),
        };
        post(&mut board, own);
        let mut back = message(5, BlackboardKind::Mention, Party::User, Recipient::User);
        back.in_reply_to = Some(MessageId::numbered(3));
        back.refs = vec![TAKEN_BACK.to_owned()];
        post(&mut board, back);

        let first = worker(7, "task-002");
        let ids = |pending: Vec<&BlackboardMessage>| -> Vec<String> {
            pending
                .iter()
                .map(|message| message.id.to_string())
                .collect()
        };
        assert_eq!(ids(board.pending_for(&first)), ["m-001", "m-002"]);

        board.apply(
            &BlackboardEvent::Delivered {
                message: MessageId::numbered(2),
                to: SessionKey::numbered(7),
            },
            &envelope(Some("task-002")),
        );
        assert_eq!(ids(board.pending_for(&first)), ["m-001"]);
        // The task's next session doesn't get the task's message again, but does get guidance.
        let second = worker(8, "task-002");
        assert_eq!(ids(board.pending_for(&second)), ["m-001", "m-004"]);

        let mut answer = message(
            6,
            BlackboardKind::Answer,
            Party::ArgonautsCode,
            Recipient::User,
        );
        answer.in_reply_to = Some(MessageId::numbered(4));
        post(&mut board, answer);
        assert_eq!(ids(board.pending_for(&second)), ["m-001"]);
        assert!(board.settled(&MessageId::numbered(4)).is_some());
    }

    #[test]
    fn recipients_match_sessions() {
        let who = worker(1, "task-002");
        assert!(who.is(&Recipient::All));
        assert!(who.is(&Recipient::Worker {
            worker: ModelRef::new("codex", None)
        }));
        assert!(!who.is(&Recipient::Worker {
            worker: ModelRef::new("codex", Some("other".to_owned()))
        }));
        assert!(who.is(&Recipient::Role {
            role: RoleName::Worker
        }));
        let reviewer = Addressee {
            role: RoleName::Reviewer,
            ..who.clone()
        };
        let task = Recipient::Task {
            task: TaskId::new("task-002").unwrap(),
        };
        assert!(who.is(&task) && !reviewer.is(&task));
        let narrator = Addressee {
            role: RoleName::Narrator,
            ..who
        };
        assert!(!narrator.is(&Recipient::All));
    }
}
