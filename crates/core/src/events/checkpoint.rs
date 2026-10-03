//! Decision points: the ◆ cards that wait for you, your rules, or your approver model.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::card::{CardKind, CardState, GateOutcome};
use super::goal::{Assumption, GoalFile, GoalQuestion};
use super::plan::{BudgetPlan, DesignDecision, JudgingResult, TaskGraph};
use super::run::Party;
use super::vendor::HandoffPlan;
use crate::handoff::QualityDebt;
use crate::names::{CheckpointId, RoleName, VendorId, WorkerRef};
use crate::policy::{ShadowRecord, Verdict, Violation};
use crate::settings::approvals::{ApprovalMode, DecideWith};
use crate::settings::budget::BudgetAmount;
use crate::steps::judgment::ReviewVerdict;
use crate::workflow::{Workflow, WorkflowRef};
use crate::{BudgetDimension, BudgetStatus, CardId, DiffStat, Spend};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CheckpointEvent {
    /// In supervised mode it waits for you and rings the bell.
    Opened(Box<CheckpointOpened>),
    /// Hard limits first, then rules or the approver model.
    Checked(PolicyCheck),
    Decided(CheckpointDecided),
    /// It no longer applies, for example because the evidence changed. A new one may open.
    Withdrawn {
        id: CheckpointId,
        reason: String,
    },
    /// An auto-approval joined your spot-check queue.
    SpotCheckQueued {
        id: CheckpointId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointOpened {
    pub id: CheckpointId,
    pub kind: CheckpointKind,
    /// Such as "Merge this run into main?", the card's heading on screen.
    pub title: String,
    pub card: Option<CardId>,
    pub packet: CheckpointPacket,
    /// An answer must carry it back, so you approve exactly what you saw.
    pub evidence_sha256: String,
    pub mode: ApprovalMode,
    /// Who decides in auto mode.
    pub decide_with: DecideWith,
    /// In display order.
    pub options: Vec<AnswerOption>,
    pub free_text: FreeText,
    /// False while the policy is still deciding.
    pub needs_you: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    Goal,
    /// Plan, task graph, owners, test list, and budget, together.
    Plan,
    Merge,
    /// A card's agent stopped: pick another, take over, or wait.
    Handoff,
    /// Its cards are paused.
    VendorDown,
    /// A card used up its attempts.
    Escalation,
    /// A card or the run hit its budget and `budget.on_hit` is `ask`.
    Budget,
    /// An owner's change request that the decision rules send to you.
    ChangeRequest,
    GoalChange,
    /// A recipe candidate passed replay.
    RecipePromotion,
    TakeOverReturn,
    /// An owner asked you something directly.
    Question,
    /// The vendor a card was handed off from is back, and the card carries quality debt.
    OriginalReturned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckpointPacket {
    Goal(GoalPacket),
    Plan(Box<PlanPacket>),
    Merge(MergePacket),
    Handoff(HandoffPlan),
    VendorDown(VendorDownPacket),
    Escalation(EscalationPacket),
    Budget(BudgetPacket),
    ChangeRequest(ChangeRequestPacket),
    GoalChange(GoalChangePacket),
    /// Must be a JSON object without a `kind` key, since the packet's tag goes beside its
    /// fields; anything else fails to serialize.
    RecipePromotion(serde_json::Value),
    TakeOverReturn(TakeOverReturnPacket),
    Question(QuestionPacket),
    OriginalReturned(OriginalReturnedPacket),
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
    pub budget: BudgetPlan,
    pub new_dependencies: Vec<String>,
    /// Anything that blocks auto-approval, such as text addressed to judges.
    pub flags: Vec<String>,
    /// The workflow the run follows from here, with the lead's reason when the lead picked it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<PlanWorkflow>,
}

/// The run's workflow as the plan checkpoint shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanWorkflow {
    /// The template it came from, or `None` when the lead composed it.
    pub from: Option<WorkflowRef>,
    /// The lead's one line for the plan checkpoint, when the lead designed it.
    pub why: Option<String>,
    /// As validated, with its parameters filled in.
    pub workflow: Workflow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergePacket {
    /// The branch an approval lands on.
    pub target: Option<String>,
    /// The integration branch (crew) or card branch (solo).
    pub branch: String,
    pub rows: Vec<MergeRow>,
    pub suite_passed_on_latest_main: bool,
    pub gates: Vec<GateOutcome>,
    pub combined: DiffStat,
    /// What Argonauts committed itself, such as the goal record and decisions: the combined
    /// diff over the paths no agent touched. With the rows, it accounts for `combined`.
    #[serde(default, skip_serializing_if = "DiffStat::is_empty")]
    pub conductor: DiffStat,
    pub diff_path: PathBuf,
    /// The integration tree's hash. An approval records it.
    pub tree_sha: String,
    pub spent: Spend,
    pub limit: Option<BudgetAmount>,
    /// Set when an agent was stopped early, so the work may be partial.
    pub partial: bool,
    /// Your untracked files at paths the merge brings, which git won't overwrite. Set only on
    /// the card that asks what to do with them; a folder ends in `/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_the_way: Vec<String>,
    /// Your uncommitted edits to files the merge changes, which git won't merge over. Set only
    /// on the card that waits to merge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited: Vec<String>,
    /// Why git wouldn't move your checkout forward when nothing of yours was in the way, in
    /// its words. Set only on the card that waits to merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

impl MergePacket {
    /// The card waits to merge into your checkout: something of yours is in the way, or git
    /// refused, and only you can clear it.
    pub fn waits_on_you(&self) -> bool {
        !self.in_the_way.is_empty() || !self.edited.is_empty() || self.refused.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeRow {
    pub card: CardId,
    pub title: String,
    pub kind: CardKind,
    pub stat: DiffStat,
    pub owner: Option<WorkerRef>,
    pub gates_passed: bool,
    pub review: Option<ReviewVerdict>,
    /// With `merge_rule = "upgrade_review"`, the card waits for a strong enough review, or
    /// your `Merge`. The card then also offers `Wait` (w) and `UpgradeReview` (a).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub debt: Vec<QualityDebt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorDownPacket {
    pub vendor: VendorId,
    /// Such as "overload errors on 3 sessions in 2 min".
    pub evidence: String,
    /// When a test card will try it again.
    #[serde(with = "time::serde::rfc3339::option")]
    pub test_at: Option<OffsetDateTime>,
    /// Paused with their work committed.
    pub paused: Vec<PausedCard>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PausedCard {
    pub card: CardId,
    pub title: String,
    pub role: RoleName,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscalationPacket {
    pub card: CardId,
    pub title: String,
    pub state: CardState,
    /// One line each.
    pub attempts: Vec<String>,
    pub last_failure: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetPacket {
    /// `None` for the run.
    pub card: Option<CardId>,
    /// The meter at the stop.
    pub status: BudgetStatus,
    /// The limit that was hit.
    pub dimension: BudgetDimension,
    pub reserve_left: Option<BudgetAmount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeRequestPacket {
    pub from: Party,
    pub card: CardId,
    pub text: String,
    pub paths: Vec<String>,
    /// Why it's yours: irreversible, changes the goal, or moves a card into a protected path.
    pub why_you: String,
    /// Cards paused until it's decided.
    pub paused: Vec<CardId>,
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
    pub card: CardId,
    /// The owner that can continue.
    pub owner: WorkerRef,
    pub changed: DiffStat,
    /// The conductor's commit of your changes.
    pub commit: Option<String>,
}

/// Offers `UpgradeReview` (r), `HandBack` (o), `Redo` (u), `KeepReplacement` (y),
/// `TakeOver` (t), and `ShowDiff` (d). A choice made while a step runs applies after that step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OriginalReturnedPacket {
    pub card: CardId,
    pub title: String,
    pub original: WorkerRef,
    /// Who continued the card.
    pub replacement: WorkerRef,
    /// The checkpoint the diff starts at, `handoff-<n>`.
    pub since: u32,
    pub commits: u32,
    pub changed: DiffStat,
    /// Whether the gates passed on the latest commit, when they ran.
    pub tests_passed: Option<bool>,
    pub debt: Vec<QualityDebt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionPacket {
    pub from: WorkerRef,
    pub card: CardId,
    pub text: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerOption {
    pub key: char,
    pub label: String,
    pub action: AnswerAction,
    pub recommended: bool,
    /// Extra text on the same row, such as a candidate's quota and cost.
    pub detail: Option<String>,
}

/// The engine checks that an answer is one the card offered.
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
        /// An answer's label, a [`SettingScope`] name on the scope question, or a guided-setup
        /// step's value.
        ///
        /// [`SettingScope`]: crate::settings::SettingScope
        value: String,
    },
    /// Answer in your own words, from the card's Type my own answer line.
    Answer,
    Candidate {
        worker: WorkerRef,
    },
    /// List every valid agent, not only the fallback list (`o`).
    OtherCandidates,
    TakeOver,
    Wait,
    /// Vendor down: one agent for every paused card.
    OneForAll,
    /// Vendor down: choose per card.
    PerCard,
    /// The amount in the note, or the default step without one.
    MoreBudget,
    HandOff,
    Escalate,
    Retry,
    /// The note says how.
    Split,
    Cancel,
    /// The card goes to `Deferred` and the run goes on without it.
    Defer,
    /// Let the owner continue after a take-over; it's told what changed.
    LetContinue,
    /// Plan checkpoint: run it solo instead.
    SwitchToSolo,
    /// Handled by the UI; never sent.
    ShowDiff,
    /// The original reviews the work since the checkpoint, or while it's still out, a
    /// candidate at least as strong does. Then it fixes the work or sends it back, and the
    /// debt clears.
    UpgradeReview,
    /// The original takes the card back from its current state, told what changed.
    HandBack,
    /// The card resets to the checkpoint and the original redoes it. The replacement's work
    /// stays on a side branch.
    Redo,
    /// The replacement keeps the card, and the debt stays for the merge rule.
    KeepReplacement,
}

/// What a note added with Tab on an option, or the Type my own answer line, carries on this
/// card. Text typed at the prompt never answers a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreeText {
    /// The card takes no note.
    None,
    /// The card offers Type my own answer.
    Answer,
    /// A note on request changes or send back.
    Guidance,
    /// A note on approve with edits.
    Edits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyCheck {
    pub id: CheckpointId,
    /// Any failure means it waits for you.
    pub violations: Vec<Violation>,
    pub verdict: Option<Verdict>,
    pub by: Option<DecideWith>,
    pub approver: Option<WorkerRef>,
    pub reasons: Vec<String>,
    /// Recorded, not acted on.
    pub shadow: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointDecided {
    pub id: CheckpointId,
    pub action: AnswerAction,
    /// The note or own answer: an answer, edits, or guidance.
    pub text: Option<String>,
    pub by: Party,
    /// Must match the checkpoint's.
    pub evidence_sha256: String,
    /// In shadow mode, what your auto settings would have decided.
    pub shadow: Option<ShadowRecord>,
    /// For "request changes on some cards", the cards sent back. Empty means every card.
    #[serde(default)]
    pub cards: Vec<CardId>,
}
