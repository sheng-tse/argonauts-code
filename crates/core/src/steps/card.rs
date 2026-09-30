//! Card steps: predictions, builds, acceptance tests, merge conflicts, and the retro.

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
    pub files_changed: Vec<String>,
    /// Test commands run and their outcomes.
    pub tests_run: Vec<String>,
    /// Risks and follow-ups.
    pub concerns: Vec<String>,
    /// Questions for the owner of another piece, or for you. Posted to the blackboard.
    pub questions: Vec<String>,
    /// Requests to change something outside the card's paths or the plan.
    pub change_requests: Vec<ChangeRequestOut>,
    /// Notes worth keeping for later sessions, checked by the safety scan.
    pub memory_proposals: Vec<MemoryProposal>,
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

/// A note proposed for `.argonauts/notes`. Data, never instructions.
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

/// One setting change for a recipe candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessChange {
    /// A key on the evolvable list.
    pub setting: String,
    /// The new value, as TOML text.
    pub value: String,
    pub why: String,
}
