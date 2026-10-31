//! What the crew learns from its runs, and how it changes itself within your rules.
//!
//! Every part reads what Argonauts Code measured, never what an agent says about itself, and
//! everything that changes is a file you can read, diff, and revert. The fixed core (approvals,
//! hard limits, the sandbox, checks, limit ceilings, the evaluation, and the mutation rules) is
//! read here and never written.
//!
//! - [`outcomes`]: the outcome log, one file per run, appended and never edited.
//! - [`records`]: track records derived from it, and the live `TrackRecords` service.
//! - [`routing`]: the router's utility, Thompson draws, and exploration guardrails.
//! - [`predictions`]: Brier scores, skill against the base rate, and reliability.
//! - [`learned_workflows`]: versioned workflows for task types, their structural diff, Starlark
//!   loops.
//! - [`evolution`]: mutation rules, the promotion pipeline, the canary, and the stages.
//! - [`replay`]: the replay set and replays in isolated repos.
//! - [`skills`]: exporting learned workflows as skills, the validator, imports, and drift.
//! - [`memory`]: the safety checks on notes and how sessions receive them.
//! - [`export::sync`]: `AGENTS.md` and the `CLAUDE.md` that imports it, from the conventions.
//! - [`export::scan`]: the text scan notes, skills, and imports share.
//! - [`export::package`]: the outside agent package other agents install.
//! - [`store`]: where learning keeps its files, and the types its parts share.

pub mod evolution;
pub mod export;
pub mod learned_workflows;
pub mod memory;
pub mod outcomes;
pub mod predictions;
pub mod records;
pub mod replay;
pub mod routing;
pub mod skills;
pub mod store;

pub use store::Store;
pub use store::error::{LearningError, Result};
pub use store::types::{Assignment, LearnedWorkflowRef, ReplayResult, RunRecord};
