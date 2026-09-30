//! Workflows and their steps, for the panel row, the workflow view, and track records per step
//! kind and standard phase.

use serde::{Deserialize, Serialize};

use crate::names::{CheckpointId, WorkflowId};
use crate::workflow::{StandardPhase, StepKind, StepRef, Workflow, WorkflowRef};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkflowEvent {
    /// The run's own workflow, or a nested one that a `workflow` step started.
    Started(Box<WorkflowStarted>),
    StepStarted(StepStarted),
    StepFinished(StepFinished),
    /// An approved change request replaced the workflow from its next phase on.
    Changed(Box<WorkflowChanged>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStarted {
    pub id: WorkflowId,
    /// The template it came from, or `None` when the lead composed it.
    pub from: Option<WorkflowRef>,
    /// SHA-256 of the definition's canonical JSON, so every record names exactly what ran.
    pub version: String,
    /// The `workflow` step that started it, for a nested one.
    pub parent: Option<StepRef>,
    /// As validated, with its parameters filled in.
    pub workflow: Workflow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepStarted {
    pub step: StepRef,
    pub kind: StepKind,
    pub standard: StandardPhase,
    /// 1 the first time. Loops and `for_each` count up.
    pub round: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepFinished {
    pub step: StepRef,
    pub kind: StepKind,
    pub standard: StandardPhase,
    pub round: u32,
    pub end: StepEnd,
    /// One line, such as "2 reviewers agreed".
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepEnd {
    Done,
    Failed,
    /// Its `when` condition didn't hold.
    Skipped,
    /// The run was stopped, or the step was replaced by a workflow change.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowChanged {
    pub id: WorkflowId,
    pub version: String,
    pub workflow: Workflow,
    /// The `change_request` checkpoint that approved it.
    pub checkpoint: CheckpointId,
    pub reason: String,
}
