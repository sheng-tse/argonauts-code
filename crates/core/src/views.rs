//! What the workflow view, the agent view, `/context`, `/rewind`, and the Learning pickers draw.
//! The controller builds these from the run's events and the learning crate's files, so the
//! views need no model call.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::events::session::SubagentUpdate;
use crate::names::{SessionKey, WorkerRef, WorkflowId};
use crate::workflow::StandardPhase;
use crate::{CardId, DiffStat, RunId, Spend};

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

/// One workflow in the Library tab of the workflow view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowFile {
    pub name: String,
    pub origin: WorkflowOrigin,
    /// `built in`, or the file it came from, with `~` for home.
    pub source: String,
    /// The workflow's own description, or why it doesn't load.
    pub description: String,
    /// The text as found, so showing it costs no round trip.
    pub text: String,
    /// Whether your file differs from the built-in of the same name. `None` when there's no
    /// file of yours or no built-in to compare with.
    #[serde(default)]
    pub edited: Option<bool>,
    /// The one the `workflow` setting names.
    #[serde(default)]
    pub in_use: bool,
}

/// Where a workflow comes from. A repo file wins over yours, which wins over the built-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOrigin {
    BuiltIn,
    /// In `~/.argonauts/workflows/`.
    Yours,
    /// In the repo's `.argonauts/workflows/`.
    Repo,
}

/// One handoff checkpoint of a card, for the `/rewind` picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRow {
    pub card: CardId,
    /// The `n` of `handoff-<n>`.
    pub n: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub commit: String,
    pub from: WorkerRef,
    /// Several for a race.
    pub to: Vec<WorkerRef>,
    pub reason: String,
    /// The card's branch was last reverted to this one.
    #[serde(default)]
    pub reverted_here: bool,
}

/// The four Learning pickers: `/records`, `/recipes`, `/skills`, and `/memory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningArea {
    Records,
    Recipes,
    Skills,
    Memory,
}

impl LearningArea {
    pub const ALL: [LearningArea; 4] = [
        LearningArea::Records,
        LearningArea::Recipes,
        LearningArea::Skills,
        LearningArea::Memory,
    ];

    /// The slash command, without the slash.
    pub fn command(self) -> &'static str {
        match self {
            LearningArea::Records => "records",
            LearningArea::Recipes => "recipes",
            LearningArea::Skills => "skills",
            LearningArea::Memory => "memory",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            LearningArea::Records => "Records",
            LearningArea::Recipes => "Recipes",
            LearningArea::Skills => "Skills",
            LearningArea::Memory => "Memory",
        }
    }

    /// The line under the title. It shares its row with the area's summary, so the two fit in
    /// 80 columns.
    pub fn description(self) -> &'static str {
        match self {
            LearningArea::Records => "Who is good at what, by kind of work",
            LearningArea::Recipes => "Recipes and how they evolve",
            LearningArea::Skills => "Exported and imported skills, and any drift",
            LearningArea::Memory => "Notes the crew keeps, and the conventions",
        }
    }

    /// The setting sections its Settings tab lists, as dotted prefixes or whole keys.
    pub fn settings(self) -> &'static [&'static str] {
        match self {
            LearningArea::Records => &["routing", "outcomes.share"],
            LearningArea::Recipes => &[
                "evolution",
                "approvals.recipe_promotion",
                "limits.evolution_usd_per_month",
                "prompts",
                "triage.use_recipes",
            ],
            LearningArea::Skills => &["skills"],
            LearningArea::Memory => &["memory"],
        }
    }
}

/// One Learning picker: its tabs of rows, each row's detail and actions, and the Settings tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningView {
    pub area: LearningArea,
    /// The area's own tabs; the picker adds Settings after them.
    pub tabs: Vec<LearningTab>,
    /// The tab it opens on, such as the one a `/records <agent>` names.
    #[serde(default)]
    pub tab: usize,
    /// The row it opens on, by id, when the command named one.
    #[serde(default)]
    pub focus: Option<String>,
    /// One line under the header, such as how much of the month's evolution budget is left.
    #[serde(default)]
    pub summary: String,
    /// The Settings tab, as `/config` rows show them.
    #[serde(default)]
    pub settings: Vec<crate::app::SettingRow>,
    /// Sent after an action, for a picker already open on this area.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningTab {
    pub title: String,
    pub columns: Vec<String>,
    pub rows: Vec<LearningRow>,
    /// What an empty tab says.
    pub empty: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningRow {
    /// Stable within its tab: a worker and task type, a recipe, a candidate, a skill, a note.
    pub id: String,
    /// One per column.
    pub cells: Vec<String>,
    /// The lines the detail pane shows for the selected row.
    #[serde(default)]
    pub detail: Vec<String>,
    #[serde(default)]
    pub actions: Vec<LearningAction>,
    /// It waits for you, as the inbox counts it.
    #[serde(default)]
    pub waiting: bool,
}

/// A key in the picker and what it asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearningAction {
    pub key: char,
    /// Such as `promote`.
    pub label: String,
    pub command: LearningCommand,
}

/// What a Learning picker asks the app to do. Promotions and anything that writes your
/// instruction files need you at the terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LearningCommand {
    /// Rebuilds the track records from the outcome log.
    Rebuild,
    /// Fixes a worker's first-pass rate on a task type at its current mean (`routing.pins`).
    Pin {
        worker: String,
        task_type: String,
    },
    /// Drops a worker's outcomes before today (`routing.resets`).
    Reset {
        worker: String,
    },
    Promote {
        candidate: String,
    },
    Reject {
        candidate: String,
    },
    Rollback {
        recipe: String,
    },
    /// Turns a promoted candidate down after all: a reversal in the outcome log, and the
    /// version before runs again.
    Close {
        candidate: String,
    },
    /// Proposes the recipe's skill export on a branch for you.
    Export {
        recipe: String,
    },
    /// Lets a checked task join the replay set, on the side the conductor's seed draws.
    AcceptEntry {
        id: String,
    },
    /// Turns a task down for the replay set, with its hidden tests.
    RejectEntry {
        id: String,
    },
    /// Commits a flagged note on its own branch for you to merge.
    AcceptNote {
        id: String,
    },
    DropNote {
        id: String,
    },
    /// Renders `AGENTS.md` and `CLAUDE.md` from the conventions.
    Sync,
    /// Opens a branch that waits for you as a pull request with `gh`, where a remote exists.
    OpenPullRequest {
        branch: String,
    },
}
