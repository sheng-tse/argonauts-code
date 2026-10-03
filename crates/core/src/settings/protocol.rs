//! Every step of the run is a setting with a default, so an ablation is a config diff.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;
use crate::names::RoleName;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct IntakeSettings {
    /// Include uncommitted changes in the run's base. Off: the run works from the recorded
    /// commit, and its first line says what was left out.
    pub include_uncommitted: bool,
}

/// Triage thresholds. The agent supplies facts; these rules pick solo or crew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TriageSettings {
    /// Force a mode for every request, as the `solo` presets do.
    pub force: Option<ForcedMode>,
    /// All hands: every task goes to the crew, and the lead designs its workflow. Questions
    /// are still just answered, and `/solo` still runs one task solo.
    pub all_hands: bool,
    pub use_recipes: bool,
    /// Solo at this many files or fewer...
    pub solo_max_files: u32,
    /// ...and only when tests exist.
    pub solo_requires_tests: bool,
    pub crew_min_files: u32,
    pub crew_labels: Vec<String>,
    pub otherwise: TriageMiddle,
}

impl TriageSettings {
    /// The All hands key, which `/effort`'s toggle writes.
    pub const ALL_HANDS: &'static str = "triage.all_hands";
}

impl Default for TriageSettings {
    fn default() -> Self {
        Self {
            force: None,
            all_hands: false,
            use_recipes: true,
            solo_max_files: 3,
            solo_requires_tests: true,
            crew_min_files: 8,
            crew_labels: vec![
                "architecture".to_owned(),
                "migration".to_owned(),
                "security".to_owned(),
            ],
            otherwise: TriageMiddle::SoloThenCrew,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ForcedMode {
    Solo,
    Crew,
}

/// What triage does between the thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriageMiddle {
    /// Solo first, then crew if the solo card escalates.
    SoloThenCrew,
    Solo,
    Crew,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SoloSettings {
    /// The default agent restates the goal and waits for approval before building.
    pub restate_first: bool,
    /// Review by another model family (`solo-reviewed`).
    pub review: bool,
    /// Independent attempts, keeping one that passes the visible tests (`best-of-n`).
    pub attempts: u32,
}

impl Default for SoloSettings {
    fn default() -> Self {
        Self {
            restate_first: false,
            review: false,
            attempts: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GoalSettings {
    /// Restaters; `None` means every crew member.
    pub restaters: Option<u32>,
    /// Differs, from different model families. One listing is enough to proceed.
    pub differs: u32,
    pub max_questions: u32,
    /// Tags that always become questions, never assumptions.
    pub always_ask: Vec<String>,
    /// Adopt a two-of-three reading outside `always_ask` as a stated assumption.
    pub majority_assumptions: bool,
}

impl Default for GoalSettings {
    fn default() -> Self {
        Self {
            restaters: None,
            differs: 2,
            max_questions: 3,
            always_ask: ["scope", "security", "data", "cost"]
                .map(str::to_owned)
                .to_vec(),
            majority_assumptions: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DraftingSettings {
    pub by: PlanningBy,
    /// Drafters can't see each other's drafts until all are sealed.
    pub blind: bool,
    /// Drafters; `None` means every crew member.
    pub drafters: Option<u32>,
}

impl Default for DraftingSettings {
    fn default() -> Self {
        Self {
            by: PlanningBy::Crew,
            blind: true,
            drafters: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanningBy {
    /// Blind drafts, the map, critique, judging.
    Crew,
    /// The coordinator's structured plan, checked by code.
    Coordinator,
}

/// The disagreement map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MappingSettings {
    /// Check every quote word for word against its draft.
    pub quote_check: bool,
    pub remap_after_revision: bool,
}

impl Default for MappingSettings {
    fn default() -> Self {
        Self {
            quote_check: true,
            remap_after_revision: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CritiqueSettings {
    /// Rounds. 0 turns critique off.
    pub rounds: u32,
    /// `rotate` (one designated critic per run, rotating across runs) or a `worker/model`.
    pub critic: String,
    pub checklist: Vec<String>,
}

impl Default for CritiqueSettings {
    fn default() -> Self {
        Self {
            rounds: 1,
            critic: "rotate".to_owned(),
            checklist: [
                "Does it meet every success criterion?",
                "What breaks first under load or failure?",
                "What is hard to test?",
                "What is more complex than it needs to be?",
                "What does it cost to run?",
            ]
            .map(str::to_owned)
            .to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SpikeSettings {
    pub enabled: bool,
    /// Dollars for all spikes in a run.
    pub budget_usd: f64,
    /// Minutes per spike option.
    pub time_box_min: f64,
}

impl Default for SpikeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            budget_usd: 2.0,
            time_box_min: 15.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct JudgingSettings {
    pub method: JudgingMethod,
    /// Judges see "Plan 1, Plan 2", never authors.
    pub anonymize: bool,
    pub both_orders: bool,
    pub rubric: Vec<String>,
    /// The winner's lead (out of 10) below which it's a near tie for you.
    pub clear_winner_margin: f64,
    /// Reserve a judge seat for a worker that didn't draft.
    pub reserve_non_drafting_seat: bool,
}

impl Default for JudgingSettings {
    fn default() -> Self {
        Self {
            method: JudgingMethod::Cross,
            anonymize: true,
            both_orders: true,
            rubric: [
                "correctness_risk",
                "completeness",
                "testability",
                "simplicity",
                "cost",
            ]
            .map(str::to_owned)
            .to_vec(),
            clear_winner_margin: 1.0,
            reserve_non_drafting_seat: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JudgingMethod {
    /// Each agent scores only the other plans.
    Cross,
    /// One non-author judge.
    Single,
    Vote,
    Coordinator,
    Human,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DivideSettings {
    /// An interface card, then acceptance-test cards, before build cards.
    pub contracts_first: bool,
    pub acceptance_tests_by: TestAuthor,
    /// How chores (integration, docs) are assigned.
    pub chores: ChoreAssignment,
    pub max_cards: u32,
    pub predictions: bool,
    /// One owner builds the whole plan (`draft-and-vote`).
    pub single_owner: bool,
}

impl Default for DivideSettings {
    fn default() -> Self {
        Self {
            contracts_first: true,
            acceptance_tests_by: TestAuthor::Reviewer,
            chores: ChoreAssignment::Rotate,
            max_cards: 12,
            predictions: true,
            single_owner: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestAuthor {
    /// The card's ring reviewer.
    Reviewer,
    /// Nobody: no test cards.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChoreAssignment {
    Rotate,
    Router,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ReviewSettings {
    pub assign: ReviewAssignment,
    /// Reviewers come from another model family than the owner.
    pub other_family: bool,
    pub per_card: u32,
    /// Reviewers can't see each other's reviews until adjudication.
    pub blind: bool,
    /// Rounds of requested changes before the card escalates.
    pub max_change_rounds: u32,
    /// Diff lines a reviewer or approver sees in one piece. Longer diffs go in per-file chunks,
    /// with a note saying what was cut.
    pub max_diff_lines: u32,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            assign: ReviewAssignment::Ring,
            other_family: true,
            per_card: 1,
            blind: false,
            max_change_rounds: 2,
            max_diff_lines: 4000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAssignment {
    Ring,
    Random,
    /// `roles.reviewer.workers`, in order.
    Fixed,
    Coordinator,
}

/// Gates. The built-in checks (protected paths, secrets, test tampering, dependencies, stub
/// check) always run and aren't settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GateSettings {
    /// Commands run in each card's sandbox, in order: lint, types, unit tests.
    pub run: Vec<GateCommand>,
    /// The full suite for the final check. Unset reuses `run`.
    pub full_suite: Vec<GateCommand>,
    pub timeout: Span,
    /// Commands the conductor runs once in each new card worktree before its first gate, such
    /// as installing from the lockfile. They run outside the agent's sandbox, with install
    /// scripts off where the tool allows it.
    pub setup: Vec<GateCommand>,
    /// Untracked files copied into each new worktree, as globs, such as `.env.test`. Nothing
    /// else untracked is copied.
    pub copy_untracked: Vec<String>,
}

impl Default for GateSettings {
    fn default() -> Self {
        Self {
            run: Vec::new(),
            full_suite: Vec::new(),
            timeout: Span::new("30m"),
            setup: Vec::new(),
            copy_untracked: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GateCommand {
    pub name: String,
    /// The argument vector. No shell.
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RoutingSettings {
    pub algorithm: RoutingAlgorithm,
    /// With epsilon-greedy, the share of cards that go to a less proven worker. Thompson
    /// sampling explores by its own draws, within max_explore_share.
    pub explore_rate: f64,
    /// Skip a vendor for new cards above this share of its quota window.
    pub skip_quota_above: f64,
    pub no_explore_labels: Vec<String>,
    /// Workers whose sampled pass rate is within this much of the best count as tied. Ties go
    /// to the lower cost estimate, then to the worker with fewer running cards.
    pub tie_band: f64,
    /// How much each part of a track record counts when the router compares workers.
    pub weights: RoutingWeights,
    /// Cards of evidence a prior is worth.
    pub prior_strength: f64,
    /// Days after which an observation counts half. The prior doesn't fade.
    pub half_life_days: f64,
    /// Most of a run's cards that may go to a less proven worker, such as 0.2 for one in five.
    pub max_explore_share: f64,
    /// Recent cards a worker needs on a task type before it's scored on that type alone rather
    /// than its group.
    pub own_type_after: u32,
    /// Share of a worker's old evidence that still counts once its model id changes.
    pub model_change_keep: f64,
    /// How sure a retro must be before it says one worker is better at a task type.
    pub claim_above: f64,
    /// An average prediction this far above the actual rate flags a worker as overconfident.
    pub overconfident_margin: f64,
    /// Predictions a worker needs before it can be flagged as overconfident.
    pub overconfident_after: u32,
    /// Coarse groups of task types, scored together while evidence is sparse.
    pub groups: BTreeMap<String, Vec<String>>,
    /// Starting rates per worker and task type, such as from your experience or a leaderboard.
    /// They fade as evidence arrives.
    pub priors: BTreeMap<String, BTreeMap<String, PriorRates>>,
    /// First-pass rates per worker and task type that stay fixed, whatever the outcomes.
    pub pins: BTreeMap<String, BTreeMap<String, f64>>,
    /// Per worker, a date such as 2026-10-01 before which its outcomes don't count.
    pub resets: BTreeMap<String, String>,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        let group = |name: &str, types: &[&str]| {
            (
                name.to_owned(),
                types.iter().map(|kind| (*kind).to_owned()).collect(),
            )
        };
        Self {
            algorithm: RoutingAlgorithm::Bandit,
            explore_rate: 0.1,
            skip_quota_above: 0.9,
            no_explore_labels: vec!["security".to_owned(), "migration".to_owned()],
            tie_band: 0.05,
            weights: RoutingWeights::default(),
            prior_strength: 3.0,
            half_life_days: 30.0,
            max_explore_share: 0.2,
            own_type_after: 10,
            model_change_keep: 0.2,
            claim_above: 0.9,
            overconfident_margin: 0.2,
            overconfident_after: 10,
            groups: BTreeMap::from([
                group("frontend", &["frontend"]),
                group("backend_and_data", &["backend", "data"]),
                group("infra_and_tests", &["infra", "tests"]),
                group("docs_and_refactor", &["docs", "refactor"]),
                group("security_and_performance", &["security", "performance"]),
            ]),
            priors: BTreeMap::new(),
            pins: BTreeMap::new(),
            resets: BTreeMap::new(),
        }
    }
}

/// The router's utility is the weighted sum of the rates minus the weighted cost and time
/// ratios.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RoutingWeights {
    /// Weight of the first-pass gate rate.
    pub pass_rate: f64,
    /// Weight of the review approval rate.
    pub approval_rate: f64,
    /// Weight of the prediction score.
    pub prediction: f64,
    /// Weight of cost against the task type's median.
    pub cost: f64,
    /// Weight of wall time against the task type's median.
    pub time: f64,
}

impl Default for RoutingWeights {
    fn default() -> Self {
        Self {
            pass_rate: 1.0,
            approval_rate: 0.5,
            prediction: 0.3,
            cost: 0.4,
            time: 0.2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PriorRates {
    /// A starting first-pass gate rate, 0 to 1.
    pub first_pass_rate: Option<f64>,
    /// A starting review approval rate, 0 to 1.
    pub approval_rate: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingAlgorithm {
    /// Thompson sampling over track records.
    Bandit,
    /// The best posterior mean, except for `explore_rate` of the cards.
    EpsilonGreedy,
    RoundRobin,
    /// `roles.owner.workers`, in order.
    Fixed,
    Coordinator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct VisibilitySettings {
    pub drafts_hidden_until_sealed: bool,
    /// Blind reviewers can't see each other's reviews until adjudication.
    pub reviews_hidden_until_adjudicated: bool,
    pub judges_see_authors: bool,
    /// Per-role switch for the status summary at session start and through the MCP tool.
    pub status_summary: BTreeMap<RoleName, bool>,
}

impl Default for VisibilitySettings {
    fn default() -> Self {
        Self {
            drafts_hidden_until_sealed: true,
            reviews_hidden_until_adjudicated: true,
            judges_see_authors: false,
            status_summary: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MergeSettings {
    pub mode: MergeMode,
    /// The branch an approval lands on: the one checked out at start, or a named one.
    pub target: Option<String>,
    pub pr_draft: bool,
    /// Untracked files Argonauts leaves out of its commits, such as `coverage/` or `*.log`. A
    /// pattern ending in `/` is a folder. Empty by default, so commits take every file git
    /// doesn't ignore.
    pub leave_out: Vec<String>,
    /// Whether its commits also leave out a built-in list of build and cache output, such as
    /// `__pycache__/`, `*.pyc`, and `node_modules/`. Off by default, since what counts as build
    /// output differs by language and tool. Each commit says what it left out.
    pub leave_out_builtin: bool,
}

impl Default for MergeSettings {
    fn default() -> Self {
        Self {
            mode: MergeMode::Ask,
            target: None,
            pr_draft: false,
            leave_out: Vec::new(),
            leave_out_builtin: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeMode {
    Ask,
    /// Keep the branch and finish.
    Leave,
    /// Open a pull request and finish, as a recipe's canary runs do.
    PullRequest,
}
