//! Task steps: predictions, builds, acceptance tests, merge conflicts, the run review, and the
//! closing answer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};
use crate::events::approval::ASSUMED;

/// A worker's probabilities for its task, scored with the Brier score in the run review.
pub struct Predict;

impl Step for Predict {
    const NAME: StepName = StepName::Predict;
    type Input = PredictInput;
    type Output = Predictions;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PredictInput {
    /// Title and task.
    pub task: String,
    pub time_cap_min: f64,
}

/// Probabilities between 0 and 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Predictions {
    pub p_pass_checks_first_try: f64,
    pub p_within_time_cap: f64,
    pub p_at_most_two_tries: f64,
}

pub struct Build;

impl Step for Build {
    const NAME: StepName = StepName::Build;
    type Input = BuildInput;
    type Output = TaskReport;
}

/// An interface worker resolves a merge conflict in a build session, like [`Build`].
pub struct ResolveConflict;

impl Step for ResolveConflict {
    const NAME: StepName = StepName::ResolveConflict;
    type Input = BuildInput;
    type Output = TaskReport;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BuildInput {
    /// Title and task.
    pub task: String,
    pub owned_paths: Vec<String>,
    pub read_only_paths: Vec<String>,
    /// SHA-256 of the approved goal. Argonauts Code refuses a build session without it.
    pub goal_sha256: String,
}

/// The report every task ends with. A superset of the 0.0.1 `Report`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskReport {
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
    /// Questions for the worker of another piece, or for you. Posted to the blackboard.
    #[serde(default)]
    pub questions: Vec<String>,
    /// Requests to change something outside the task's paths or the plan.
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

impl TaskReport {
    /// What the agent says it decided in the person's place: its concerns' lines marked
    /// `Assumed:`, without the mark.
    pub fn assumptions(&self) -> Vec<&str> {
        self.concerns
            .iter()
            .flat_map(|concern| concern.lines())
            .filter_map(assumption)
            .collect()
    }
}

/// What a concern's line says was assumed, when it's marked `Assumed:`.
pub fn assumption(line: &str) -> Option<&str> {
    let line = line.trim().trim_start_matches(['-', '*', ' ']);
    let marked = line.get(..ASSUMED.len())?.eq_ignore_ascii_case(ASSUMED);
    let assumed = line[ASSUMED.len()..].trim().trim_end_matches('.');
    (marked && !assumed.is_empty()).then_some(assumed)
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

/// An endpoint reviewer returns acceptance tests as files; Argonauts Code writes and runs them.
pub struct AcceptanceTests;

impl Step for AcceptanceTests {
    const NAME: StepName = StepName::AcceptanceTests;
    type Input = AcceptanceTestsInput;
    type Output = AcceptanceTestsOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceTestsInput {
    pub task: String,
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
    /// Path inside the test task's owned paths.
    pub path: String,
    pub content: String,
}

/// The run review agent reviews the run and proposes at most one process change.
pub struct RunReview;

impl Step for RunReview {
    const NAME: StepName = StepName::RunReview;
    type Input = RunReviewInput;
    type Output = RunReviewOutput;
}

/// A code-built digest of the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunReviewInput {
    pub summary: String,
    /// Failures, stuck tasks and retries.
    pub trouble: Vec<String>,
    pub evolvable: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunReviewOutput {
    pub change: Option<ProcessChange>,
}

/// The run's closing answer, written by the main agent from the tasks' reports once a crew run
/// ends.
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
    /// Each task's report, the ones whose work merged first. Only those are what changed; the
    /// others are what's left.
    pub tasks: Vec<TaskDigest>,
    /// The changed files with their line counts.
    pub files: Vec<String>,
    /// The checks Argonauts Code ran and their results.
    pub checks: Vec<String>,
    /// Each reviewer and sign-off asked, and what it said, such as `review:per-key (Haiku 4.5
    /// on Claude Code): accepted`.
    #[serde(default)]
    pub asked: Vec<String>,
    /// How many times the run came to the person for an answer or a decision.
    #[serde(default)]
    pub needed_you: u32,
    /// What the run decided in the person's place on autopilot, one line each. They are shown
    /// beside the answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decided_on_autopilot: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskDigest {
    pub title: String,
    /// `merged` when its work is in what the run delivers, else how it ended, such as
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

/// One setting change for a learned workflow candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessChange {
    /// A key on the evolvable list.
    pub setting: String,
    /// The new value, as TOML text.
    pub value: String,
    pub why: String,
}
