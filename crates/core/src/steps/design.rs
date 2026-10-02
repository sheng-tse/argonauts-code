//! The lead's workflow design: pick the closest template and fill in its parameters, or
//! compose a workflow from the step kinds. Code validates the answer before anything runs, and
//! sends the errors back when it fails. At a phase end the same step adapts the running
//! workflow, given what it is and what its finished phases produced.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};

pub struct DesignWorkflow;

impl Step for DesignWorkflow {
    const NAME: StepName = StepName::DesignWorkflow;
    type Input = DesignInput;
    type Output = DesignOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DesignInput {
    pub request: String,
    /// The approved goal's summary and criteria, when there is one.
    pub goal: Vec<String>,
    /// Templates and past workflows that did well on similar tasks, best first. The engine
    /// sends them as a markdown part after the input instead, one `### name` section each, so
    /// their files read as YAML; it leaves this empty.
    pub examples: Vec<WorkflowExample>,
    /// Each step kind with one line on what it does.
    pub step_kinds: Vec<String>,
    pub allow_composed: bool,
    /// The validator's errors on the last answer. Empty the first time.
    pub errors: Vec<String>,
    /// When adapting mid-run, the running workflow as YAML with its finished phases marked.
    /// `None` when designing one for a new task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// One line per finished phase or step of `current`, such as "Review: 2 reviewers
    /// disagreed on the retry header".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowExample {
    pub name: String,
    pub description: String,
    /// The workflow file.
    pub yaml: String,
    /// Finished runs of similar tasks, and how many of them passed.
    pub runs: u32,
    pub passed: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DesignOutput {
    /// The template picked. `None` means `composed` holds a new workflow.
    pub template: Option<String>,
    pub params: Vec<ParamSetting>,
    /// A workflow file in YAML, when no template fits.
    pub composed: Option<String>,
    /// One line for the plan checkpoint.
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ParamSetting {
    pub name: String,
    /// As YAML scalar text, such as `diff` or `2`.
    pub value: String,
}
