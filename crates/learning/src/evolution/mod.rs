//! The evolution loop. The run review proposes one change; the mutation rules check it before a
//! cent is spent; a replay compares it with its parent task by task against a threshold fixed in
//! advance, then on the holdout; the promotion approval decides under your approval settings; and a
//! canary watches the next runs and rolls back on any tripwire.
//!
//! Candidates and canaries live in Argonauts Code's state, outside every sandbox, so a version
//! under test never reaches your repo until it has earned it. A promotion that passes its
//! canary becomes a commit on its own branch that waits for you; a rollback takes effect at once.

mod canary;
mod candidate;
mod pipeline;
mod rules;
pub mod state;
mod stats;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::RunId;
use argonauts_core::settings::Settings;
use argonauts_core::steps::task::ProcessChange;

use crate::error::{LearningError, Result};
use crate::learned_workflows::{self, Change, ChangeClass, LoadedLearnedWorkflow};
use crate::store::Store;
use crate::types::{LearnedWorkflowRef, RunRecord};

pub use rules::{check_evolvable, check_mutation};
pub use stats::{
    BOOTSTRAP_RESAMPLES, NOT_WORSE_FLOOR, compare, holdout_ok, holdout_passes, holdout_tasks,
    tripwires,
};

/// The stages of autonomy, from `evolution.stage` and `approvals.workflow_promotion.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Saves what worked as a learned workflow.
    Record,
    /// The run review proposes; you apply.
    Suggest,
    /// Replay-tested candidates wait for you at the promotion approval.
    Evolve,
    /// Your rules promote the classes they allow; replay, holdout, and canary still run.
    Auto,
}

pub fn stage(settings: &Settings) -> Stage {
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::evolution::EvolutionStage;
    match settings.evolution.stage {
        EvolutionStage::Record => Stage::Record,
        EvolutionStage::Suggest => Stage::Suggest,
        EvolutionStage::Evolve => match settings.approvals.workflow_promotion.mode {
            ApprovalMode::Auto | ApprovalMode::Autopilot => Stage::Auto,
            ApprovalMode::Supervised | ApprovalMode::AutoPreview => Stage::Evolve,
        },
    }
}

/// The bar a candidate must clear, copied from the settings when it's queued and never moved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    pub min_gain_points: f64,
    pub max_cost_increase: f64,
    /// Only "not worse": the lower end of the gain stays above -5 points.
    pub not_worse: bool,
}

/// A proposed learned workflow version and where it stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Such as `api-feature-v4`.
    pub id: String,
    pub parent: LearnedWorkflowRef,
    pub version: u32,
    pub change: Change,
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_run: Option<RunId>,
    pub thresholds: Thresholds,
    pub state: CandidateState,
    #[serde(with = "time::serde::rfc3339")]
    pub queued: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CandidateState {
    /// Stage 2: shown to you, applied by hand.
    Suggested,
    /// Waiting for the evolution limit or its turn.
    Queued,
    Replaying {
        spent_usd: f64,
    },
    /// The replay or holdout said no, or the mutation rules did. Logged with why.
    Rejected {
        reason: String,
    },
    /// Passed replay and holdout; the promotion approval decides.
    AwaitingPromotion,
    Canary {
        runs: u32,
    },
    /// Passed its canary.
    Current,
    RolledBack {
        reason: String,
    },
}

/// The comparison with the parent, task by task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// The estimated gain in pass rate, in points.
    pub gain_points: f64,
    /// The lower end of the one-sided 90% paired bootstrap interval, resampling tasks.
    pub lower_bound: f64,
    /// Cost change as a fraction of the parent's.
    pub cost_change: f64,
    pub passed: bool,
    pub reasons: Vec<String>,
}

/// What can trip a canary run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tripwire {
    StuckTasks,
    FailedFinalChecks,
    Cost,
    /// You rejected one of its approvals.
    Rejected,
    /// You reverted one of its merges.
    Reverted,
}

/// What the caller does after the loop moves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Replay this candidate when the limit and the replay worker allow.
    Replay { candidate: String },
    /// Open the promotion approval, with this evidence packet.
    AskPromotion {
        candidate: String,
        packet: serde_json::Value,
    },
    /// A version became current; its branch waits for you.
    Promoted {
        learned_workflow: LearnedWorkflowRef,
        branch: String,
        pr_command: Vec<String>,
    },
    RolledBack {
        learned_workflow: LearnedWorkflowRef,
        to: u32,
        reason: String,
    },
    /// Stage 1: a workflow that worked, proposed as a learned workflow's first version.
    Recorded {
        learned_workflow: LearnedWorkflowRef,
        branch: String,
    },
    /// Something you should know, in one line.
    Tell { text: String },
}

/// The class a change falls in, for the promotion rules.
pub fn class_of(change: &Change) -> ChangeClass {
    change.class
}

/// The loop's state on disk.
pub struct Evolution {
    store: Store,
    /// A fixed clock, when one is set.
    now: Option<OffsetDateTime>,
}

impl Evolution {
    pub fn open(store: &Store) -> Result<Self> {
        Ok(Self {
            store: store.clone(),
            now: None,
        })
    }

    /// Reads the time from `now` instead of the system clock, for tests and for replaying the
    /// loop's own decisions.
    pub fn at(mut self, now: OffsetDateTime) -> Self {
        self.now = Some(now);
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    fn now(&self) -> OffsetDateTime {
        self.now.unwrap_or_else(OffsetDateTime::now_utc)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.store.evolution_dir().join(name)
    }

    pub fn candidates(&self) -> Result<Vec<Candidate>> {
        state::read(&self.file(state::CANDIDATES))
    }

    fn save_candidates(&self, candidates: &state::Candidates) -> Result<()> {
        state::write(&self.file(state::CANDIDATES), candidates)
    }

    pub fn candidate(&self, id: &str) -> Result<Option<Candidate>> {
        Ok(self.candidates()?.into_iter().find(|c| c.id == id))
    }

    /// Canaries running now, by learned workflow.
    pub fn canaries(&self) -> Result<state::Canaries> {
        state::read(&self.file(state::CANARIES))
    }

    fn save_canaries(&self, canaries: &state::Canaries) -> Result<()> {
        state::write(&self.file(state::CANARIES), canaries)
    }

    fn learned_workflow_states(&self) -> Result<state::LearnedWorkflows> {
        state::read(&self.file(state::LEARNED_WORKFLOWS))
    }

    fn save_learned_workflow_states(
        &self,
        learned_workflows: &state::LearnedWorkflows,
    ) -> Result<()> {
        state::write(&self.file(state::LEARNED_WORKFLOWS), learned_workflows)
    }

    /// The candidate's own learned workflow folder, where its replays read it from.
    pub fn candidate_dir(&self, id: &str) -> PathBuf {
        self.file(state::CANDIDATE_DIRS).join(id)
    }

    /// A candidate's learned workflow as written in its folder.
    pub fn candidate_learned_workflow(&self, id: &str) -> Result<LoadedLearnedWorkflow> {
        let candidate = self
            .candidate(id)?
            .ok_or_else(|| LearningError::Refused(format!("there's no candidate `{id}`")))?;
        learned_workflows::load_file(
            &self
                .candidate_dir(id)
                .join(learned_workflows::version_file(candidate.version)),
        )
    }

    /// The version a learned workflow's runs use now: a canary's, else the current one.
    pub fn running_version(&self, learned_workflow: &str) -> Result<Option<u32>> {
        if let Some(canary) = self.canaries()?.get(learned_workflow) {
            return Ok(Some(canary.version));
        }
        let checkout = self.checkout_current(learned_workflow)?;
        let mut states = self.learned_workflow_states()?;
        let Some(pointer) = states
            .get(learned_workflow)
            .and_then(|state| state.pointer.clone())
        else {
            return Ok(checkout);
        };
        if pointer.checkout_was == checkout {
            return Ok(Some(pointer.version));
        }
        // You merged the pointer's branch or moved `current` yourself: the checkout speaks for
        // the learned workflow again, and the old pointer must not wake up if `current` moves back.
        if let Some(state) = states.get_mut(learned_workflow) {
            state.pointer = None;
        }
        self.save_learned_workflow_states(&states)?;
        Ok(checkout)
    }

    /// The learned workflow version a run of `learned_workflow` should use, from the checkout or,
    /// for a canary or a promotion that hasn't merged yet, from Argonauts Code's state.
    pub fn running(&self, learned_workflow: &str) -> Result<Option<LoadedLearnedWorkflow>> {
        match self.running_version(learned_workflow)? {
            Some(version) => self.load_version(learned_workflow, version),
            None => Ok(None),
        }
    }

    fn checkout_set(
        &self,
        learned_workflow: &str,
    ) -> Result<Option<learned_workflows::LearnedWorkflowSet>> {
        let folder = self.store.learned_workflows_dir().join(learned_workflow);
        if folder.is_dir() {
            learned_workflows::load_set(&folder)
        } else {
            Ok(None)
        }
    }

    fn checkout_current(&self, learned_workflow: &str) -> Result<Option<u32>> {
        Ok(self.checkout_set(learned_workflow)?.map(|set| set.current))
    }

    /// A version from the checkout, or from a candidate's folder when only the loop has it.
    pub fn load_version(
        &self,
        learned_workflow: &str,
        version: u32,
    ) -> Result<Option<LoadedLearnedWorkflow>> {
        if let Some(loaded) = self
            .checkout_set(learned_workflow)?
            .and_then(|set| set.version(version).cloned())
        {
            return Ok(Some(loaded));
        }
        let candidate = self
            .candidates()?
            .into_iter()
            .find(|c| c.parent.name == learned_workflow && c.version == version);
        match candidate {
            Some(candidate) => self.candidate_learned_workflow(&candidate.id).map(Some),
            None => Ok(None),
        }
    }

    /// One past every version the checkout or a candidate has used, so ids never repeat.
    fn next_version(&self, learned_workflow: &str) -> Result<u32> {
        let checkout = self
            .checkout_set(learned_workflow)?
            .and_then(|set| set.versions.last().map(|l| l.learned_workflow.version))
            .unwrap_or(0);
        let candidates = self
            .candidates()?
            .iter()
            .filter(|c| c.parent.name == learned_workflow)
            .map(|c| c.version)
            .max()
            .unwrap_or(0);
        Ok(checkout.max(candidates) + 1)
    }

    /// When a version of the learned workflow last started its canary, which a rollback doesn't
    /// undo.
    pub fn last_promoted(&self, learned_workflow: &str) -> Result<Option<OffsetDateTime>> {
        Ok(self
            .learned_workflow_states()?
            .get(learned_workflow)
            .and_then(|state| state.last_promoted))
    }

    /// After a run's run review: record the run under its learned workflow, check a canary's
    /// tripwires, save what worked (stage 1), and queue the run review's proposal under the stage
    /// in effect.
    pub fn after_run(
        &self,
        settings: &Settings,
        run: &RunRecord,
        proposal: Option<&ProcessChange>,
    ) -> Result<Vec<Action>> {
        let stage = stage(settings);
        let mut actions = match &run.learned_workflow {
            Some(learned_workflow) => self.watch_canary(settings, run, learned_workflow)?,
            None => self.record(settings, run)?,
        };
        if let Some(proposal) = proposal
            && stage != Stage::Record
        {
            actions.extend(self.propose(settings, run, proposal, stage)?);
        }
        if matches!(stage, Stage::Evolve | Stage::Auto) {
            actions.extend(self.next_replay(settings)?);
        }
        Ok(actions)
    }

    /// What evolution spent this calendar month, against `limits.evolution_usd_per_month`.
    pub fn spent_this_month(&self, now: OffsetDateTime) -> Result<f64> {
        let ledger: state::Ledger = state::read(&self.file(state::SPEND))?;
        Ok(ledger
            .iter()
            .filter(|spend| spend.at.year() == now.year() && spend.at.month() == now.month())
            // From +0.0: an empty f64 sum is -0.0, which prints as $-0.00.
            .fold(0.0, |total, spend| total + spend.usd))
    }
}

/// The branch a learned workflow version waits on.
pub fn learned_workflow_branch(name: &str, version: u32) -> String {
    format!("argonauts/learned/{name}-v{version}")
}
