//! The blackboard and the retro.

use serde::{Deserialize, Serialize};

use super::run::Party;
use crate::CardId;
use crate::names::{MessageId, RoleName, SessionKey, WorkerRef};
use crate::steps::card::{MemoryProposal, ProcessChange};

/// Typed messages between agents, and from you. No free chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BlackboardEvent {
    Posted(BlackboardMessage),
    /// It reached a session at a delivery point.
    Delivered {
        message: MessageId,
        to: SessionKey,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlackboardMessage {
    pub id: MessageId,
    pub kind: BlackboardKind,
    pub from: Party,
    pub to: Recipient,
    pub text: String,
    pub in_reply_to: Option<MessageId>,
    /// Paths, cards, or decisions it refers to.
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlackboardKind {
    /// A question for the owner of another piece.
    Question,
    Answer,
    ChangeRequest,
    Decision,
    SpikeReport,
    Handoff,
    /// Your guidance, typed while the run works.
    Guidance,
    /// Your `@mention`.
    Mention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum Recipient {
    /// Every working agent.
    All,
    /// The owner of a card.
    Card {
        card: CardId,
    },
    Role {
        role: RoleName,
    },
    /// Every session of a worker.
    Worker {
        worker: WorkerRef,
    },
    Session {
        session: SessionKey,
    },
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RetroEvent {
    /// A prediction scored against the outcome.
    Scored {
        card: CardId,
        /// Such as `p_pass_gates_first_try`.
        metric: String,
        /// The predicted probability.
        predicted: f64,
        outcome: bool,
        /// (predicted - outcome)^2.
        brier: f64,
    },
    /// An outcome record appended to the outcome log.
    Outcome {
        card: CardId,
        worker: WorkerRef,
        task_type: String,
        /// Passed gates first try.
        first_try: bool,
        attempts: u32,
        integrated: bool,
    },
    /// A memory proposal and its safety scan.
    Memory {
        from: WorkerRef,
        proposal: MemoryProposal,
        /// Whether it passed the scan.
        accepted: bool,
        reason: Option<String>,
    },
    /// Notes and decisions committed to `.argonauts/`.
    Committed { sha: String, paths: Vec<String> },
    /// A recipe candidate for the evolution loop.
    Candidate(ProcessChange),
}
