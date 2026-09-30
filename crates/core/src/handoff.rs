//! Handoff checkpoints and quality debt: marking a card's state when another worker takes it
//! over, and tracking work that a weaker replacement did until a strong enough model checks it.

use serde::{Deserialize, Serialize};

use crate::names::WorkerRef;
use crate::{CardId, RunId};

/// A git tag on a card's commit at a handoff. It stays put while the card's branch moves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckpointTag {
    pub run: RunId,
    pub card: CardId,
    /// 1 for the card's first handoff.
    pub n: u32,
}

impl CheckpointTag {
    /// `argonauts/<run>/<card>/handoff-<n>`.
    pub fn tag_name(&self) -> String {
        format!("argonauts/{}/{}/{}", self.run, self.card, self.label())
    }

    /// `handoff-<n>`, as `/revert` and the transcript name it.
    pub fn label(&self) -> String {
        format!("handoff-{}", self.n)
    }

    /// Where `/revert` keeps the work after this checkpoint: `argonauts/<run>/<card>-replaced-<n>`.
    pub fn replaced_branch(&self) -> String {
        format!("argonauts/{}/{}-replaced-{}", self.run, self.card, self.n)
    }

    /// The `n` of a `handoff-<n>` label.
    pub fn parse_label(label: &str) -> Option<u32> {
        label
            .strip_prefix("handoff-")
            .filter(|n| n.chars().all(|c| c.is_ascii_digit()))
            .and_then(|n| n.parse().ok())
            .filter(|n| *n > 0)
    }
}

/// Commits since a checkpoint by a replacement weaker than the card's original owner. Your own
/// commits after a take-over never count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityDebt {
    /// The `n` of the checkpoint the debt starts at.
    pub since: u32,
    pub by: WorkerRef,
    /// The strongest previous owner. A review by it, or by a model at least as strong, clears
    /// the debt.
    pub needs: WorkerRef,
}
