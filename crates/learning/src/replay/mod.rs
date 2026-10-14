//! Replays: past tasks run again with live agents, in fresh repositories whose history stops at
//! each task's base commit, to compare a learned workflow version with its parent at equal limits.
//! This is re-execution, which costs real money and varies run to run, not the free log replay that
//! recovery uses.
//!
//! The set and its hidden tests live in Argonauts Code's state, outside every sandbox's read
//! scope, and are applied only when a result is scored. Agents never see which tasks are in it.
//!
//! `replay/` holds `entries/<id>.json`, `proposals/<id>.json`, the `tuning.json` and `holdout.json`
//! id lists, `tests/<id>/` with each entry's hidden tests at their repo paths, `setup/<id>.json`
//! for scoring checkouts, `split.jsonl` with every seeded draw, `spend.jsonl` with what replays
//! cost by month, `progress/` for a replay waiting on the evolution limit, and `work/` for the
//! scoring checkouts, which are deleted as soon as they're scored. The repositories agents replay
//! in live beside it, in `replay-repos/`, so no sandbox needs to see into `replay/`.

mod checks;
mod files;
mod isolated;
mod names;
mod run;
mod set;
mod stopping;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::RunId;
use argonauts_core::future::BoxFuture;
use argonauts_core::names::ModelRef;
use argonauts_core::workflow::StandardPhase;

use crate::error::Result;
use crate::learned_workflows::LoadedLearnedWorkflow;
use crate::store::Store;
use crate::types::ReplayResult;

pub use isolated::isolated_repo;
pub use run::{Versions, replay, replay_workers, score, start_phase};
pub use set::{SplitReason, SplitRecord, draw_seed, holdout_draw};
pub use stopping::{StoppingRule, Verdict};

/// A past task in the replay set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// The run it came from.
    pub run: RunId,
    /// The request as you gave it.
    pub task: String,
    pub task_type: String,
    /// The commit before the change.
    pub base: String,
    /// The commit the change merged as.
    pub merged: String,
    /// The acceptance test files, stored in the set, never in a worktree: copies under
    /// `replay/tests/<id>/`, each at its repo path below that.
    pub tests: Vec<PathBuf>,
    /// The argument vector that runs them.
    pub test_command: Vec<String>,
    pub limit_usd: f64,
    pub holdout: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub accepted: OffsetDateTime,
}

/// A finished run's task, checked for the set: its tests fail at the base commit, pass with
/// the merged change, and don't lean on names or files the request never mentions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub entry: Entry,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

impl Proposal {
    pub fn valid(&self) -> bool {
        self.checks.iter().all(|check| check.passed)
    }
}

/// The tuning and holdout sets on disk.
pub struct ReplaySet {
    store: Store,
}

/// One task under one learned workflow version.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayRequest {
    /// The task, without its hidden tests or its side of the split.
    pub entry: Entry,
    pub learned_workflow: LoadedLearnedWorkflow,
    /// The isolated repository.
    pub repo: PathBuf,
    /// The same for both versions.
    pub limit_usd: f64,
    /// Prefix replay: the recorded outputs of the phases before this one are reused.
    pub from_phase: Option<StandardPhase>,
    /// The workers allowed, at pinned versions, on API keys or endpoints unless
    /// `evolution.replay.use_subscriptions` is on.
    pub workers: Vec<ModelRef>,
    /// What every task of the replay keeps hidden: the hidden tests at their repo paths, and
    /// the replay folder that holds them. Never the tests themselves.
    pub hidden_paths: Vec<String>,
}

/// What a replayed run left behind.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayRun {
    pub cost_usd: f64,
    pub minutes: f64,
    /// The commit to score, or `None` when the run ended without one.
    pub head: Option<String>,
}

/// Runs a replayed task with live agents. The app implements it by starting a supervised run in the
/// isolated repo with the learned workflow's settings, only the request's workers, the limit as a
/// hard cap, and `merge.mode = "leave"`, then returning the commit it left; tests use a fake.
pub trait ReplayRunner: Send + Sync {
    fn run<'a>(&'a self, request: &'a ReplayRequest) -> BoxFuture<'a, Result<ReplayRun>>;
}

/// A candidate's replay so far.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReplayReport {
    pub results: Vec<ReplayResult>,
    pub spent_usd: f64,
    /// The stopping rule ended it early.
    pub stopped_early: bool,
    /// The month's evolution limit is spent; it resumes once there is room under it again.
    pub at_limit: bool,
}
