//! The run event model. Each run's `events.jsonl` holds one [`RunEvent`] per line; it is the
//! source of truth, and only the process holding the run lock appends to it.

pub mod card;
pub mod checkpoint;
pub mod context;
pub mod failure;
pub mod goal;
pub mod plan;
pub mod run;
pub mod session;
pub mod shared;
pub mod vendor;
pub mod workflow;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::names::SessionKey;
use crate::{AgentEvent, CardId, Note, RunId};

pub use card::{CardEvent, GateOutcome, IntegrationEvent, ReviewEvent};
pub use checkpoint::CheckpointEvent;
pub use context::{ContextCategory, ContextDetail, ContextItem, ContextPart};
pub use goal::GoalEvent;
pub use plan::{DivideEvent, PlanEvent};
pub use run::{PhaseChange, RunLifecycle};
pub use session::SessionEvent;
pub use shared::{BlackboardEvent, RetroEvent};
pub use vendor::{BudgetEvent, HandoffEvent, VendorEvent};
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
    pub card_id: Option<CardId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionKey>,
    /// `conductor`, `user`, `policy`, or an adapter or endpoint id.
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
    /// The task graph, owners, reviewers, predictions, budget lines.
    Divide(DivideEvent),
    Card(CardEvent),
    Session(SessionEvent),
    /// The envelope's `session` says whose.
    Agent(AgentEvent),
    Gate(GateOutcome),
    Review(ReviewEvent),
    Integration(IntegrationEvent),
    Checkpoint(CheckpointEvent),
    /// A card's agent stopped and someone else continues, or it waits.
    Handoff(HandoffEvent),
    Vendor(VendorEvent),
    Budget(BudgetEvent),
    Blackboard(BlackboardEvent),
    /// Predictions scored, outcomes, memory proposals, recipe candidates.
    Retro(RetroEvent),
    Note(Note),
    Workflow(WorkflowEvent),
}
