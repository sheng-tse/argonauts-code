//! The blackboard and the run review.

use serde::{Deserialize, Serialize};

use super::run::Party;
use crate::TaskId;
use crate::names::{MessageId, ModelRef, RoleName, SessionKey};
use crate::steps::task::{MemoryProposal, ProcessChange};

/// Typed messages between agents, and from you. No free conversation.
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
    /// Paths, tasks, or decisions it refers to.
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlackboardKind {
    /// A question for the worker of another piece.
    Question,
    Answer,
    ChangeRequest,
    Decision,
    ExperimentReport,
    Fallback,
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
    /// The worker of a task.
    Task {
        task: TaskId,
    },
    Role {
        role: RoleName,
    },
    /// Every session of one agent and model.
    Worker {
        worker: ModelRef,
    },
    Session {
        session: SessionKey,
    },
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunReviewEvent {
    /// A prediction scored against the outcome.
    Scored {
        task: TaskId,
        /// Such as `p_pass_checks_first_try`.
        metric: String,
        /// The predicted probability.
        predicted: f64,
        outcome: bool,
        /// (predicted - outcome)^2.
        brier: f64,
    },
    /// An outcome record appended to the outcome log.
    Outcome {
        task: TaskId,
        worker: ModelRef,
        task_type: String,
        /// Passed checks first try.
        first_try: bool,
        tries: u32,
        merged: bool,
    },
    /// A memory proposal and its safety scan.
    Memory {
        from: ModelRef,
        proposal: MemoryProposal,
        /// Whether it passed the scan.
        accepted: bool,
        reason: Option<String>,
    },
    /// Notes and decisions committed to `.argonauts/`.
    Committed { sha: String, paths: Vec<String> },
    /// A learned workflow candidate for the evolution loop.
    Candidate(ProcessChange),
}
