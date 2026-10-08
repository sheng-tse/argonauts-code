//! The steps a fallback may need. The summary (`fallback.context = "note+summary"`) reads the
//! old session's transcript and writes what the fallback needs to know. The pick
//! (`fallback.pick = "judge"`) chooses among fallbacks that did the same task at once. Both
//! are read-only, with no tools.

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

/// The judge's pick among fallbacks that did the same task at once (`pick = "judge"`).
/// Read-only, no tools: each attempt's diff follows as a part of its own.
pub struct PickAttempt;

impl Step for PickAttempt {
    const NAME: StepName = StepName::PickAttempt;
    type Input = PickAttemptInput;
    type Output = PickAttemptOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PickAttemptInput {
    pub card: String,
    pub title: String,
    pub task: String,
    /// How many attempts passed their checks. They are numbered from 1, and never named.
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PickAttemptOutput {
    /// The number of the attempt that does the task best, from 1.
    pub pick: u32,
    /// Why that one, in a sentence or two.
    pub why: String,
}
