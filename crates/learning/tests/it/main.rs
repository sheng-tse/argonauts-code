//! The tests that use this crate from outside, one module per file, built as one program so
//! that a change links it once instead of once per file.

mod records_support;
mod replay_support;

mod evolution_learned_workflows;
mod evolution_loop;
mod records_book;
mod records_outcomes;
mod records_predictions;
mod records_router;
mod replay_runs;
mod replay_set;
mod skills_export;
mod skills_sync;
