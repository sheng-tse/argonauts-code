//! Card steps: predictions, builds, acceptance tests, merge conflicts, the retro, and the
//! closing answer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};

/// An owner's probabilities for its card, scored with the Brier score in the retro.
pub struct Predict;

impl Step for Predict {
    const NAME: StepName = StepName::Predict;
    type Input = PredictInput;
    type Output = Predictions;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PredictInput {
    /// Title and task.
    pub card: String,
    pub time_cap_min: f64,
}

/// Probabilities between 0 and 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Predictions {
    pub p_pass_gates_first_try: f64,
    pub p_within_time_cap: f64,
    pub p_at_most_two_attempts: f64,
}

pub struct Build;

impl Step for Build {
    const NAME: StepName = StepName::Build;
    type Input = BuildInput;
    type Output = CardReport;
}

/// An interface owner resolves a merge conflict in a build session, like [`Build`].
pub struct ResolveConflict;

impl Step for ResolveConflict {
    const NAME: StepName = StepName::ResolveConflict;
    type Input = BuildInput;
    type Output = CardReport;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BuildInput {
    /// Title and task.
    pub card: String,
    pub owned_paths: Vec<String>,
    pub read_only_paths: Vec<String>,
    /// SHA-256 of the approved goal. The conductor refuses a build session without it.
    pub goal_sha256: String,
}

/// The report every card ends with. A superset of the 0.0.1 `Report`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CardReport {
    pub status: ReportStatus,
    /// What was done, in a few sentences.
    pub summary: String,
    /// The files you changed or created that belong in the change, by path from the working
    /// directory. A new file your edit tools didn't write is committed only when it's listed
    /// here, so list scaffolds and generated code, and leave out caches, build output and
    /// test leftovers.
    pub files_changed: Vec<String>,
    /// Test commands run and their outcomes.
    pub tests_run: Vec<String>,
    /// Risks and follow-ups.
    pub concerns: Vec<String>,
    // The lists below came after 0.0.1's report. The strict schema still asks for them, but a
    // report without them, from a CLI that doesn't enforce the schema, still counts.
    /// Questions for the owner of another piece, or for you. Posted to the blackboard.
    #[serde(default)]
    pub questions: Vec<String>,
    /// Requests to change something outside the card's paths or the plan.
    #[serde(default)]
    pub change_requests: Vec<ChangeRequestOut>,
    /// Notes worth keeping for later sessions, checked by the safety scan.
    #[serde(default)]
    pub memory_proposals: Vec<MemoryProposal>,
    /// What you'd tell the person who asked, ending the task as your own CLI would: what you
    /// changed, how you checked it, and anything left. A few short sentences of plain prose.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answer: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Done,
    Blocked,
    Failed,
    NeedsDecision,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeRequestOut {
    pub text: String,
    pub paths: Vec<String>,
}

/// A note proposed for the crew's notes. Data, never instructions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryProposal {
    /// Such as `testing` or `redis`.
    pub topic: String,
    /// The note, at most a few sentences.
    pub text: String,
}

/// An endpoint reviewer returns acceptance tests as files; the conductor writes and runs them.
pub struct AcceptanceTests;

impl Step for AcceptanceTests {
    const NAME: StepName = StepName::AcceptanceTests;
    type Input = AcceptanceTestsInput;
    type Output = AcceptanceTestsOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceTestsInput {
    pub card: String,
    /// Goal criteria it serves.
    pub criteria: Vec<String>,
    /// The interface files, as text.
    pub interfaces: Vec<String>,
    pub test_dir: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceTestsOutput {
    pub files: Vec<TestFile>,
    pub criteria_covered: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TestFile {
    /// Path inside the test card's owned paths.
    pub path: String,
    pub content: String,
}

/// The retro agent reviews the run and proposes at most one process change.
pub struct Retro;

impl Step for Retro {
    const NAME: StepName = StepName::Retro;
    type Input = RetroInput;
    type Output = RetroOutput;
}

/// A code-built digest of the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetroInput {
    pub summary: String,
    /// Failures, escalations, and retries.
    pub trouble: Vec<String>,
    pub evolvable: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetroOutput {
    pub change: Option<ProcessChange>,
}

/// The run's closing answer, written by the lead from the cards' reports once a crew run ends.
pub struct CloseRun;

impl Step for CloseRun {
    const NAME: StepName = StepName::CloseRun;
    type Input = CloseRunInput;
    type Output = CloseRunOutput;
}

/// What the run did, written by code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CloseRunInput {
    /// What the person asked for.
    pub request: String,
    /// How the run ended, such as "merged into main".
    pub ending: String,
    /// Each card's report, the ones whose work landed first. Only those are what changed; the
    /// others are what's left.
    pub cards: Vec<CardDigest>,
    /// The changed files with their line counts.
    pub files: Vec<String>,
    /// The checks Argonauts ran and their results.
    pub checks: Vec<String>,
    /// Each reviewer and sign-off asked, and what it said, such as `review:per-key (Haiku 4.5
    /// on Claude Code): accepted`.
    #[serde(default)]
    pub asked: Vec<String>,
    /// How many times the run came to the person for an answer or a decision.
    #[serde(default)]
    pub needed_you: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CardDigest {
    pub title: String,
    /// `landed` when its work is in what the run delivers, else how it ended, such as
    /// `failed` or `cancelled`.
    #[serde(default)]
    pub state: String,
    pub summary: String,
    pub tests_run: Vec<String>,
    pub concerns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CloseRunOutput {
    /// The closing answer: what changed, how it was checked, and anything left, in a few
    /// short sentences of plain prose.
    pub answer: String,
}

/// One setting change for a recipe candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessChange {
    /// A key on the evolvable list.
    pub setting: String,
    /// The new value, as TOML text.
    pub value: String,
    pub why: String,
}
