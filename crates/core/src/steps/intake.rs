//! Steps the app controller runs: reading a new request, and conversation answers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Step, StepName};

/// Reads a new request in the main agent's read-only conversation session.
pub struct Estimate;

impl Step for Estimate {
    const NAME: StepName = StepName::Estimate;
    type Input = IntakeInput;
    type Output = IntakeReply;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IntakeInput {
    pub request: String,
    /// "2 uncommitted files are not included", when that applies.
    pub uncommitted_note: Option<String>,
}

/// Code reads `kind` and picks the branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IntakeReply {
    pub kind: ReplyKind,
    /// The answer to show. For a change, a one-paragraph plan of attack.
    pub answer_markdown: String,
    /// Present for `change` and `unclear`.
    pub estimate: Option<ChangeEstimate>,
    /// What the request itself says about how to run it, one entry per instruction. Empty when
    /// it says nothing, and in replies from before there was a list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub how_to_run: Vec<Instruction>,
}

/// One thing a request says about how to run it, as the model sorted and copied it. The model
/// names agents and models as the request does, and code resolves and checks every one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Instruction {
    /// The request's own words for this instruction, copied exactly.
    pub says: String,
    pub kind: InstructionKind,
    /// An agent, provider, or model as the request names it ("codex", "gemini", "opus"), or
    /// empty.
    #[serde(default)]
    pub who: String,
    /// A model as the request names it, or empty.
    #[serde(default)]
    pub model: String,
    /// An effort as the request names it, or empty.
    #[serde(default)]
    pub effort: String,
    /// For `mode`, `captain` or `crew`. For `goal`, the condition in the request's own words,
    /// such as "its tests pass"; or, when the request says who keeps each task's goal going,
    /// `cli` for the CLI's own goal ("codex's own goal"), `argonauts`, or `off`. For
    /// `isolation`, `checked` or `direct`. For the other kinds, what was asked, such as "$2".
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub scope: InstructionScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstructionKind {
    /// Captain or the crew.
    Mode,
    /// Keep going until something holds: "keep going until its tests pass", "don't stop until
    /// the README explains the flag". Or who keeps each task's goal going: "have the workers
    /// use codex's own goal for each task".
    Goal,
    /// A checked worktree, or your own checkout.
    Isolation,
    /// Who is the run's main agent.
    MainAgent,
    /// Who orchestrates the crew.
    Orchestrate,
    /// Who builds.
    Build,
    /// Who reviews.
    Review,
    /// Who decides and signs off, the judge.
    Judge,
    /// Who plans, or how many plans.
    Plan,
    /// Who checks the goal.
    Check,
    /// Which agents make up the crew, as in "use only Claude" or "just Codex and agy".
    Crew,
    /// The model every agent but the one the user talks to runs on, as in "all the other agents
    /// need to be Opus". A role another instruction names keeps that one. Words that name the
    /// only agents to use, as in "use only Claude", are `crew`.
    Others,
    /// An agent to leave out.
    Exclude,
    /// Which model.
    Model,
    /// How much effort.
    Effort,
    /// What the run's models cost, as in "keep it cheap": every agent in the crew on its
    /// cheapest model. The crew itself stays as it is.
    Cost,
    Approvals,
    Merge,
    Limit,
    Network,
    /// The tests and checks a change must pass.
    Checks,
    Sandbox,
    /// Anything else about how the main agent should shape the run, such as "no drafting phase".
    ForTheMainAgent,
    /// Words for one role's agents, such as "tell the reviewers to focus on date parsing":
    /// the role in `who` ("the reviewers"), and what they are told in `value` ("focus on date
    /// parsing").
    ForARole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstructionScope {
    #[default]
    ThisRequest,
    /// The request says to keep doing it, so a question asks where to save it.
    FromNowOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplyKind {
    /// Something said in conversation, or a question. No run starts.
    Answer,
    /// A change to the repo.
    Change,
    /// It could be either; the app offers `[s] Captain [c] crew [n] no`.
    Unclear,
}

/// Facts the Captain-or-crew rules read. The agent never picks the mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeEstimate {
    /// A task type learned workflows can match, such as `docs`, `bugfix`, or `api-feature`.
    pub task_type: String,
    /// One line describing the change.
    pub summary: String,
    pub files_likely: Vec<String>,
    /// How many files, when the list is only a sample.
    pub file_count: u32,
    /// Labels such as `architecture`, `migration`, `security`, `storage`, `api`.
    pub risk_labels: Vec<String>,
    pub tests_exist: bool,
    pub unknowns: Vec<String>,
}

/// An answer only, never a run (`/ask`, or `/ask` during a run with the narrator off).
pub struct ConversationAnswer;

impl Step for ConversationAnswer {
    const NAME: StepName = StepName::ConversationAnswer;
    type Input = ConversationAnswerInput;
    type Output = ConversationAnswerOutput;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationAnswerInput {
    pub question: String,
    /// The zero-token status summary of the current run, if any.
    pub run_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationAnswerOutput {
    pub answer_markdown: String,
}
