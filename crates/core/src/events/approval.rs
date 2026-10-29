//! Decision points: the ◆ questions that wait for you, your rules, or your judge.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::agent_status::FallbackPlan;
use super::council::{HeldWhy, TextPart};
use super::goal::{Assumption, GoalFile, GoalQuestion};
use super::plan::{DesignDecision, JudgingResult, LimitPlan, TaskGraph};
use super::run::Party;
use super::task::{CheckOutcome, TaskKind, TaskState, Way};
use crate::fallback::QualityDebt;
use crate::names::{AgentId, ApprovalId, MessageId, ModelRef, RoleName, SeatId};
use crate::policy::{AutoPreviewRecord, Verdict, Violation};
use crate::settings::approvals::{ApprovalMode, DecideWith};
use crate::settings::asking::WhenDeclined;
use crate::settings::limit::LimitAmount;
use crate::steps::judgment::ReviewVerdict;
use crate::workflow::{StepRef, Workflow, WorkflowRef};
use crate::{DiffStat, LimitDimension, Spend, SpendStatus, TaskId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ApprovalEvent {
    /// In supervised mode it waits for you and rings the bell.
    Opened(Box<ApprovalOpened>),
    /// Hard limits first, then rules or the judge.
    Checked(PolicyCheck),
    Decided(ApprovalDecided),
    /// It no longer applies, for example because the evidence changed. A new one may open.
    Withdrawn {
        id: ApprovalId,
        reason: String,
    },
    /// An auto-approval joined your spot-check queue.
    SpotCheckQueued {
        id: ApprovalId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalOpened {
    pub id: ApprovalId,
    pub kind: ApprovalKind,
    /// Such as "Merge this run into main?", the question's heading on screen.
    pub title: String,
    pub task: Option<TaskId>,
    pub packet: ApprovalPacket,
    /// An answer must carry it back, so you approve exactly what you saw.
    pub evidence_sha256: String,
    pub mode: ApprovalMode,
    /// Who decides in auto mode.
    pub decide_with: DecideWith,
    /// What Carry on without me does here, as `approvals.when_declined` said when it opened.
    #[serde(default)]
    pub when_declined: WhenDeclined,
    /// In display order.
    pub options: Vec<AnswerOption>,
    pub free_text: FreeText,
    /// False while the policy is still deciding.
    pub needs_you: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    /// The yes or no once Understand has read the request back.
    Understand,
    /// Plan, task graph, workers, test list, and limit, together.
    Plan,
    Merge,
    /// A task's agent stopped: pick another, take over, or wait.
    Fallback,
    /// Its tasks are paused.
    AgentDown,
    /// A task used up its tries.
    Stuck,
    /// A task reached its limit and no fallback could take it.
    Limit,
    /// A worker's change request that the decision rules send to you.
    ChangeRequest,
    GoalChange,
    /// A learned workflow candidate passed replay.
    WorkflowPromotion,
    TakeOverReturn,
    /// A worker asked you something directly.
    Question,
    /// The agent a task fell back from is back, and the task carries quality debt.
    OriginalReturned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalPacket {
    Understand(GoalPacket),
    Plan(Box<PlanPacket>),
    Merge(MergePacket),
    Fallback(FallbackPlan),
    AgentDown(AgentDownPacket),
    Stuck(StuckPacket),
    Limit(LimitPacket),
    ChangeRequest(ChangeRequestPacket),
    GoalChange(GoalChangePacket),
    /// Must be a JSON object without a `kind` key, since the packet's tag goes beside its
    /// fields; anything else fails to serialize.
    WorkflowPromotion(serde_json::Value),
    TakeOverReturn(TakeOverReturnPacket),
    Question(QuestionPacket),
    OriginalReturned(OriginalReturnedPacket),
    /// Whether a CLI already at work in a step may take a job of it nothing else can.
    SecondAgent(SecondAgentPacket),
    /// A council asks you what happens to a message code held, or how a point its talk left
    /// open is settled.
    Council(CouncilPacket),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalPacket {
    pub goal: GoalFile,
    pub agreements: Vec<String>,
    pub assumptions: Vec<Assumption>,
    /// Answering the last one approves the goal.
    pub questions: Vec<GoalQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanPacket {
    pub summary: String,
    /// Set when plans were judged.
    pub judging: Option<JudgingResult>,
    /// A near tie is yours to break.
    pub near_tie: bool,
    pub decisions: Vec<DesignDecision>,
    pub graph: TaskGraph,
    pub acceptance_tests: Vec<String>,
    pub limit: LimitPlan,
    pub new_dependencies: Vec<String>,
    /// Anything that blocks auto-approval, such as text addressed to judges.
    pub flags: Vec<String>,
    /// The workflow the run follows from here, with the main agent's reason when the main agent
    /// picked it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<PlanWorkflow>,
    /// The seat that builds each task a council's plan gives one, so the plan names the seat
    /// even where two seats share a model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seats: Vec<TaskSeat>,
    /// Each council seat still against a way you asked for. Your way stands, and the plan shows
    /// the objection beside its task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objections: Vec<WayObjection>,
    /// What Approve with edits offers a plan whose tasks have ways. `None` for any other plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<WaysOffer>,
}

/// A task of a council's plan and the seat whose folder and session build it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSeat {
    pub task: TaskId,
    pub seat: SeatId,
}

/// The ways a plan's tasks may take, as the setup allows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaysOffer {
    /// In the order ←/→ steps through them.
    pub ways: Vec<Way>,
    /// What the setup leaves out and where to change it, as the sheet's line reads: "Not
    /// offered: Best-of-n, Draft-and-vote · this council's setup lists two workflows · Set up
    /// Council changes it".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_offered: Option<String>,
}

/// A seat's objection to the way you asked for one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WayObjection {
    pub task: TaskId,
    pub seat: SeatId,
    /// The first sentence of the seat's newest message against your way, with the seats it
    /// names kept as seats. Empty when it objected with a tapback alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason: Vec<TextPart>,
}

impl WayObjection {
    /// The reason's words, with each seat it names written by `name`.
    pub fn words(&self, name: impl Fn(&SeatId) -> String) -> String {
        self.reason
            .iter()
            .map(|part| match part {
                TextPart::Words(words) => words.clone(),
                TextPart::Seat(seat) => name(seat),
            })
            .collect()
    }
}

/// The run's workflow as the plan approval shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanWorkflow {
    /// The template it came from, or `None` when the main agent composed it.
    pub from: Option<WorkflowRef>,
    /// The main agent's one line for the plan approval, when the main agent designed it.
    pub why: Option<String>,
    /// As validated, with its parameters filled in.
    pub workflow: Workflow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergePacket {
    /// The branch an approval lands on.
    pub target: Option<String>,
    /// The run branch (crew) or task branch (Captain).
    pub branch: String,
    pub rows: Vec<MergeRow>,
    pub suite_passed_on_latest_main: bool,
    pub checks: Vec<CheckOutcome>,
    pub combined: DiffStat,
    /// What Argonauts Code committed itself, such as the goal record and decisions: the combined
    /// diff over the paths no agent touched. With the rows, it accounts for `combined`.
    #[serde(default, skip_serializing_if = "DiffStat::is_empty")]
    pub conductor: DiffStat,
    pub diff_path: PathBuf,
    /// The merge tree's hash. An approval records it.
    pub tree_sha: String,
    pub spent: Spend,
    pub limit: Option<LimitAmount>,
    /// Set when an agent was stopped early, so the work may be partial.
    pub partial: bool,
    /// Your untracked files at paths the merge brings, which git won't overwrite. Set only on
    /// the approval that asks what to do with them; a folder ends in `/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_the_way: Vec<String>,
    /// Your uncommitted edits to files the merge changes, which git won't merge over. Set only
    /// on the approval that waits to merge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited: Vec<String>,
    /// Why git wouldn't move your checkout forward when nothing of yours was in the way, in
    /// its words. Set only on the approval that waits to merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// In a folder without git, the files you and the run changed in the same lines. Set only
    /// on the question that asks which version goes in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub both_changed: Vec<BothChanged>,
    /// The run started with your checkout on no branch. Set only on the question that asks
    /// which of your branches takes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asks_which_branch: bool,
    /// Why the question opens again, when your last answer couldn't be done: a landing git
    /// refused, or a pull request that couldn't be opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub again: Option<String>,
}

/// A file you and a run changed in the same lines since the run began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BothChanged {
    pub path: String,
    pub yours: DiffStat,
    pub runs: DiffStat,
}

impl MergePacket {
    /// The run is written into a folder without git, whose history Argonauts Code keeps: there
    /// is no branch of yours to merge into, to keep, or to open a pull request from.
    pub fn into_your_folder(&self) -> bool {
        self.target.as_deref() == Some(crate::hidden_git::BRANCH)
    }

    /// The approval waits on you alone: something of yours is in the way of the merge, git
    /// refused, or it asks what only you can say. No rule or judge can settle any of them.
    pub fn waits_on_you(&self) -> bool {
        !self.in_the_way.is_empty()
            || !self.edited.is_empty()
            || self.refused.is_some()
            || self.follows_the_merge_question()
    }

    /// The question comes after a merge question already answered, whose evidence it carries
    /// again: which version of a file both changed goes in, or which branch takes the run.
    pub fn follows_the_merge_question(&self) -> bool {
        !self.both_changed.is_empty() || self.asks_which_branch
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeRow {
    pub task: TaskId,
    pub title: String,
    pub kind: TaskKind,
    pub stat: DiffStat,
    pub worker: Option<ModelRef>,
    pub checks_passed: bool,
    pub review: Option<ReviewVerdict>,
    /// With `merge_rule = "upgrade_review"`, the task waits for a strong enough review, or
    /// your `Merge`. The question then also offers `Wait` (w) and `UpgradeReview` (a).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub debt: Vec<QualityDebt>,
    /// What the task's agent asked you that nothing waited on, or that was answered in your
    /// place, so the merge question shows it beside the task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<MergeQuestion>,
}

/// A question a task's agent asked you, as the merge question carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeQuestion {
    pub text: String,
    /// The answer given in your place; `None` while the question is still open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// The model that answered, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<ModelRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentDownPacket {
    pub agent_id: AgentId,
    /// Such as "overload errors on 3 sessions in 2 min".
    pub evidence: String,
    /// When a test task will try it again.
    #[serde(with = "time::serde::rfc3339::option")]
    pub test_at: Option<OffsetDateTime>,
    /// Paused with their work committed.
    pub paused: Vec<PausedTask>,
    /// The agent that can take every paused task, when one can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fits_all: Option<ModelRef>,
    /// The setting that keeps the paused tasks on this CLI, such as `same_cli`. Waiting is
    /// then what your own settings answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PausedTask {
    pub task: TaskId,
    pub title: String,
    pub role: RoleName,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StuckPacket {
    pub task: TaskId,
    pub title: String,
    pub state: TaskState,
    /// One line each.
    pub tries: Vec<String>,
    pub last_failure: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitPacket {
    /// `None` for the run.
    pub task: Option<TaskId>,
    /// The meter at the stop.
    pub status: SpendStatus,
    /// The limit that was hit.
    pub dimension: LimitDimension,
    pub reserve_left: Option<LimitAmount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeRequestPacket {
    pub from: Party,
    pub task: TaskId,
    pub text: String,
    pub paths: Vec<String>,
    /// Why it's yours: irreversible, changes the goal, or moves a task into a protected path.
    pub why_you: String,
    /// Tasks paused until it's decided.
    pub paused: Vec<TaskId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalChangePacket {
    pub from: Party,
    pub text: String,
    pub clarification: bool,
    pub goal: GoalFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeOverReturnPacket {
    pub task: TaskId,
    /// The model that can continue.
    pub worker: ModelRef,
    pub changed: DiffStat,
    /// Argonauts Code's commit of your changes.
    pub commit: Option<String>,
}

/// Offers `UpgradeReview` (r), `GiveBack` (o), `Redo` (u), `KeepFallback` (y),
/// `TakeOver` (t), and `ShowDiff` (d). A choice made while a step runs applies after that step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OriginalReturnedPacket {
    pub task: TaskId,
    pub title: String,
    pub original: ModelRef,
    /// Who continued the task.
    pub fallback: ModelRef,
    /// The checkpoint the diff starts at, `fallback-<n>`.
    pub since: u32,
    pub commits: u32,
    pub changed: DiffStat,
    /// Whether the checks passed on the latest commit, when they ran.
    pub tests_passed: Option<bool>,
    pub debt: Vec<QualityDebt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecondAgentPacket {
    /// The job, by the label a workflow gave it: `propose:p3`.
    pub job: String,
    /// The model it was asked of, which nothing else could take it from.
    pub stopped: ModelRef,
    /// The other model of a CLI at work in the step that would take it.
    pub taker: ModelRef,
    /// Why, and what happens either way, as the question reads.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CouncilPacket {
    /// The talk step whose council asks.
    pub talk: StepRef,
    pub about: CouncilQuestion,
    /// The question as your screen asks it.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "about", rename_all = "snake_case")]
pub enum CouncilQuestion {
    /// A seat's message that repeats what you told it in private.
    Held {
        message: MessageId,
        /// Why it's held, which the question words with the time below.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        why: Option<HeldWhy>,
        /// When you wrote the private message it repeats. Your screen writes it as a time of
        /// day in its own zone, which the engine doesn't know.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "time::serde::rfc3339::option"
        )]
        wrote_at: Option<OffsetDateTime>,
    },
    /// A point still open when the talk stopped, by its number.
    Point { point: u32 },
    /// The note keeper's plan, which repeats what you told it in private: every task reads the
    /// plan, so it waits for you before it is sealed.
    HeldPlan {
        keeper: SeatId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        why: Option<HeldWhy>,
        /// When you wrote the private message it repeats.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "time::serde::rfc3339::option"
        )]
        wrote_at: Option<OffsetDateTime>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionPacket {
    pub from: ModelRef,
    pub task: TaskId,
    pub text: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerOption {
    pub key: char,
    pub label: String,
    pub action: AnswerAction,
    pub recommended: bool,
    /// Extra text on the same row, such as a candidate's plan usage and cost.
    pub detail: Option<String>,
}

/// The engine checks that an answer is one the question offered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AnswerAction {
    Approve,
    /// The note holds the edits.
    ApproveWithEdits,
    /// The note holds the guidance.
    SendBack,
    Stop,
    Merge,
    PullRequest,
    /// The note holds the changes.
    RequestChanges,
    /// The run ends as `BranchLeft`.
    KeepBranch,
    /// Branches are kept.
    Abandon,
    Choose {
        /// An answer's label, a [`SettingScope`] name on a recorded scope question, or a
        /// guided-setup step's value.
        ///
        /// [`SettingScope`]: crate::settings::SettingScope
        value: String,
    },
    /// Answer in your own words, from the question's Type my own answer line.
    Answer,
    Candidate {
        worker: ModelRef,
    },
    /// List every valid agent, not only the fallback list (`o`).
    OtherCandidates,
    TakeOver,
    Wait,
    /// Agent down: one agent for every paused task.
    OneForAll,
    /// Agent down: choose per task.
    PerTask,
    /// The amount in the note, or the default step without one.
    RaiseLimit,
    Fallback,
    AskYou,
    Retry,
    /// The note says how.
    Split,
    Cancel,
    /// The task goes to `Deferred` and the run goes on without it.
    Defer,
    /// What a stopped session committed goes on to the checks, review, and merge, as if it had
    /// reported done.
    KeepWork,
    /// Let the agent continue after a take-over; it's told what changed.
    LetContinue,
    /// Plan approval: run it as Captain instead.
    SwitchToCaptain,
    /// Handled by the UI; never sent.
    ShowDiff,
    /// The original reviews the work since the checkpoint, or while it's still out, a
    /// candidate at least as strong does. Then it fixes the work or sends it back, and the
    /// debt clears.
    UpgradeReview,
    /// The original takes the task back from its current state, told what changed.
    GiveBack,
    /// The task resets to the checkpoint and the original redoes it. The fallback's work
    /// stays on a side branch.
    Redo,
    /// The fallback keeps the task, and the debt stays for the merge rule.
    KeepFallback,
    /// "Carry on without me", the last option the screen adds to a run's question or
    /// approval. The engine takes an option in your place, as `approvals.when_declined` says,
    /// and records that one, so this never reaches the journal.
    CarryOn,
}

/// What an agent reads as the answer to a question you pressed Esc on. What it then assumed
/// comes back in its report's concerns, marked so the run can show it for you to review.
pub const LEFT_TO_THE_AGENT: &str = "The user left this to you. Decide, carry on, and list \
                                     what you assumed in your report's `concerns`, each line \
                                     starting with \"Assumed:\".";

/// How an agent marks, in its report's concerns, something it decided in your place.
pub const ASSUMED: &str = "Assumed:";

/// What a note added with Tab on an option, or the Type my own answer line, carries on this
/// question. Text typed at the prompt never answers a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreeText {
    /// The question takes no note.
    None,
    /// The question offers Type my own answer.
    Answer,
    /// A note on request changes or send back.
    Guidance,
    /// A note on approve with edits.
    Edits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyCheck {
    pub id: ApprovalId,
    /// Any failure means it waits for you.
    pub violations: Vec<Violation>,
    pub verdict: Option<Verdict>,
    pub by: Option<DecideWith>,
    pub judge: Option<ModelRef>,
    pub reasons: Vec<String>,
    /// Recorded, not acted on.
    pub auto_preview: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalDecided {
    pub id: ApprovalId,
    pub action: AnswerAction,
    /// The note or own answer: an answer, edits, or guidance.
    pub text: Option<String>,
    pub by: Party,
    /// Must match the approval's.
    pub evidence_sha256: String,
    /// In auto preview mode, what your auto settings would have decided.
    pub auto_preview: Option<AutoPreviewRecord>,
    /// For "request changes on some tasks", the tasks sent back. Empty means every task.
    #[serde(default)]
    pub tasks: Vec<TaskId>,
}
