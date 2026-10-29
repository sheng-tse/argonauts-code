//! The run event model. Each run's `events.jsonl` holds one [`RunEvent`] per line; it is the
//! source of truth, and only the process holding the run lock appends to it.

pub mod agent_status;
pub mod approval;
pub mod context;
pub mod council;
pub mod failure;
pub mod goal;
pub mod plan;
pub mod run;
pub mod session;
pub mod shared;
pub mod task;
pub mod workflow;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::names::SessionKey;
use crate::{AgentEvent, Note, RunId, TaskId};

pub use agent_status::{AgentStatusEvent, FallbackEvent, SpendEvent};
pub use approval::ApprovalEvent;
pub use context::{ContextCategory, ContextDetail, ContextKind, ContextUsage};
pub use goal::GoalEvent;
pub use plan::{DivideEvent, PlanEvent};
pub use run::{PhaseChange, RunLifecycle};
pub use session::SessionEvent;
pub use shared::{BlackboardEvent, RunReviewEvent};
pub use task::{CheckOutcome, MergeEvent, ReviewEvent, TaskEvent};
pub use workflow::WorkflowEvent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    /// Position in the log from 0, assigned by the writer. Subscribers resume from it.
    pub seq: u64,
    /// In UTC.
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionKey>,
    /// `argonauts-code`, `user`, `policy`, or an adapter or endpoint id.
    pub source: String,
    pub kind: RunEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RunEventKind {
    Run(RunLifecycle),
    Phase(PhaseChange),
    Goal(GoalEvent),
    Plan(PlanEvent),
    /// The task graph, workers, reviewers, predictions, limit lines.
    Divide(DivideEvent),
    Task(TaskEvent),
    Session(SessionEvent),
    /// The envelope's `session` says whose.
    Agent(AgentEvent),
    Check(CheckOutcome),
    Review(ReviewEvent),
    Merge(MergeEvent),
    Approval(ApprovalEvent),
    /// A task's agent stopped and someone else continues, or it waits.
    Fallback(FallbackEvent),
    AgentStatus(AgentStatusEvent),
    Spend(SpendEvent),
    Blackboard(BlackboardEvent),
    /// Predictions scored, outcomes, memory proposals, learned workflow candidates.
    RunReview(RunReviewEvent),
    Note(Note),
    Workflow(WorkflowEvent),
    /// A council's seats, messages, stances and checks.
    Council(council::CouncilEvent),
}
