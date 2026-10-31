//! Types more than one part of the learning crate reads. Each part owns its own module; these
//! are the shapes they hand each other, so they stay fixed while the parts are built.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::RunId;
use argonauts_core::runs::events::run::RunOutcome;

/// One version of one learned workflow, such as `api-feature` v3.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LearnedWorkflowRef {
    pub name: String,
    pub version: u32,
}

impl std::fmt::Display for LearnedWorkflowRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} v{}", self.name, self.version)
    }
}

/// Why the router gave a task to its worker. Fallbacks and retries inherit tasks that already
/// failed, so they never count toward the new worker's first-pass rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assignment {
    /// The best by posterior mean.
    Exploit,
    /// Another candidate won the draw.
    Explore,
    /// The first worker stopped, and the task fell back to this one.
    Fallback,
    /// The same task again after a failure.
    Retry,
}

/// A whole run, as the canary and learned workflow statistics see it. Appended to the outcome log
/// by the run review, after the run's tasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run: RunId,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub task_type: String,
    /// The learned workflow version the run used, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learned_workflow: Option<LearnedWorkflowRef>,
    /// The workflow it ran and that file's SHA-256, for retrieving workflows that did well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<(String, String)>,
    pub outcome: RunOutcome,
    /// Tasks that came to you.
    pub stuck_tasks: u32,
    /// Whether the final checks passed on the first try.
    pub final_checks_passed: bool,
    pub cost_usd: f64,
    pub minutes: f64,
    /// Whether this run was part of a learned workflow version's canary.
    #[serde(default)]
    pub canary: bool,
}

/// One replayed task under one learned workflow version, as the promotion pipeline compares them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayResult {
    /// The replay set entry.
    pub task: String,
    pub learned_workflow: LearnedWorkflowRef,
    /// Which of the task's runs this was, from 1.
    pub try_number: u32,
    /// Whether the hidden acceptance tests passed on the result.
    pub passed: bool,
    pub cost_usd: f64,
    pub minutes: f64,
    pub holdout: bool,
}
