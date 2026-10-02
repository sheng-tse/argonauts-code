//! The replay set on disk: entries, proposals, the seeded split, the holdout's retirement, and
//! the month's spend.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rand::rngs::StdRng;
use rand::{RngExt as _, SeedableRng as _};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::RunId;
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::GateRunner;
use argonauts_core::settings::Settings;
use argonauts_core::settings::evolution::ReplaySettings;

use super::files::{
    append_json_line, read_json, read_json_dir, read_json_lines, remove_if_present, write_json,
};
use super::{Entry, Proposal, ReplaySet, checks};
use crate::error::{LearningError, Result};
use crate::store::Store;
use crate::types::RecipeRef;

/// Beside `replay/`, so an agent's sandbox can be given a replay repository without any view
/// into the hidden folder.
const REPOS_DIR: &str = "replay-repos";

/// One draw of the split, or a holdout task retired into tuning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitRecord {
    pub entry: String,
    /// The seed the draw used; `None` for a retirement.
    pub seed: Option<u64>,
    pub holdout: bool,
    pub reason: SplitReason,
    /// Which holdout this was: it counts up each time the holdout retires.
    pub generation: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitReason {
    /// The seed decided.
    Drawn,
    /// The seed drew the holdout, but it already had `holdout_tasks`.
    HoldoutFull,
    /// The holdout checked `holdout_retire_after` candidates, so its tasks joined tuning.
    Retired,
}

/// A fresh seed for `ReplaySet::accept`, drawn by the conductor, never by an agent or by you,
/// and kept with the split it decides.
pub fn draw_seed() -> u64 {
    rand::random()
}

/// Whether a seed puts a task in the holdout, which takes `holdout_share` of new tasks.
pub fn holdout_draw(seed: u64, holdout_share: f64) -> bool {
    StdRng::seed_from_u64(seed).random::<f64>() < holdout_share
}

/// The split's rule, as the conductor last read it from your settings, and the holdout's
/// count of candidates. `accept` has no settings of its own, so it uses these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SetState {
    tuning_tasks: u32,
    holdout_tasks: u32,
    holdout_retire_after: u32,
    candidates: u32,
    generation: u32,
}

impl Default for SetState {
    fn default() -> Self {
        let mut state = Self {
            tuning_tasks: 0,
            holdout_tasks: 0,
            holdout_retire_after: 0,
            candidates: 0,
            generation: 0,
        };
        state.follow(&ReplaySettings::default());
        state
    }
}

impl SetState {
    fn follow(&mut self, settings: &ReplaySettings) {
        self.tuning_tasks = settings.tasks;
        self.holdout_tasks = settings.holdout_tasks;
        self.holdout_retire_after = settings.holdout_retire_after;
    }

    /// New tasks split in proportion to the two sets' sizes.
    fn holdout_share(&self) -> f64 {
        let total = self.tuning_tasks + self.holdout_tasks;
        if total == 0 {
            0.0
        } else {
            f64::from(self.holdout_tasks) / f64::from(total)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SpendRecord {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    usd: f64,
    task: String,
    recipe: RecipeRef,
}

impl ReplaySet {
    pub fn open(store: &Store) -> Result<Self> {
        Ok(Self {
            store: store.clone(),
        })
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Accepted entries, oldest first.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut entries: Vec<Entry> = read_json_dir(&self.dir().join("entries"))?;
        entries.sort_by(|a, b| a.accepted.cmp(&b.accepted).then_with(|| a.id.cmp(&b.id)));
        Ok(entries)
    }

    pub fn tuning(&self) -> Result<Vec<Entry>> {
        let mut entries = self.entries()?;
        entries.retain(|entry| !entry.holdout);
        Ok(entries)
    }

    pub fn holdout(&self) -> Result<Vec<Entry>> {
        let mut entries = self.entries()?;
        entries.retain(|entry| entry.holdout);
        Ok(entries)
    }

    pub fn entry(&self, id: &str) -> Result<Option<Entry>> {
        read_json(&self.entry_path(checked(id)?))
    }

    /// Proposals waiting for you, under `evolution.replay.accept_entries = "ask"`.
    pub fn proposals(&self) -> Result<Vec<Proposal>> {
        read_json_dir(&self.dir().join("proposals"))
    }

    /// Every draw and retirement, in order.
    pub fn splits(&self) -> Result<Vec<SplitRecord>> {
        read_json_lines(&self.dir().join("split.jsonl"))
    }

    /// Checks a finished run's task for the set and keeps it as a proposal, or accepts it at
    /// once under `accept_entries = "auto"` when every check passed. Runs that didn't merge,
    /// or left no acceptance tests, give `None`, as does a run already in the set.
    pub fn consider<'a>(
        &'a self,
        run: &'a RunId,
        settings: &'a Settings,
        gates: Arc<dyn GateRunner>,
    ) -> BoxFuture<'a, Result<Option<Proposal>>> {
        Box::pin(checks::consider(self, run, settings, gates))
    }

    /// Accepts a proposal. The conductor, not an agent, puts it in tuning or holdout at random:
    /// the seed draws the side, and the draw is recorded in `split.jsonl`.
    pub fn accept(&self, id: &str, seed: u64) -> Result<Entry> {
        let id = checked(id)?;
        let proposal: Proposal = read_json(&self.proposal_path(id))?.ok_or_else(|| {
            LearningError::Refused(format!("there's no replay proposal named {id}"))
        })?;
        if !proposal.valid() {
            let failed: Vec<&str> = proposal
                .checks
                .iter()
                .filter(|check| !check.passed)
                .map(|check| check.name.as_str())
                .collect();
            return Err(LearningError::Refused(format!(
                "{id} can't join the replay set: it failed {}",
                failed.join(", ")
            )));
        }
        let state = self.state()?;
        let drawn = holdout_draw(seed, state.holdout_share());
        let room = self.holdout()?.len() < state.holdout_tasks as usize;
        let mut entry = proposal.entry;
        entry.holdout = drawn && room;
        entry.accepted = OffsetDateTime::now_utc();
        write_json(&self.entry_path(id), &entry)?;
        remove_if_present(&self.proposal_path(id))?;
        self.record_split(SplitRecord {
            entry: entry.id.clone(),
            seed: Some(seed),
            holdout: entry.holdout,
            reason: if drawn && !room {
                SplitReason::HoldoutFull
            } else {
                SplitReason::Drawn
            },
            generation: state.generation,
            at: entry.accepted,
        })?;
        self.write_lists()?;
        Ok(entry)
    }

    /// Drops a proposal you turned down, with its hidden tests.
    pub fn reject(&self, id: &str) -> Result<()> {
        let id = checked(id)?;
        if self.entry(id)?.is_some() {
            return Err(LearningError::Refused(format!(
                "{id} is already in the replay set"
            )));
        }
        remove_if_present(&self.proposal_path(id))?;
        remove_if_present(&self.hidden_tests_dir(id))?;
        remove_if_present(&self.scoring_setup_path(id))
    }

    /// Counts a candidate the holdout checked; after `holdout_retire_after`, its tasks join
    /// tuning and fresh tasks fill the holdout as they're accepted.
    pub fn holdout_used(&self, settings: &Settings) -> Result<()> {
        let mut state = self.state()?;
        state.follow(&settings.evolution.replay);
        state.candidates += 1;
        if state.candidates >= state.holdout_retire_after.max(1) {
            let at = OffsetDateTime::now_utc();
            for mut entry in self.holdout()? {
                entry.holdout = false;
                write_json(&self.entry_path(&entry.id), &entry)?;
                self.record_split(SplitRecord {
                    entry: entry.id,
                    seed: None,
                    holdout: false,
                    reason: SplitReason::Retired,
                    generation: state.generation,
                    at,
                })?;
            }
            state.candidates = 0;
            state.generation += 1;
        }
        self.write_state(&state)?;
        self.write_lists()
    }

    /// Candidates the current holdout has checked.
    pub fn holdout_candidates(&self) -> Result<u32> {
        Ok(self.state()?.candidates)
    }

    /// What replays spent in the calendar month (UTC) that holds `at`.
    pub fn spent_in_month(&self, at: OffsetDateTime) -> Result<f64> {
        let records: Vec<SpendRecord> = read_json_lines(&self.spend_path())?;
        Ok(records
            .iter()
            .filter(|record| record.at.year() == at.year() && record.at.month() == at.month())
            // From +0.0: an empty f64 sum is -0.0, which prints as $-0.00.
            .fold(0.0, |total, record| total + record.usd))
    }

    /// `limits.evolution_usd_per_month` less what replays spent this month.
    pub fn budget_left(&self, settings: &Settings, now: OffsetDateTime) -> Result<f64> {
        let spent = self.spent_in_month(now)?;
        Ok((settings.limits.evolution_usd_per_month - spent).max(0.0))
    }

    /// Where each replay's fresh repository goes.
    pub fn repos_dir(&self) -> PathBuf {
        self.store.state_dir.join(REPOS_DIR)
    }

    pub(super) fn dir(&self) -> PathBuf {
        self.store.replay_dir()
    }

    pub(super) fn work_dir(&self) -> PathBuf {
        self.dir().join("work")
    }

    pub(super) fn hidden_tests_dir(&self, id: &str) -> PathBuf {
        self.dir().join("tests").join(id)
    }

    /// What a replay's cards keep hidden: each of the entry's hidden tests at its repo path,
    /// and the replay folder that holds them.
    pub(super) fn hidden_paths(&self, entry: &Entry) -> Vec<String> {
        let root = self.hidden_tests_dir(&entry.id);
        let mut paths: Vec<String> = entry
            .tests
            .iter()
            .filter_map(|test| test.strip_prefix(&root).ok())
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .collect();
        paths.push(self.dir().to_string_lossy().into_owned());
        paths
    }

    pub(super) fn scoring_setup_path(&self, id: &str) -> PathBuf {
        scoring_setup_path(&self.dir(), id)
    }

    pub(super) fn progress_path(&self, key: &str) -> PathBuf {
        self.dir().join("progress").join(format!("{key}.json"))
    }

    pub(super) fn proposal_path(&self, id: &str) -> PathBuf {
        self.dir().join("proposals").join(format!("{id}.json"))
    }

    pub(super) fn refresh_rule(&self, settings: &ReplaySettings) -> Result<()> {
        let mut state = self.state()?;
        state.follow(settings);
        self.write_state(&state)
    }

    pub(super) fn record_spend(&self, task: &str, recipe: &RecipeRef, usd: f64) -> Result<()> {
        append_json_line(
            &self.spend_path(),
            &SpendRecord {
                at: OffsetDateTime::now_utc(),
                usd,
                task: task.to_owned(),
                recipe: recipe.clone(),
            },
        )
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.dir().join("entries").join(format!("{id}.json"))
    }

    fn spend_path(&self) -> PathBuf {
        self.dir().join("spend.jsonl")
    }

    fn state(&self) -> Result<SetState> {
        Ok(read_json(&self.dir().join("state.json"))?.unwrap_or_default())
    }

    fn write_state(&self, state: &SetState) -> Result<()> {
        write_json(&self.dir().join("state.json"), state)
    }

    fn record_split(&self, record: SplitRecord) -> Result<()> {
        append_json_line(&self.dir().join("split.jsonl"), &record)
    }

    /// The two id lists, rewritten from the entries whenever the split changes.
    fn write_lists(&self) -> Result<()> {
        let entries = self.entries()?;
        let ids = |holdout: bool| -> Vec<&str> {
            entries
                .iter()
                .filter(|entry| entry.holdout == holdout)
                .map(|entry| entry.id.as_str())
                .collect()
        };
        write_json(&self.dir().join("tuning.json"), &ids(false))?;
        write_json(&self.dir().join("holdout.json"), &ids(true))
    }
}

/// Entry ids are run ids, which can't climb out of a folder.
fn checked(id: &str) -> Result<&str> {
    RunId::new(id)
        .map(|_| id)
        .map_err(|_| LearningError::Refused(format!("{id:?} isn't a replay entry")))
}

pub(super) fn scoring_setup_path(replay_dir: &Path, id: &str) -> PathBuf {
    replay_dir.join("setup").join(format!("{id}.json"))
}
