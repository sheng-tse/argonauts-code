//! The step in which a model you named writes another agent's instructions
//! (`[prompts] writer`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};

pub struct WriteInstructions;

impl Step for WriteInstructions {
    const NAME: StepName = StepName::WriteInstructions;
    type Input = WriteInstructionsInput;
    type Output = WrittenInstructions;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WriteInstructionsInput {
    /// The crew role the agent fills: main, orchestrator, workers, reviewers or judge.
    pub role: String,
    /// The frame the agent reads before your instructions, so you needn't repeat it.
    pub frame: String,
    /// What the agent would be told without you: the orchestrator's words, or Argonauts' own
    /// instructions for the step.
    pub draft: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WrittenInstructions {
    /// The instructions, as the agent will read them.
    pub instructions: String,
}
