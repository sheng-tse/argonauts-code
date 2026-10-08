//! Integration and merge. Approved cards merge into `argonauts/<run>/integration` in dependency
//! order; when every card is in, main is merged in if it moved, the final check runs, and the
//! merge checkpoint decides how the run lands. Only the engine writes git state.

use argonauts_core::CardId;

use crate::engine::host::Host;
use crate::error::EngineResult;

pub mod absorb;
pub mod branch;
pub mod card_merge;
pub mod conductor;
pub mod deliver;
pub mod diff;
pub mod final_check;
pub mod in_the_way;
pub mod land;
pub mod merge;
pub mod merge_packet;
pub mod unborn;

/// What merging one approved card came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardMerge {
    /// Merged and its affected tests pass.
    Integrated { sha: String },
    /// A conflict went to the interface's owner (or the integration chore's) as a
    /// `ResolveConflict` session; the card waits for it.
    Conflict { files: Vec<String> },
    /// The affected tests failed; the card goes back to `Running` with the output.
    TestsFailed { output: String },
}

/// Called by the card pipeline in `Integrating`.
pub async fn integrate_card(host: &Host, card: &CardId) -> EngineResult<CardMerge> {
    card_merge::merge(host, card).await
}
