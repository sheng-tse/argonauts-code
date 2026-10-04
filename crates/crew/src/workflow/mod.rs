//! The workflow runner. A workflow is phases of typed steps; the runner walks them, records
//! `Workflow` events with each step's standard phase, and hands each step to its kind's
//! executor. Gates before review, review by another family, hard limits, approvals, budgets,
//! and handoffs stay in code, so no workflow can turn them off.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::WorkflowId;
use argonauts_core::workflow::{StandardPhase, Step, StepRef, WorkflowRef};

pub mod authors;
pub mod brief;
pub mod builtin;
pub mod change;
pub mod conditions;
pub mod kinds;
pub mod naming;
pub mod params;
pub mod replay;
pub mod runner;
pub mod shape;
pub mod validate;
pub mod yaml;

/// Where a call sits: the step's place in its workflow, and which round of it this is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepAt {
    pub step: StepRef,
    /// 1 the first time. Loops and `for_each` count up.
    pub round: u32,
}

/// One step as the runner hands it to its kind's executor.
#[derive(Debug, Clone, PartialEq)]
pub struct StepCall {
    pub at: StepAt,
    /// 0 for the run's own workflow, 1 and 2 for nested ones.
    pub depth: u32,
    pub standard: StandardPhase,
    /// As validated, with its parameters filled in.
    pub step: Step,
    /// The current item inside a `for_each`.
    pub item: Option<Value>,
}

/// Where a step sits, for messages: "phase 2, step 1.3".
pub fn describe(step: &StepRef) -> String {
    let path: Vec<String> = step
        .path
        .iter()
        .map(|index| (index + 1).to_string())
        .collect();
    format!("phase {}, step {}", step.phase + 1, path.join("."))
}

impl StepCall {
    pub fn workflow(&self) -> &WorkflowId {
        &self.at.step.workflow
    }
}

/// How a step ended, as `StepFinished` records it, and what the run does next.
#[derive(Debug, Clone, PartialEq)]
pub struct StepDone {
    pub end: StepEnd,
    /// One line, such as "2 reviewers agreed".
    pub summary: Option<String>,
    pub flow: Flow,
}

impl StepDone {
    pub fn done(summary: impl Into<String>) -> Self {
        Self {
            end: StepEnd::Done,
            summary: Some(summary.into()),
            flow: Flow::Continue,
        }
    }

    pub fn skipped(summary: impl Into<String>) -> Self {
        Self {
            end: StepEnd::Skipped,
            summary: Some(summary.into()),
            flow: Flow::Continue,
        }
    }
}

/// What the run does after a step. Only code decides this, from schema-checked fields.
#[derive(Debug, Clone, PartialEq)]
pub enum Flow {
    Continue,
    /// The run ends, such as after landing, or after you abandon it.
    Finish(RunOutcome),
    /// The rest of the run follows another workflow, such as "run it solo" at the plan
    /// checkpoint, or `solo.escalate_to_crew` when the solo card escalates.
    Switch(WorkflowRef),
}
