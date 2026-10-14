//! Every step of the run is a setting with a default, so an ablation is a config diff.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;
use crate::names::RoleName;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct IntakeSettings {
    /// Include uncommitted changes in the run's base. They stay yours to commit: the run's
    /// commit holds only its own work, and your edits are still uncommitted afterwards. Off:
    /// the run works from the recorded commit, and its first line says what was left out.
    pub include_uncommitted: bool,
}

/// Captain-or-crew thresholds. The agent supplies facts; these rules pick Captain or crew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CaptainOrCrewSettings {
    /// Force a mode for every request, as the `captain` workflow does.
    pub force: Option<ForcedMode>,
    pub use_learned: bool,
    /// Captain at this many files or fewer...
    pub captain_max_files: u32,
    /// ...and only when tests exist.
    pub captain_requires_tests: bool,
    pub crew_min_files: u32,
    pub crew_labels: Vec<String>,
    pub otherwise: CaptainOrCrewOtherwise,
    /// Captain that these rules pick, rather than you: checked, so it can still grow into a crew
    /// run, direct, or as `captain.kind` says.
    pub captain_kind: CaptainOrCrewKind,
    /// The estimate lists how the request says to run it, and code follows what passes its
    /// checks.
    pub read_instructions: bool,
}

impl Default for CaptainOrCrewSettings {
    fn default() -> Self {
        Self {
            force: None,
            use_learned: true,
            captain_max_files: 3,
            captain_requires_tests: true,
            crew_min_files: 8,
            crew_labels: vec![
                "architecture".to_owned(),
                "migration".to_owned(),
                "security".to_owned(),
            ],
            otherwise: CaptainOrCrewOtherwise::CaptainThenCrew,
            captain_kind: CaptainOrCrewKind::Checked,
            read_instructions: true,
        }
    }
}

/// Which Captain the Captain-or-crew rules pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptainOrCrewKind {
    #[default]
    Checked,
    Direct,
    /// Whatever `captain.kind` says.
    AsCaptain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ForcedMode {
    Captain,
    Crew,
}

/// What the rules pick between the thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptainOrCrewOtherwise {
    /// Captain first, then crew if the Captain task needs the crew.
    CaptainThenCrew,
    Captain,
    Crew,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CaptainSettings {
    /// Captain you choose: the main session in your checkout, like its CLI, or a checked worktree.
    pub kind: CaptainKind,
    /// What a direct turn asks you in supervised mode.
    pub asks: crate::session::Asks,
    /// A snapshot before each direct turn, for `/rewind`.
    pub rewind: bool,
    /// Untracked files bigger than this can't be restored by `/rewind`.
    pub rewind_max_mb: u64,
    /// In Captain the main session may still start a workflow when your own words ask for the
    /// crew, other agents or a checked worktree (`argonauts_start_workflow`). Off, nothing leaves
    /// it.
    pub start_workflow_tool: bool,
    /// The main agent reads the request back and waits for approval before building. Makes Captain
    /// checked.
    pub understand_first: bool,
    /// Review by another provider (`maker-checker`). Makes Captain checked.
    pub review: bool,
    /// Independent tries, keeping one that passes the visible tests (`best-of-n`). Above 1
    /// makes Captain checked.
    pub tries: u32,
}

impl CaptainSettings {
    /// Whether Captain you choose runs as a direct turn. Each of review, tries, and reading back
    /// first needs a worktree, so any of them makes Captain checked without touching `kind`.
    pub fn direct(&self) -> bool {
        self.kind == CaptainKind::Direct && !self.needs_a_worktree()
    }

    /// Review, understanding first, or more than one try each need a worktree to work in.
    pub fn needs_a_worktree(&self) -> bool {
        self.review || self.understand_first || self.tries > 1
    }
}

impl Default for CaptainSettings {
    fn default() -> Self {
        Self {
            kind: CaptainKind::Direct,
            asks: crate::session::Asks::LikeCli,
            rewind: true,
            rewind_max_mb: 50,
            start_workflow_tool: true,
            understand_first: false,
            review: false,
            tries: 1,
        }
    }
}

/// Which Captain runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptainKind {
    /// A turn of the main session in your checkout, as its CLI works.
    #[default]
    Direct,
    /// A worktree, one task, checks, and a merge into your branch.
    Checked,
}

/// `[understand]`: models from several providers read your request before anything runs, and
/// what they leave open becomes an assumption or a question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UnderstandSettings {
    /// In every crew run, only when the main agent decides, or never. Captain runs skip it.
    pub when: UnderstandWhen,
    /// Readers; `None` means every crew member.
    pub readers: Option<u32>,
    /// Comparers, from other model providers than the readings they compare. One listing is
    /// enough to proceed.
    pub comparers: u32,
    pub max_questions: u32,
    /// Topics that are always a question, never a majority's silent assumption. Who answers
    /// them is `approvals.answers`.
    pub always_ask: Vec<String>,
    /// Adopt a two-of-three reading outside `always_ask` as a stated assumption.
    pub majority_assumptions: bool,
}

impl Default for UnderstandSettings {
    fn default() -> Self {
        Self {
            when: UnderstandWhen::Crew,
            readers: None,
            comparers: 2,
            max_questions: 3,
            always_ask: ["scope", "security", "data", "cost"]
                .map(str::to_owned)
                .to_vec(),
            majority_assumptions: true,
        }
    }
}

/// `understand.when`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnderstandWhen {
    /// Every crew run opens with Understand, the original design.
    Crew,
    /// The main agent decides, as Claude Code's orchestrator decides whether to scout first.
    Main,
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DraftingSettings {
    pub by: PlanningBy,
    /// Drafting agents can't see each other's drafts until all are sealed.
    pub blind: bool,
    /// Drafting agents; `None` means every crew member.
    pub agents: Option<u32>,
}

impl Default for DraftingSettings {
    fn default() -> Self {
        Self {
            by: PlanningBy::Crew,
            blind: true,
            agents: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanningBy {
    /// Blind drafts, the map, critique, judging.
    Crew,
    /// The orchestrator's structured plan, checked by code.
    Orchestrator,
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
    /// `rotate` (one designated critic per run, rotating across runs) or an `agent/model`.
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
pub struct ExperimentSettings {
    pub enabled: bool,
    /// Dollars for all experiments in a run.
    pub limit_usd: f64,
    /// Minutes per experiment option.
    pub time_box_min: f64,
}

impl Default for ExperimentSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            limit_usd: 2.0,
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
    /// The winner's margin (out of 10) below which it's a near tie for you.
    pub clear_winner_margin: f64,
    /// Keep a judge's place for a model that didn't draft.
    pub reserve_non_drafting_judge: bool,
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
            reserve_non_drafting_judge: false,
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
    Orchestrator,
    Human,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DivideSettings {
    /// An interface task, then acceptance-test tasks, before build tasks.
    pub contracts_first: bool,
    pub acceptance_tests_by: TestAuthor,
    /// How chores (merge, docs) are assigned.
    pub chores: ChoreAssignment,
    pub max_tasks: u32,
    pub predictions: bool,
    /// One worker builds the whole plan (`draft-and-vote`).
    pub single_worker: bool,
}

impl Default for DivideSettings {
    fn default() -> Self {
        Self {
            contracts_first: true,
            acceptance_tests_by: TestAuthor::Reviewer,
            chores: ChoreAssignment::Rotate,
            max_tasks: 12,
            predictions: true,
            single_worker: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestAuthor {
    /// The task's ring reviewer.
    Reviewer,
    /// Nobody: no test tasks.
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
    /// Reviewers come from another provider than the worker.
    pub other_provider: bool,
    pub per_task: u32,
    /// Reviewers can't see each other's reviews until the judge settles them.
    pub blind: bool,
    /// Rounds of requested changes before the task needs you.
    pub max_change_rounds: u32,
    /// Diff lines a reviewer or judge sees in one piece. Longer diffs go in per-file chunks,
    /// with a note saying what was cut.
    pub max_diff_lines: u32,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            assign: ReviewAssignment::Ring,
            other_provider: true,
            per_task: 1,
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
    /// `roles.reviewer.models`, in order.
    Fixed,
    Orchestrator,
}

/// Checks. The built-in checks (protected paths, secrets, test tampering, dependencies, stub
/// check) always run and aren't settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CheckSettings {
    /// Commands run in each task's sandbox, in order: lint, types, unit tests.
    pub run: Vec<CheckCommand>,
    /// The full suite for the final check. Unset reuses `run`.
    pub full_suite: Vec<CheckCommand>,
    pub timeout: Span,
    /// Commands Argonauts Code runs once in each new task worktree before its first check, such
    /// as installing from the lockfile. They run outside the agent's sandbox, with install
    /// scripts off where the tool allows it.
    pub setup: Vec<CheckCommand>,
    /// Untracked files copied into each new worktree, as globs, such as `.env.test`. Nothing
    /// else untracked is copied.
    pub copy_untracked: Vec<String>,
}

impl Default for CheckSettings {
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
pub struct CheckCommand {
    pub name: String,
    /// The argument vector. No shell.
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RoutingSettings {
    pub algorithm: RoutingAlgorithm,
    /// With epsilon-greedy, the share of tasks that go to a less proven model. Thompson
    /// sampling explores by its own draws, within max_explore_share.
    pub explore_rate: f64,
    pub no_explore_labels: Vec<String>,
    /// Models whose sampled pass rate is within this much of the best count as tied. Ties go
    /// to the lower cost estimate, then to the model with fewer running tasks.
    pub tie_band: f64,
    /// How much each part of a track record counts when the router compares models.
    pub weights: RoutingWeights,
    /// Tasks of evidence a prior is worth.
    pub prior_strength: f64,
    /// Days after which an observation counts half. The prior doesn't fade.
    pub half_life_days: f64,
    /// Most of a run's tasks that may go to a less proven model, such as 0.2 for one in five.
    pub max_explore_share: f64,
    /// Recent tasks a model needs on a task type before it's scored on that type alone rather
    /// than its group.
    pub own_type_after: u32,
    /// Share of a model's old evidence that still counts once its id changes.
    pub model_change_keep: f64,
    /// How sure a run review must be before it says one model is better at a task type.
    pub claim_above: f64,
    /// An average prediction this far above the actual rate flags a model as overconfident.
    pub overconfident_margin: f64,
    /// Predictions a model needs before it can be flagged as overconfident.
    pub overconfident_after: u32,
    /// Coarse groups of task types, scored together while evidence is sparse.
    pub groups: BTreeMap<String, Vec<String>>,
    /// Starting rates per model and task type, such as from your experience or a leaderboard.
    /// They fade as evidence arrives.
    pub priors: BTreeMap<String, BTreeMap<String, PriorRates>>,
    /// First-pass rates per model and task type that stay fixed, whatever the outcomes.
    pub pins: BTreeMap<String, BTreeMap<String, f64>>,
    /// Per model, a date such as 2026-10-01 before which its outcomes don't count.
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
    /// Weight of the first-pass check rate.
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
    /// A starting first-pass check rate, 0 to 1.
    pub first_pass_rate: Option<f64>,
    /// A starting review approval rate, 0 to 1.
    pub approval_rate: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingAlgorithm {
    /// Thompson sampling over track records.
    Bandit,
    /// The best posterior mean, except for `explore_rate` of the tasks.
    EpsilonGreedy,
    RoundRobin,
    /// `roles.worker.models`, in order.
    Fixed,
    Orchestrator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct VisibilitySettings {
    pub drafts_hidden_until_sealed: bool,
    /// Blind reviewers can't see each other's reviews until the judge settles them.
    pub reviews_hidden_until_settled: bool,
    pub judges_see_authors: bool,
    /// Per-role switch for the status summary at session start and through the MCP tool.
    pub status_summary: BTreeMap<RoleName, bool>,
}

impl Default for VisibilitySettings {
    fn default() -> Self {
        Self {
            drafts_hidden_until_sealed: true,
            reviews_hidden_until_settled: true,
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
    /// Untracked files Argonauts Code leaves out of its commits, such as `coverage/` or `*.log`. A
    /// pattern ending in `/` is a folder. Empty by default, so commits take every file git
    /// doesn't ignore.
    pub leave_out: Vec<String>,
    /// Whether its commits also leave out a built-in list of build and cache output, such as
    /// `__pycache__/`, `*.pyc`, and `node_modules/`. Off by default, since what counts as build
    /// output varies by language and tool. Each commit says what it left out.
    pub leave_out_builtin: bool,
    /// How a run lands when your branch had no commit at its start: as changes to your files
    /// for you to commit, or as the branch's first commit, made from your files as the run
    /// found them and its work.
    pub unborn: super::delivery::UnbornLanding,
}

impl Default for MergeSettings {
    fn default() -> Self {
        Self {
            mode: MergeMode::Ask,
            target: None,
            pr_draft: false,
            leave_out: Vec::new(),
            leave_out_builtin: false,
            unborn: super::delivery::UnbornLanding::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeMode {
    Ask,
    /// Keep the branch and finish.
    Leave,
    /// Open a pull request and finish, as a learned workflow's canary runs do.
    PullRequest,
}
