//! The narrator step. Read-only, no tools, sees digests only.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};

pub struct Narrate;

impl Step for Narrate {
    const NAME: StepName = StepName::Narrate;
    type Input = NarrateInput;
    type Output = NarrateOutput;
}

/// Digests built by code from the event log. No transcripts, file contents, or credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NarrateInput {
    pub mode: NarrateMode,
    /// The goal summary, if approved.
    pub goal: Option<String>,
    pub decisions: Vec<String>,
    /// Digest lines since the last update (or the whole run for a summary), oldest first.
    pub digest: Vec<String>,
    /// The narrator's own recent lines, so it doesn't repeat itself.
    pub previous: Vec<String>,
    /// Your question, for `answer`.
    pub question: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NarrateMode {
    /// One to three short lines about what changed.
    Update,
    Summary,
    Answer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NarrateOutput {
    /// Short plain sentences.
    pub lines: Vec<String>,
}
