//! The run engine: a workflow runner. A run is a workflow of typed steps (Understand,
//! planning, dividing, tasks, merge) done by a crew of agents. Agents think inside their
//! sessions; code decides what runs next, who does it, and whether it passed.

use argonauts_core::engine::RunEngine;
use argonauts_core::events::run::RunMode;
use argonauts_core::settings::Settings;

pub mod approvals;
pub mod blackboard;
pub mod captain;
pub mod captain_or_crew;
pub mod change;
pub mod claude_codes;
pub mod closing;
pub mod consultant;
pub mod divide;
pub mod engine;
pub mod error;
pub mod fallback;
pub mod follow;
pub mod git;
pub mod goal;
pub mod idempotency;
pub mod labels;
pub mod lifetime;
pub mod merge;
pub mod orchestrated;
pub mod plan;
pub mod prompts;
pub mod replay_terms;
pub mod review;
pub mod roles;
pub mod run_review;
pub mod script;
pub mod spec;
pub mod state;
pub mod status;
pub mod task;
pub mod tie_breaker;
pub mod ultra;
pub mod understand;
pub mod workflow;
pub mod workflow_size;

#[cfg(test)]
mod testutil;

pub use error::{EngineError, EngineResult};

/// Recorded as the failed run's detail while the engine can't run a workflow yet.
pub const ENGINE_NOT_BUILT: &str = "crew engine not built yet";

pub fn engine_for(settings: &Settings, mode: RunMode) -> Box<dyn RunEngine> {
    Box::new(engine::CrewEngine::new(settings, mode))
}
