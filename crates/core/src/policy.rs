//! Approval and narrator interfaces. Approvals come only from you, your rules, or a judge
//! model you configured; the MCP server never gets [`crate::api::Approvals`].

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::broadcast;

use crate::events::RunEvent;
use crate::events::approval::{AnswerAction, ApprovalKind};
use crate::events::goal::GoalFile;
use crate::events::run::RunPhase;
use crate::events::task::TaskSpec;
use crate::future::BoxFuture;
use crate::names::{ApprovalId, ModelRef};
use crate::session_runner::SessionSpec;
use crate::settings::approvals::{ApprovalMode, DecideWith};
use crate::settings::fallback::FallbackMode;
use crate::settings::limit::LimitAmount;
use crate::settings::ui::NarratorMode;
use crate::{RunId, Spend, canonical_json, sha256_hex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approve,
    Reject,
    AskYou,
}

/// A failed hard limit. No mode or model overrides one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    pub limit: HardLimit,
    pub detail: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardLimit {
    /// A check failed or didn't run.
    ChecksFailed,
    TestDeleted,
    /// A skip or focus marker was added.
    SkipMarker,
    /// Test or build config changed.
    TestConfigChanged,
    /// Stops the task.
    Secret,
    ProtectedPath,
    /// A dependency not on the allowlist.
    Dependency,
    Lockfile,
    /// A task, run, day, or month limit ceiling.
    Ceiling,
    /// The run branch moved after the approval, so it no longer covers what would land.
    HeadMoved,
    SentBackTwice,
    AutoMergeCap,
    /// An irreversible outside action not on your allowlist.
    Irreversible,
    SelfApproval,
    /// A CLI session (any access) or a check without a working sandbox.
    NoSandbox,
    /// A build session without the approved goal's hash.
    NoGoalHash,
    TestPassesOnStubs,
    CriterionUntested,
    /// A test-tampering flag is open.
    TamperFlag,
    /// A plan or merge whose evidence names no commits, so an approval would cover nothing.
    NoSubject,
    /// Instruction files other agents read changed, so the merge waits for you.
    InstructionFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionPoint {
    pub run: RunId,
    pub approval: ApprovalId,
    pub kind: ApprovalKind,
    /// Auto preview records count per learned workflow.
    pub learned_workflow: Option<String>,
    /// The judge must not be one of these.
    pub authors: Vec<ModelRef>,
    /// Set from Shift+Tab; replaces the configured mode for this point.
    pub mode_override: Option<ApprovalMode>,
    /// The main agent, whose answers the judge checks. In autopilot, when nothing you can run
    /// comes from a provider none of the authors come from, the judge comes from another
    /// provider than this one's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_agent: Option<ModelRef>,
}

/// Rules read only `facts`. Agent outputs can add conditions but never remove them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidencePacket {
    pub kind: ApprovalKind,
    pub goal: Option<GoalFile>,
    pub facts: EvidenceFacts,
    /// Reviewer verdicts, judge scores, comparer tags, one line each.
    pub agent_outputs: Vec<String>,
    /// Shown as data: the plan text, the combined diff, your rules file.
    pub documents: Vec<EvidenceDocument>,
}

impl EvidencePacket {
    /// Approvals are bound to this hash of the canonical JSON.
    pub fn sha256(&self) -> String {
        // Plain data can't fail to serialize, and an empty object would match no approval.
        let value = serde_json::to_value(self).unwrap_or_default();
        sha256_hex(canonical_json(&value).as_bytes())
    }
}

/// Bigger documents are stored as files next to the packet, whose hash covers their SHA-256.
pub const EVIDENCE_INLINE_MAX: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceDocument {
    pub label: String,
    /// Empty when the document is stored as a file.
    pub text: String,
    #[serde(default)]
    pub path: Option<std::path::PathBuf>,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// The commits an approval covers. The engine refuses to land when the run branch's
/// head is no longer `head_commit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSubject {
    pub base_commit: String,
    pub head_commit: String,
    pub tree_sha: String,
}

/// `None` means the fact doesn't apply to this decision point.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EvidenceFacts {
    pub subject: Option<EvidenceSubject>,
    pub open_questions: Option<u32>,
    /// Largest gap between a judge's two orders.
    pub max_score_spread: Option<f64>,
    pub winner_margin: Option<f64>,
    /// Every counted score came from another provider than the plan's author.
    pub judges_other_provider: Option<bool>,
    /// Every dispute with a proposed experiment was settled by running it.
    pub disputes_settled: Option<bool>,
    /// Plans flagged for text addressed to judges.
    pub flagged_plans: Option<u32>,
    pub estimate_usd: Option<f64>,
    pub new_dependencies: Vec<String>,
    /// Paths agents changed in the combined diff against main, or tasks own in a plan. Hard
    /// limits check these.
    pub agent_paths: Vec<String>,
    /// Paths only Argonauts Code committed, under an `Argonauts-Code: <run>` trailer.
    /// Shown, not checked.
    pub conductor_paths: Vec<String>,
    /// The protected subset of `agent_paths`.
    pub protected_paths: Vec<String>,
    pub checks_passed: Option<bool>,
    pub reviews_approved: Option<bool>,
    /// Every task's standing approval came from a reviewer of another provider than the task's
    /// worker by the registry, or from one you chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by_other_provider: Option<bool>,
    /// The tasks whose approval came only from reviewers code picked from the worker's own
    /// provider, each with who gave the approval that stands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_within_provider: Vec<WithinProvider>,
    /// The tasks that came to the merge unreviewed, since nobody but their builder, or their
    /// builder's provider, may review them: why, for each, naming the task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreviewed: Vec<String>,
    /// Every acceptance test fails on the stubs and passes on the run branch.
    pub acceptance_tests_valid: Option<bool>,
    pub untested_criteria: Vec<String>,
    /// Share of changed lines covered by tests.
    pub changed_line_coverage: Option<f64>,
    pub tamper_flags: Option<u32>,
    pub combined_diff_lines: Option<u32>,
    pub suite_on_latest_main: Option<bool>,
    /// Tasks whose quality debt no strong enough model reviewed, by the labels the merge table
    /// gives them, from the merge rows under `merge_rule = "upgrade_review"`, and empty under
    /// `none`. `None` means not measured, so auto won't merge unless `allow_debt` is set.
    #[serde(default)]
    pub unreviewed_debt: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck: Option<StuckFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<SpendFacts>,
    /// A goal change only clarifies the goal and leaves its success criteria as they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria_unchanged: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PromotionFacts>,
    /// Outside actions this point would take that can't be undone, such as a publish or a
    /// deploy. Hard limits check them against `limits.irreversible_allowlist`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub irreversible_actions: Option<Vec<String>>,
    /// Spent on this run.
    pub spent: Spend,
    pub auto_merges_today: u32,
    /// Across every run.
    pub spent_today_usd: f64,
    pub send_backs: u32,
}

/// A task whose approval came only from a reviewer code picked from its worker's own provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithinProvider {
    /// As the merge table names it.
    pub task: String,
    /// Who gave the approval that stands, as people read it: `Sonnet 5 on Claude Code`.
    pub reviewer: String,
    /// It reviewed in the place of a reviewer that was out.
    pub fallback: bool,
}

/// What auto needs to grant a task that needs you one more try. The engine fills in what it
/// knows, and a fact it doesn't know asks you.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StuckFacts {
    /// The task's role's fallback mode.
    pub fallback_mode: FallbackMode,
    /// Model providers that have worked on the task.
    pub providers_tried: Vec<String>,
    /// The provider of the fallback the next try would go to. `None` when no fallback is left.
    pub next_provider: Option<String>,
    /// What that fallback is called on screen, such as `Claude Code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_name: Option<String>,
    /// Extra tries granted so far, against `rules.stuck.max_extra_tries`.
    pub extra_tries: u32,
    /// Whether one more try fits in what's left of the reserve. `None` when that can't be
    /// told: the task has no limit of its own, or the two are counted in different units.
    #[serde(default)]
    pub fits_reserve: Option<bool>,
}

/// What auto needs to release the run's reserve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SpendFacts {
    /// The estimated cost of the tasks not yet done.
    pub remaining_tasks: LimitAmount,
    pub reserve_left: LimitAmount,
}

/// What auto needs to promote a learned workflow change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionFacts {
    /// As `rules.workflow_promotion.change_classes` names it, such as `checklist_item`.
    pub change_class: String,
    /// The learned workflow's last promotion or canary. `None` when it never had one.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_promoted: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PolicyOutcome {
    /// Supervised, auto preview, a hard limit failed, or auto left it to you.
    AskUser {
        violations: Vec<Violation>,
        auto_preview: Option<AutoPreviewRecord>,
    },
    Decided {
        verdict: Verdict,
        by: DecideWith,
        judge: Option<ModelRef>,
        reasons: Vec<String>,
        /// Whether this approval joined your spot-check queue.
        spot_check: bool,
    },
}

/// What auto would have decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoPreviewRecord {
    pub would: Verdict,
    pub by: DecideWith,
    pub reasons: Vec<String>,
}

/// Your decision, reported back so auto preview records can be scored.
#[derive(Debug, Clone, PartialEq)]
pub struct UserDecision {
    pub point: DecisionPoint,
    pub action: AnswerAction,
    pub edited: bool,
    pub auto_preview: Option<AutoPreviewRecord>,
}

pub trait ApprovalPolicy: Send + Sync {
    /// Hard limits first, then the mode and `decide_with`. Starts the judge in a fresh,
    /// read-only session when `decide_with` includes `model`.
    fn evaluate<'a>(
        &'a self,
        point: &'a DecisionPoint,
        evidence: &'a EvidencePacket,
    ) -> BoxFuture<'a, PolicyOutcome>;
    /// Feeds auto preview matching and earned autonomy.
    fn user_decided(&self, decision: &UserDecision);
    /// A wrong auto-approval drops that point back to auto preview.
    fn spot_checked(&self, point: &DecisionPoint, correct: bool);
    /// The mode `point` runs in, given the one your settings ask for. Your records may only
    /// lower it, such as back to auto preview after a wrong auto-approval.
    fn effective_mode(&self, point: &DecisionPoint, requested: ApprovalMode) -> ApprovalMode {
        let _ = point;
        requested
    }
    /// Landing what auto approved at `point` was refused, such as when the branch moved after
    /// the approval, so nothing merged: the auto-merge it took goes back to today's count.
    fn landing_refused(&self, point: &DecisionPoint) {
        let _ = point;
    }
    /// Every decision point back to supervised, now, for every run.
    fn auto_off(&self);
    fn hard_limits(&self) -> &dyn HardLimits;
    fn view(&self) -> ApprovalsView;
}

pub trait HardLimits: Send + Sync {
    fn check_point(&self, point: &DecisionPoint, evidence: &EvidencePacket) -> Vec<Violation>;
    /// The built-in checks on one task's diff: protected paths, secrets, test tampering,
    /// dependencies, lockfiles.
    fn check_task_diff(&self, task: &TaskSpec, unified_diff: &str) -> Vec<Violation>;
    /// As [`Self::check_task_diff`], with the task's checked-out tree, where a changed file's
    /// folders show whether it's part of a Claude Code plugin.
    fn check_task_tree_diff(
        &self,
        task: &TaskSpec,
        unified_diff: &str,
        tree: &std::path::Path,
    ) -> Vec<Violation> {
        let _ = tree;
        self.check_task_diff(task, unified_diff)
    }
    /// Refuses a build session without a sandbox or goal hash.
    fn check_session(&self, spec: &SessionSpec) -> Vec<Violation>;
}

/// What `/approvals` shows.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ApprovalsView {
    pub points: Vec<ApprovalRow>,
    pub spot_checks_waiting: u32,
    pub judge: Option<ModelRef>,
    /// What the settings check found in these settings that may not do what you meant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRow {
    pub kind: ApprovalKind,
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    pub matches_in_a_row: u32,
    pub misses: u32,
    /// Set when matches reach `suggest_auto_after_matches`.
    pub suggest_auto: bool,
}

/// Always shown as the narrator's, never as a record of the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NarratorLine {
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub run: Option<RunId>,
    pub kind: NarratorLineKind,
    pub text: String,
    pub model: ModelRef,
    pub spend: Spend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NarratorLineKind {
    Update,
    Summary,
    Answer {
        question: String,
    },
    /// About the narrator itself, such as "spend cap reached".
    Notice,
    /// The line goes to scrollback, not only the live line.
    PhaseEnd {
        phase: RunPhase,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NarratorStatus {
    pub mode: NarratorMode,
    pub model: ModelRef,
    pub spent: Spend,
    /// As written in settings.
    pub cap: String,
    /// At the cap, `s` in `/status` asks before spending more.
    pub capped: bool,
    pub interval_s: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NarratorError {
    #[error("the narrator is off")]
    Off,
    #[error("the narrator reached its spend cap")]
    CapReached,
    #[error("no run is going")]
    NoRun,
    #[error("{0}")]
    Failed(String),
}

/// Read-only, no tools, sees event digests only. Its lines go to `narrator.jsonl` and the lane,
/// never into `events.jsonl`, so no agent can see them.
pub trait Narrator: Send + Sync {
    /// Cheap: builds a digest line in code.
    fn observe(&self, event: &RunEvent);
    fn summary(&self) -> BoxFuture<'_, Result<NarratorLine, NarratorError>>;
    fn ask(&self, question: String) -> BoxFuture<'_, Result<NarratorLine, NarratorError>>;
    fn set_mode(&self, mode: NarratorMode);
    fn set_model(&self, model: ModelRef);
    fn status(&self) -> NarratorStatus;
    fn lines(&self) -> broadcast::Receiver<NarratorLine>;
}
