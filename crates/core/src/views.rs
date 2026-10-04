//! What the workflow view, the agent view, `/context`, `/rewind`, and the Learning pickers draw.
//! The controller builds these from the run's events and the learning crate's files, so the
//! views need no model call.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::events::goal::{Assumption, Attempt, Criterion, CriterionState, GoalStatus};
use crate::events::session::{
    SubagentStatus, SubagentUpdate, VendorAgent, VendorAgentState, VendorWorkflow,
};
use crate::names::{SessionKey, WorkerRef, WorkflowId};
use crate::settings::workflows::RunStyle;
use crate::steps::StepName;
use crate::workflow::{StandardPhase, StepRef};
use crate::{CardId, DiffStat, RunId, Spend};

/// A run's workflow, or a nested one, with every phase it declares and their rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowView {
    pub run: RunId,
    pub id: WorkflowId,
    /// The run's own name and description, from the lead's brief or intake, never a
    /// template's.
    pub name: String,
    pub description: String,
    /// `None` for a nested workflow, or a vendor's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<RunStyle>,
    /// At most two lines under the header: the goal line in a goal run, then either what
    /// needs you or the lead's latest note. A finished run's line says how it ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_lines: Vec<StatusLine>,
    /// Names from the run's workflow down to this one. Empty at the top.
    pub breadcrumb: Vec<String>,
    /// Counted at every level below this one too.
    pub agents_done: u32,
    pub agents_total: u32,
    pub elapsed_ms: u64,
    pub spent: Spend,
    pub phases: Vec<PhaseRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusLine {
    pub kind: StatusKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    /// `◎ Goal · 3 of 4 met · attempt 2 of up to 5 · not yet: …`
    Goal,
    /// `◆ Needs you · …`
    NeedsYou,
    /// The latest `›` note.
    Note,
    /// How the run ended.
    Ended,
}

/// One declared phase. A phase the workflow never declared is never a row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseRow {
    pub name: String,
    /// One line on who does what, shown as `Not started yet` with it until the phase starts.
    #[serde(default)]
    pub detail: String,
    pub standard: StandardPhase,
    pub status: RowStatus,
    /// In a goal run, the attempt this phase is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Agents done of agents in the phase, once it has any, so a phase with none is never
    /// drawn `0/0`. Commands don't count, as in Claude Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<PhaseCount>,
    pub rows: Vec<WorkflowRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseCount {
    /// A nested workflow counts as one.
    pub done: u32,
    pub total: u32,
}

impl PhaseCount {
    /// `None` when there's nothing to count.
    pub fn of(done: u32, total: u32) -> Option<Self> {
        (total > 0).then_some(Self { done, total })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "snake_case")]
pub enum WorkflowRow {
    Agent(AgentRow),
    /// A `◇` row that opens the nested workflow's own view.
    Workflow(NestedWorkflowRow),
    /// A gate or the merge, such as `tests:slugify  pytest · 6 passed`, so what the crew is
    /// doing includes its commands.
    Command(CommandRow),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandRow {
    /// Such as `tests:slugify` or `merge:main`.
    pub label: String,
    /// What runs, such as `pytest`.
    pub command: String,
    /// Such as `6 passed`, once it's known.
    pub result: Option<String>,
    pub status: RowStatus,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRow {
    pub session: SessionKey,
    /// The lead's label, such as `fix:slugify`, unique in the run.
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
    /// Who it stands in for after a handoff, drawn `↷`.
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
    /// Not started yet.
    Waiting,
    /// Started, and waiting its turn for a slot.
    Queued,
    Running,
    /// Waiting for your answer, `◆`.
    NeedsYou,
    Done,
    Failed,
    /// Its condition didn't hold, or the lead skipped it, `–`.
    Skipped,
}

/// Whether a session is one of a workflow's agents, a row in its view and a count in its panel
/// row: a step placed it in the workflow, and it isn't the lead's own ask beside the workflow
/// (its design at a phase end, or a goal attempt's plan), which shows as a `›` note instead.
pub fn is_workflow_agent(step: Option<StepName>, workflow_step: Option<&StepRef>) -> bool {
    workflow_step.is_some()
        && !matches!(step, Some(StepName::DesignWorkflow | StepName::PlanAttempt))
}

/// The id the views give a workflow an agent's own CLI runs, kept apart from Argonauts' own
/// `wf-NNN` so a request for it can't name one of ours.
pub fn vendor_workflow_id(vendor_id: &str) -> WorkflowId {
    let safe: String = vendor_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(56)
        .collect();
    WorkflowId::parse(&format!("vendor-{safe}")).unwrap_or_else(|| WorkflowId::numbered(0))
}

impl WorkflowView {
    /// A workflow an agent's own CLI runs, drawn like one of Argonauts' own with whatever
    /// phases and labels it reports. `host` is the session running it; its
    /// agents open that session, since the vendor's own have none of ours. Once it ends, a
    /// phase that never ran leaves the list, as ours do.
    /// `now_ms`, in milliseconds since the epoch, times what's still running.
    pub fn of_vendor(
        run: &RunId,
        host: &AgentRow,
        workflow: &VendorWorkflow,
        now_ms: Option<u64>,
    ) -> Self {
        let ended = matches!(
            workflow.status,
            SubagentStatus::Done | SubagentStatus::Failed
        );
        let row = |agent: &VendorAgent| AgentRow {
            session: host.session.clone(),
            label: agent.label.clone(),
            worker: WorkerRef::new(
                host.worker.worker.clone(),
                agent.model.clone().or_else(|| host.worker.model.clone()),
            ),
            status: match agent.state {
                VendorAgentState::Queued => RowStatus::Queued,
                VendorAgentState::Running | VendorAgentState::Other => RowStatus::Running,
                VendorAgentState::Done => RowStatus::Done,
                VendorAgentState::Failed => RowStatus::Failed,
            },
            tokens: agent.tokens,
            spent: Spend::default(),
            elapsed_ms: workflow.agent_elapsed_ms(agent, now_ms),
            subagents: 0,
            context: None,
            standing_in_for: None,
        };
        let phases: Vec<PhaseRow> = workflow
            .phases
            .iter()
            .filter_map(|phase| {
                let rows: Vec<WorkflowRow> = workflow
                    .agents_in(phase.index)
                    .map(|agent| WorkflowRow::Agent(row(agent)))
                    .collect();
                if ended && rows.is_empty() {
                    return None;
                }
                let count = workflow
                    .count(phase.index)
                    .and_then(|(done, total)| PhaseCount::of(done, total));
                Some(PhaseRow {
                    name: phase.title.clone(),
                    detail: phase.detail.clone(),
                    standard: StandardPhase::Do,
                    status: vendor_phase_status(&rows),
                    attempt: None,
                    count,
                    rows,
                })
            })
            .collect();
        let done = workflow
            .agents
            .iter()
            .filter(|agent| agent.state == VendorAgentState::Done)
            .count();
        Self {
            run: run.clone(),
            id: vendor_workflow_id(&workflow.id),
            name: workflow.name.clone(),
            description: workflow.description.clone(),
            style: None,
            status_lines: Vec::new(),
            breadcrumb: Vec::new(),
            agents_done: u32::try_from(done).unwrap_or(u32::MAX),
            agents_total: u32::try_from(workflow.agents.len()).unwrap_or(u32::MAX),
            elapsed_ms: workflow.elapsed_ms(now_ms),
            spent: Spend::default(),
            phases,
        }
    }
}

fn vendor_phase_status(rows: &[WorkflowRow]) -> RowStatus {
    let statuses: Vec<RowStatus> = rows
        .iter()
        .filter_map(|row| match row {
            WorkflowRow::Agent(agent) => Some(agent.status),
            _ => None,
        })
        .collect();
    if statuses.is_empty() {
        RowStatus::Waiting
    } else if statuses
        .iter()
        .any(|status| matches!(status, RowStatus::Running | RowStatus::Queued))
    {
        RowStatus::Running
    } else if statuses.contains(&RowStatus::Failed) {
        RowStatus::Failed
    } else {
        RowStatus::Done
    }
}

/// A goal-style run's goal (`/goal`, or Tab in the workflow view).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalView {
    pub run: RunId,
    pub objective: String,
    pub status: GoalStatus,
    pub criteria: Vec<CriterionRow>,
    pub attempts: Vec<Attempt>,
    pub max_attempts: u32,
    /// The last check's reason, once there's been one.
    pub last_reason: Option<String>,
    pub assumptions: Vec<Assumption>,
    pub spent: Spend,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionRow {
    pub criterion: Criterion,
    /// The last check's verdict. `None` before the first check.
    pub state: Option<CriterionState>,
    /// A passing test, or a file and line, when met.
    pub evidence: String,
    /// What's missing, when not.
    pub missing: String,
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
