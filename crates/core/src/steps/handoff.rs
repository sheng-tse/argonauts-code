//! The handoff summary step (`handoff.context = "note+summary"`). Read-only, no tools: it reads
//! the old session's transcript and writes what the replacement needs to know.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};

pub struct HandoffSummary;

impl Step for HandoffSummary {
    const NAME: StepName = StepName::HandoffSummary;
    type Input = HandoffSummaryInput;
    type Output = HandoffSummaryOutput;
}

/// The transcript itself follows as a part of its own, so it reads as Markdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HandoffSummaryInput {
    pub card: String,
    pub title: String,
    pub why_stopped: String,
    /// Only the transcript's tail fit the summarizer's context.
    pub cut: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HandoffSummaryOutput {
    /// What was tried, what worked and what didn't, the decisions made, and what's left, in at
    /// most about 300 words.
    pub summary: String,
}
