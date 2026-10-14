//! Fallback checkpoints and quality debt: marking a task's state when it falls back to another
//! model, and tracking work that a weaker fallback did until a strong enough model checks it.

use serde::{Deserialize, Serialize};

use crate::names::ModelRef;
use crate::{RunId, TaskId};

/// A git tag on a task's commit at a fallback. It stays put while the task's branch moves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckpointTag {
    pub run: RunId,
    pub task: TaskId,
    /// 1 for the task's first fallback.
    pub n: u32,
}

impl CheckpointTag {
    /// `argonauts/<run>/<task>/fallback-<n>`.
    pub fn tag_name(&self) -> String {
        format!("argonauts/{}/{}/{}", self.run, self.task, self.label())
    }

    /// `fallback-<n>`, as `/rewind` and the transcript name it.
    pub fn label(&self) -> String {
        format!("fallback-{}", self.n)
    }

    /// Where `/rewind` keeps the work after this checkpoint: `argonauts/<run>/<task>-replaced-<n>`.
    pub fn replaced_branch(&self) -> String {
        format!("argonauts/{}/{}-replaced-{}", self.run, self.task, self.n)
    }

    /// The `n` of a `fallback-<n>` label.
    pub fn parse_label(label: &str) -> Option<u32> {
        label
            .strip_prefix("fallback-")
            .filter(|n| n.chars().all(|c| c.is_ascii_digit()))
            .and_then(|n| n.parse().ok())
            .filter(|n| *n > 0)
    }
}

/// Commits since a checkpoint by a fallback weaker than the task's original model. Your own
/// commits after a take-over never count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityDebt {
    /// The `n` of the checkpoint the debt starts at.
    pub since: u32,
    pub by: ModelRef,
    /// The strongest previous model. A review by it, or by a model at least as strong, clears
    /// the debt.
    pub needs: ModelRef,
}
