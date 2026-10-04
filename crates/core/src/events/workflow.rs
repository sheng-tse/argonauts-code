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
    /// What the workflow says it is: its name, description, and phases. Only declared phases
    /// exist in the views, so a template's phase that never runs is never drawn. Recorded
    /// when a workflow starts, and again after a change.
    Declared(Box<WorkflowDeclared>),
    /// A declared phase's first step started.
    PhaseStarted(PhaseMark),
    /// A declared phase's last step ended.
    PhaseEnded {
        #[serde(flatten)]
        phase: PhaseMark,
        end: StepEnd,
    },
    /// The lead added a phase that hadn't started, at a phase end or between attempts.
    PhaseAdded {
        id: WorkflowId,
        phase: DeclaredPhase,
        why: String,
    },
    /// A declared phase that won't run. It stays in the view, dim, with its reason.
    PhaseSkipped {
        #[serde(flatten)]
        phase: PhaseMark,
        why: String,
    },
    /// One line on why the plan moved, by the lead or by code, such as "review:median found
    /// median([]) returns None, so a second round".
    Note(RunNote),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDeclared {
    pub id: WorkflowId,
    pub name: String,
    pub description: String,
    pub phases: Vec<DeclaredPhase>,
}

/// A phase as the workflow declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclaredPhase {
    /// From 0, its place in the workflow.
    pub index: u32,
    pub title: String,
    /// One line on who does what.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub standard: StandardPhase,
}

/// A declared phase, by place and title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseMark {
    pub id: WorkflowId,
    /// From 0.
    pub index: u32,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunNote {
    pub text: String,
    /// The workflow it's about, when it's about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowId>,
}

impl WorkflowDeclared {
    /// The declaration of a workflow as it's defined.
    pub fn of(id: &WorkflowId, workflow: &Workflow) -> Self {
        Self {
            id: id.clone(),
            name: workflow.name.clone(),
            description: workflow.description.clone(),
            phases: workflow
                .phases
                .iter()
                .enumerate()
                .map(|(index, phase)| DeclaredPhase {
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    title: phase.name.clone(),
                    detail: phase.detail.clone(),
                    standard: phase.standard,
                })
                .collect(),
        }
    }
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
    /// The `change_request` checkpoint that approved it. `None` for a change that needs no
    /// approval: the lead's design moving before anything writes, or a goal's next attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointId>,
    pub reason: String,
}
