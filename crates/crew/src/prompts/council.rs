//! What a council's seats and checkers read: the seat's frame with its letters, its role, the
//! note keeper's duty, each turn of the talk, and the check that never learns who made the
//! change.

pub const SEAT: &str = argonauts_core::template!("prompts/council/seat.md");
pub const SKEPTIC_NOTE: &str = argonauts_core::template!("prompts/council/skeptic-note.md");
pub const PROPOSER: &str = argonauts_core::template!("prompts/council/proposer.md");
pub const SKEPTIC: &str = argonauts_core::template!("prompts/council/skeptic.md");
pub const CHECKER: &str = argonauts_core::template!("prompts/council/checker.md");
pub const NOTES: &str = argonauts_core::template!("prompts/council/notes.md");
pub const TURN: &str = argonauts_core::template!("prompts/council/turn.md");
pub const CHECK: &str = argonauts_core::template!("prompts/council/check.md");
