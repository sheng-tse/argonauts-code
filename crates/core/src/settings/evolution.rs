//! The evolution loop's settings. They're safety keys: only you change them, never a workflow, a
//! learned workflow, or the loop itself, since a loop that could move its own threshold would learn
//! to game it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;

/// Keys evolution never changes, whatever `evolution.evolvable` lists, because they score or
/// constrain it: the loop's own rules, memory safety, the outcome log, who judges and reviews
/// and what they see, and the comparers. Safety keys are fixed core too. A listed key under one
/// of these is a config error.
pub const FIXED_CORE_KEYS: &[&str] = &[
    "evolution.*",
    "memory.*",
    "outcomes.*",
    "judging.*",
    "review.*",
    "visibility.*",
    "mapping.*",
    "goal.*",
    "drafting.blind",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EvolutionSettings {
    /// What the loop does on its own: record what worked, also suggest changes, or also test
    /// them by replay. Evolve with auto promotion of learned workflows is stage 4.
    pub stage: EvolutionStage,
    /// Settings that may evolve. Fixed-core and safety keys can't be listed.
    pub evolvable: Vec<String>,
    /// Runs between proposals; 0 means only when you ask.
    pub propose_every: u32,
    /// How long the run review's agent may take over a proposal. The run's end waits on it, so
    /// past this the agent is stopped and the run ends with no proposal. `"none"` waits for it
    /// however long it takes.
    pub propose_timeout: Span,
    /// Pass-rate points a candidate must gain, fixed before any replay.
    pub min_gain_points: f64,
    /// Largest cost increase allowed, as a fraction: 0.10 is 10%.
    pub max_cost_increase: f64,
    /// Change classes that only need to be no worse, within 5 points, instead of a gain.
    pub not_worse_classes: Vec<String>,
    pub replay: ReplaySettings,
    /// Runs a promoted change is watched for before it becomes current.
    pub canary_runs: u32,
    /// Whether a canary run's merge opens a pull request instead of merging.
    pub canary_pull_requests: bool,
    /// A tripwire fires when chance would explain a canary run this much worse less often than
    /// this, such as 0.05 for 1 time in 20.
    pub tripwire_p: f64,
    /// Runs of the version before a canary that its runs are compared with.
    pub tripwire_baseline_runs: u32,
}

impl Default for EvolutionSettings {
    fn default() -> Self {
        Self {
            stage: EvolutionStage::Evolve,
            evolvable: [
                "critique.rounds",
                "critique.checklist",
                "prompts.drafting",
                "divide.max_tasks",
                "limits.per_task_usd",
            ]
            .map(str::to_owned)
            .to_vec(),
            propose_every: 1,
            propose_timeout: Span::new("2m"),
            min_gain_points: 10.0,
            max_cost_increase: 0.10,
            not_worse_classes: Vec::new(),
            replay: ReplaySettings::default(),
            canary_runs: 5,
            canary_pull_requests: true,
            tripwire_p: 0.05,
            tripwire_baseline_runs: 20,
        }
    }
}

/// The stages of autonomy. Each one does what the one before it does, and more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionStage {
    /// Saves a workflow that worked as a learned workflow for its task type.
    Record,
    /// Also proposes one change every `propose_every` runs, for you to apply.
    Suggest,
    /// Also tests each proposal by replay within the evolution limit. Promotion then follows
    /// `approvals.workflow_promotion`.
    #[default]
    Evolve,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ReplaySettings {
    /// Tasks a candidate is replayed on.
    pub tasks: u32,
    /// Runs of each replayed task, per version.
    pub runs_per_task: u32,
    /// Replay only from the phase the change touches.
    pub prefix_replay: bool,
    /// Stop a replay once its result is clear, by a rule fixed before it starts.
    pub sequential: bool,
    /// Whether a validated past task joins the replay set by itself or waits for you.
    pub accept_entries: EntryAcceptance,
    /// Tasks in the holdout set, which only ever reports pass or fail.
    pub holdout_tasks: u32,
    /// Candidates the holdout checks before fresh tasks replace it.
    pub holdout_retire_after: u32,
    /// Whether replays may spend subscription plan usage. Off, they run only on API keys and
    /// endpoints you allowed, so evolution never eats the plan usage your own work needs.
    pub use_subscriptions: bool,
}

impl Default for ReplaySettings {
    fn default() -> Self {
        Self {
            tasks: 30,
            runs_per_task: 3,
            prefix_replay: true,
            sequential: true,
            accept_entries: EntryAcceptance::Auto,
            holdout_tasks: 10,
            holdout_retire_after: 10,
            use_subscriptions: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryAcceptance {
    /// A task whose checks pass joins the set, assigned to tuning or holdout at random.
    #[default]
    Auto,
    /// Each validated task waits for you.
    Ask,
}
