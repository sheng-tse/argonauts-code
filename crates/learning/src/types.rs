//! Types more than one part of the learning crate reads. Each part owns its own module; these
//! are the shapes they hand each other, so they stay fixed while the parts are built.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::RunId;
use argonauts_core::events::run::RunOutcome;

/// One version of one recipe, such as `api-feature` v3.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RecipeRef {
    pub name: String,
    pub version: u32,
}

impl std::fmt::Display for RecipeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} v{}", self.name, self.version)
    }
}

/// Why the router gave a card to its owner. Fallbacks and retries inherit cards that already
/// failed, so they never count toward the new owner's first-pass rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assignment {
    /// The best by posterior mean.
    Exploit,
    /// Another candidate won the draw.
    Explore,
    /// The first owner stopped, and this one took over.
    Fallback,
    /// The same card again after a failure.
    Retry,
}

/// A whole run, as the canary and recipe statistics see it. Appended to the outcome log by the
/// retro, after the run's cards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run: RunId,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub task_type: String,
    /// The recipe version the run used, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<RecipeRef>,
    /// The workflow it ran and that file's SHA-256, for retrieving workflows that did well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<(String, String)>,
    pub outcome: RunOutcome,
    /// Cards that escalated to you.
    pub escalations: u32,
    /// Whether the final checks passed on the first try.
    pub final_checks_passed: bool,
    pub cost_usd: f64,
    pub minutes: f64,
    /// Whether this run was part of a recipe version's canary.
    #[serde(default)]
    pub canary: bool,
}

/// One replayed task under one recipe version, as the promotion pipeline compares them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayResult {
    /// The replay set entry.
    pub task: String,
    pub recipe: RecipeRef,
    /// Which of the task's runs this was, from 1.
    pub attempt: u32,
    /// Whether the hidden acceptance tests passed on the result.
    pub passed: bool,
    pub cost_usd: f64,
    pub minutes: f64,
    pub holdout: bool,
}
