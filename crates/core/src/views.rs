//! What the workflow view, the agent view, and `/context` draw. The controller builds these
//! from the run's events, so the views need no model call.

use serde::{Deserialize, Serialize};

use crate::events::session::SubagentUpdate;
use crate::names::{SessionKey, WorkerRef, WorkflowId};
use crate::workflow::StandardPhase;
use crate::{DiffStat, RunId, Spend};

/// A run's workflow, or a nested one, with every phase's rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowView {
    pub run: RunId,
    pub id: WorkflowId,
    pub name: String,
    pub description: String,
    /// Names from the run's workflow down to this one. Empty at the top.
    pub breadcrumb: Vec<String>,
    /// Counted at every level below this one too.
    pub agents_done: u32,
    pub agents_total: u32,
    pub elapsed_ms: u64,
    pub spent: Spend,
    pub phases: Vec<PhaseRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseRow {
    pub name: String,
    pub standard: StandardPhase,
    pub status: RowStatus,
    /// A nested workflow counts as one row.
    pub done: u32,
    pub total: u32,
    pub rows: Vec<WorkflowRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "snake_case")]
pub enum WorkflowRow {
    Agent(AgentRow),
    /// A `◇` row that opens the nested workflow's own view.
    Workflow(NestedWorkflowRow),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRow {
    pub session: SessionKey,
    /// Such as `owner · card-002`.
    pub label: String,
    /// The real vendor and model.
    pub worker: WorkerRef,
    pub status: RowStatus,
    /// Input plus output tokens.
    pub tokens: u64,
    pub spent: Spend,
    pub elapsed_ms: u64,
    /// Children the vendor reported, for `+3 subagents`.
    pub subagents: u32,
    pub context: Option<ContextUse>,
    /// Who it stands in for after a handoff, for `↺ for claude-code`.
    pub standing_in_for: Option<WorkerRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedWorkflowRow {
    pub id: WorkflowId,
    pub name: String,
    pub status: RowStatus,
    pub agents_done: u32,
    pub agents_total: u32,
    /// Each distinct worker once.
    pub workers: Vec<WorkerRef>,
    pub elapsed_ms: u64,
}

/// In tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUse {
    pub used: u64,
    /// Where it rotates, from `sessions.rotate_at`.
    pub cap: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowStatus {
    Waiting,
    Running,
    NeedsYou,
    Done,
    Failed,
}

/// One agent's session, opened from its row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentView {
    pub run: RunId,
    pub row: AgentRow,
    /// Exactly what Argonauts sent, in full. The view shows the first lines until expanded.
    pub prompt: String,
    /// The latest tool calls, oldest first.
    pub activity: Vec<ActivityLine>,
    /// Every tool call so far, for `last 3 of 195`.
    pub tool_calls: u32,
    /// Against the card's base. `None` for an agent with no worktree.
    pub diff: Option<DiffStat>,
    /// Only when the agent runs its own workflow.
    pub subagents: Option<SubagentsSection>,
    /// The report or final message, as Markdown.
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityLine {
    pub tool: String,
    /// One line, sanitized.
    pub summary: String,
    /// `None` while it runs.
    pub ok: Option<bool>,
}

/// An agent's own internal workflow, whose structure the vendor doesn't expose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentsSection {
    /// `Subagents`, or `Threads` for Codex.
    pub title: String,
    /// Such as "structure not exposed" or "steps not streamed".
    pub note: String,
    pub children: Vec<SubagentUpdate>,
}

/// Every live session's context, for `/context`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextView {
    pub rows: Vec<ContextRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextRow {
    /// `None` for chat with the lead.
    pub run: Option<RunId>,
    pub session: SessionKey,
    pub label: String,
    pub worker: WorkerRef,
    /// In tokens.
    pub used: u64,
    pub window: Option<u64>,
    /// Where it rotates, from `sessions.rotate_at`.
    pub cap: Option<u64>,
    pub tool_calls: u32,
    pub spent: Spend,
    /// Fresh sessions it has rotated to so far.
    pub rotations: u32,
}
