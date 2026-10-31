//! The run engine: a workflow runner. A run is a workflow of typed steps (Understand,
//! planning, dividing, tasks, merge) done by a crew of agents. Agents think inside their
//! sessions; code decides what runs next, who does it, and whether it passed.

use argonauts_core::runs::engine::RunEngine;
use argonauts_core::runs::events::run::RunMode;
use argonauts_core::settings::Settings;

pub mod approvals;
pub mod blackboard;
pub mod council;
pub mod divide;
pub mod engine;
pub mod fallback;
pub mod follow;
pub mod git;
pub mod goal;
pub mod merge;
pub mod plan;
pub mod prompts;
pub mod review;
pub mod roles;
pub mod run_review;
pub mod script;
pub mod sessions;
pub mod state;
pub mod status;
pub mod task;
pub mod understand;
pub mod workflow;

#[cfg(test)]
mod testing;

pub use engine::error::{EngineError, EngineResult};

/// Recorded as the failed run's detail while the engine can't run a workflow yet.
pub const ENGINE_NOT_BUILT: &str = "crew engine not built yet";

pub fn engine_for(settings: &Settings, mode: RunMode) -> Box<dyn RunEngine> {
    Box::new(engine::CrewEngine::new(settings, mode))
}
