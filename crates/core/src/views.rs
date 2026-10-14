//! What the workflow view, the agent view, `/context`, `/rewind`, and the Learning pickers draw.
//! The controller builds these from the run's events and the learning crate's files, so the
//! views need no model call.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::agent_goal::AgentGoal;
use crate::events::context::ContextCategory;
use crate::events::goal::{Assumption, Criterion, CriterionState, GoalStatus, Try};
use crate::events::session::{
    CliAgent, CliAgentState, CliWorkflowRun, EndReason, SubagentStatus, SubagentUpdate,
};
use crate::names::{ModelRef, SessionKey, WorkflowId};
use crate::settings::workflows::RunStyle;
use crate::steps::StepName;
use crate::steps::plan::Severity;
use crate::workflow::{StandardPhase, StepRef};
use crate::{DiffStat, RunId, Spend, TaskId};

/// A run's workflow, or a nested one, with every phase it declares and their rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowView {
    pub run: RunId,
    pub id: WorkflowId,
    /// The run's own name and description, from the main agent's brief or intake, never a
    /// template's.
    pub name: String,
    pub description: String,
    /// `None` for a nested workflow, or an agent's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<RunStyle>,
    /// At most two lines under the header: the goal line in a goal run, then either what
    /// needs you or the main agent's latest note. A finished run's line says how it ended.
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
    /// What kind of workflow it is, read after its name: such as `orchestrator workflow`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The names of the steps you require of every workflow, or of this request. A phase by
    /// one of these names carries a mark, so you can see it is yours.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    /// Each agent that works toward a goal, found by its session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goals: Vec<AgentGoal>,
    /// The CLI whose own workflow runs the steps, such as `claude-code`, when one session's
    /// CLI runs the whole run for the crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Where the workflow stands, said after its totals once it isn't simply running.
    #[serde(default, skip_serializing_if = "WorkflowState::is_running")]
    pub state: WorkflowState,
}

/// Where a workflow stands. The header says every state but running, as Claude Code's does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    #[default]
    Running,
    Paused,
    Done,
    Stopped,
    Failed,
}

impl WorkflowState {
    pub fn is_running(&self) -> bool {
        *self == Self::Running
    }

    /// The word after the header's totals, and none while it runs.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Self::Running => None,
            Self::Paused => Some("paused"),
            Self::Done => Some("done"),
            Self::Stopped => Some("stopped"),
            Self::Failed => Some("failed"),
        }
    }
}

/// How words Argonauts Code adds to a row or a line read: dim, or in the color of good news,
/// of something to look at, or of a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Plain,
    Good,
    Warn,
    Bad,
}

impl Tone {
    pub fn is_plain(&self) -> bool {
        *self == Self::Plain
    }
}

/// What Argonauts Code says after a row's counts, where Claude Code has its state words: a
/// verdict such as `approved`, why a fallback runs, or what a step did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowNote {
    pub text: String,
    #[serde(default, skip_serializing_if = "Tone::is_plain")]
    pub tone: Tone,
}

impl RowNote {
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusLine {
    pub kind: StatusKind,
    /// The words up to the first ` · ` are the headline, and the rest reads dim after it.
    pub text: String,
    /// How a run that ended went: good news draws `✔`, a failure `✘`.
    #[serde(default, skip_serializing_if = "Tone::is_plain")]
    pub tone: Tone,
    /// A wait the line ends with, which the screen counts down, as in `tried again in 4m48s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<TimeLeft>,
}

/// A wait a status line counts down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeLeft {
    /// The words before the time, such as `tried again in`.
    pub words: String,
    /// How long was left as the view was built.
    pub ms: u64,
}

impl StatusLine {
    pub fn new(kind: StatusKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            tone: Tone::Plain,
            left: None,
        }
    }

    pub fn toned(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Ends the line with `words` and the `ms` left, counted down on screen.
    pub fn counting(mut self, words: impl Into<String>, ms: u64) -> Self {
        self.left = Some(TimeLeft {
            words: words.into(),
            ms,
        });
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    /// `◎ Goal · 3 of 4 met · try 2 of up to 5 · not yet: …`
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
    /// In a goal run, the try this phase is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub try_number: Option<u32>,
    /// Agents done of agents in the phase, once it has any, so a phase with none is never
    /// drawn `0/0`. Commands don't count, as in Claude Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<PhaseCount>,
    pub rows: Vec<WorkflowRow>,
    /// One dim line under the rows on what the phase waits for, such as `build:due-flag is on
    /// round 2 with the findings · its review runs again after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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
    /// A step Argonauts Code runs itself, a check or the merge, such as `final check  npm test ·
    /// 15 of 16 pass`: drawn like an agent, with the command or `git` where a model would be.
    Command(CommandRow),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandRow {
    /// Such as `final check` or `merge onto main`.
    pub label: String,
    /// What runs, such as `npm test` or `git`.
    pub command: String,
    /// Such as `15 of 16 pass, as at the start`, once it's known.
    pub result: Option<String>,
    /// How the result reads: a conflict git found is something to look at, a broken test a
    /// failure.
    #[serde(default, skip_serializing_if = "Tone::is_plain")]
    pub tone: Tone,
    pub status: RowStatus,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRow {
    pub session: SessionKey,
    /// The main agent's label, such as `fix:slugify`, unique in the run.
    pub label: String,
    /// The real agent and model.
    pub worker: ModelRef,
    pub status: RowStatus,
    /// Input plus output tokens.
    pub tokens: u64,
    pub spent: Spend,
    pub elapsed_ms: u64,
    /// Children the agent reported, for `+3 subagents`.
    pub subagents: u32,
    pub context: Option<ContextUse>,
    /// Who it is the fallback for, drawn `↷`.
    pub fallback_for: Option<ModelRef>,
    /// A step of a CLI's own workflow that a role answered through Argonauts Code: the row is
    /// that role's session, and its note says what it decided.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub answered: bool,
    /// The step's earlier tries, oldest first: each one that failed, the agent this one took
    /// over from, and each earlier round of a review. The row is the step, and these fold
    /// beneath it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tries: Vec<StepTry>,
    /// After the tokens: `approved`, `sent it back · 2 findings`, `paused · work so far kept`.
    /// On a fallback's row it is why the first model stopped, such as `overloaded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<RowNote>,
    /// Which agent of a workflow its CLI runs itself this is. `session` is then the session
    /// running that workflow, which all its agents share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner: Option<InnerAgent>,
}

/// One agent of a workflow its CLI runs itself, such as Claude Code's own. It has no session
/// of ours, so the workflow and its number there are how it is asked for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InnerAgent {
    /// The workflow, by the id its view has.
    pub workflow: WorkflowId,
    /// From 1, as its CLI counts the workflow's agents.
    pub index: u32,
}

impl AgentRow {
    /// Whether `other` is this same agent. A session alone doesn't say, since the agents of
    /// a workflow their CLI runs share one.
    pub fn is(&self, other: &AgentRow) -> bool {
        self.session == other.session && self.inner == other.inner
    }
}

/// One earlier try at a step, folded under the step's row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepTry {
    pub session: SessionKey,
    pub worker: ModelRef,
    pub status: RowStatus,
    pub elapsed_ms: u64,
    /// Why its session ended. The view says it in the plain line the agent's own failed row
    /// uses, such as `Codex refused the answer format Argonauts Code asked for`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<EndReason>,
    /// The round it was, from 1, when it is a review or a sign-off that finished and sent the
    /// work back, so the step was asked again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedWorkflowRow {
    pub id: WorkflowId,
    pub name: String,
    pub status: RowStatus,
    pub agents_done: u32,
    pub agents_total: u32,
    /// Each distinct model once.
    pub workers: Vec<ModelRef>,
    pub elapsed_ms: u64,
    /// The agent whose own CLI started it, when one did. The row sits under that agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionKey>,
    /// Whose workflow it is when it isn't Argonauts Code's, such as `Claude Code's own`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
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
    /// Held where it was with its work so far kept, as when its CLI is down.
    Paused,
    /// Waiting for your answer, `◆`.
    NeedsYou,
    Done,
    Failed,
    /// Its condition didn't hold, or the main agent skipped it, `–`.
    Skipped,
}

/// Whether a session is one of a workflow's agents, a row in its view and a count in its panel
/// row: a step placed it in the workflow, and it isn't the main agent's own ask beside the workflow
/// (its design at a phase end, or a goal try's plan), which shows as a `›` note instead.
pub fn is_workflow_agent(step: Option<StepName>, workflow_step: Option<&StepRef>) -> bool {
    workflow_step.is_some() && !matches!(step, Some(StepName::DesignWorkflow | StepName::PlanTry))
}

/// The id the views give a workflow an agent's own CLI runs, kept apart from Argonauts Code's own
/// `wf-NNN` so a request for it can't name one of ours.
/// The run the views name your conversation by, for a workflow its own CLI runs in a conversation
/// turn No run is ever named so: every run's id starts with `run-`.
pub fn conversation_run() -> RunId {
    RunId::new("conversation").unwrap_or_else(|_| RunId::generate())
}

pub fn cli_workflow_run_id(agent_id: &str) -> WorkflowId {
    let safe: String = agent_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(56)
        .collect();
    WorkflowId::parse(&format!("cli-{safe}")).unwrap_or_else(|| WorkflowId::numbered(0))
}

impl WorkflowView {
    /// A workflow an agent's own CLI runs, drawn like one of Argonauts Code's own with whatever
    /// phases and labels it reports. `host` is the session running it. Its agents have no
    /// session of ours, so each row holds the host's and says which agent it is. Once it
    /// ends, a phase that never ran leaves the list, as ours do.
    /// `now_ms`, in milliseconds since the epoch, times what's still running.
    pub fn of_agent(
        run: &RunId,
        host: &AgentRow,
        workflow: &CliWorkflowRun,
        now_ms: Option<u64>,
    ) -> Self {
        let ended = matches!(
            workflow.status,
            SubagentStatus::Done | SubagentStatus::Failed
        );
        let phases: Vec<PhaseRow> = workflow
            .phases
            .iter()
            .filter_map(|phase| {
                let rows: Vec<WorkflowRow> = workflow
                    .agents_in(phase.index)
                    .map(|agent| WorkflowRow::Agent(inner_row(host, workflow, agent, now_ms)))
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
                    status: cli_phase_status(&rows),
                    try_number: None,
                    count,
                    rows,
                    note: None,
                })
            })
            .collect();
        let done = workflow
            .agents
            .iter()
            .filter(|agent| agent.state == CliAgentState::Done)
            .count();
        Self {
            run: run.clone(),
            id: cli_workflow_run_id(&workflow.id),
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
            notes: Vec::new(),
            required: Vec::new(),
            goals: Vec::new(),
            worker: None,
            state: match workflow.status {
                SubagentStatus::Done => WorkflowState::Done,
                SubagentStatus::Failed => WorkflowState::Failed,
                _ => WorkflowState::Running,
            },
        }
    }

    /// Whether you require the step this phase is, whatever case its name is written in.
    pub fn requires(&self, phase: &PhaseRow) -> bool {
        let name = phase.name.trim();
        self.required
            .iter()
            .any(|required| required.trim().eq_ignore_ascii_case(name))
    }

    /// The goal `agent` works toward, when it has one. Agents that their CLI runs itself share
    /// its session, so the label tells their goals apart.
    pub fn goal_of(&self, agent: &AgentRow) -> Option<&AgentGoal> {
        self.goals.iter().find(|goal| {
            let label = goal.label.trim();
            goal.session == agent.session && (label.is_empty() || label == agent.label.trim())
        })
    }

    /// Tokens across the workflow: what was metered, and for a workflow whose CLI meters
    /// nothing through Argonauts Code, what its agents reported.
    pub fn tokens(&self) -> u64 {
        let metered = self.spent.total_tokens();
        if metered > 0 {
            return metered;
        }
        self.phases
            .iter()
            .flat_map(|phase| &phase.rows)
            .map(|row| match row {
                WorkflowRow::Agent(agent) => agent.tokens,
                _ => 0,
            })
            .sum()
    }
}

/// What an agent of a CLI's own workflow ended with when the workflow was over before it
/// was, in Claude Code's words for it.
const STOPPED_WITH_ITS_WORKFLOW: &str = "The workflow stopped before this agent finished.";

/// The row of one agent of a workflow `host`'s CLI runs itself: its own label, model, state
/// and counts as the CLI reports them, on the host's session.
fn inner_row(
    host: &AgentRow,
    workflow: &CliWorkflowRun,
    agent: &CliAgent,
    now_ms: Option<u64>,
) -> AgentRow {
    // An agent that names no model runs on its session's.
    let on = |model: &Option<String>| {
        ModelRef::new(
            host.worker.agent.clone(),
            model.clone().or_else(|| host.worker.model.clone()),
        )
    };
    let stopped = workflow.stopped_with_it(agent);
    // The model at work is the one that took over, once one did.
    let (at_work, first) = match &agent.fallback_model {
        Some(_) => (&agent.fallback_model, Some(&agent.model)),
        None => (&agent.model, None),
    };
    AgentRow {
        session: host.session.clone(),
        label: agent.label.clone(),
        worker: on(at_work),
        status: match agent.state {
            // Stopped part-way, as a session of ours that was stopped reads.
            _ if stopped => RowStatus::Skipped,
            CliAgentState::Queued => RowStatus::Queued,
            CliAgentState::Running | CliAgentState::Other => RowStatus::Running,
            CliAgentState::Done => RowStatus::Done,
            CliAgentState::Failed | CliAgentState::Blocked => RowStatus::Failed,
            CliAgentState::Skipped => RowStatus::Skipped,
        },
        tokens: agent.tokens,
        spent: Spend::default(),
        elapsed_ms: workflow.agent_elapsed_ms(agent, now_ms),
        subagents: 0,
        context: None,
        fallback_for: first.map(on),
        answered: false,
        tries: Vec::new(),
        // Claude Code's own words for an agent it refused to start and for one its workflow
        // stopped, where one that ran and failed reads `failed`.
        note: match agent.state {
            CliAgentState::Blocked => Some(RowNote::new("blocked", Tone::Plain)),
            _ if stopped => Some(RowNote::new("stopped", Tone::Plain)),
            _ => None,
        },
        inner: Some(InnerAgent {
            workflow: cli_workflow_run_id(&workflow.id),
            index: agent.index,
        }),
    }
}

fn cli_phase_status(rows: &[WorkflowRow]) -> RowStatus {
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
    } else if statuses.contains(&RowStatus::Failed) || statuses.contains(&RowStatus::Skipped) {
        // As Claude Code marks a phase: every agent is over, and one of them didn't finish.
        RowStatus::Failed
    } else {
        RowStatus::Done
    }
}

/// The goal around a run's workflow (Tab in the workflow view).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalView {
    pub run: RunId,
    pub objective: String,
    pub status: GoalStatus,
    pub criteria: Vec<CriterionRow>,
    pub tries: Vec<Try>,
    /// The most rounds, when you set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
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
    /// Exactly what Argonauts Code sent, in full. The view shows the first lines until expanded.
    pub prompt: String,
    /// The latest tool calls, oldest first.
    pub activity: Vec<ActivityLine>,
    /// Every tool call so far, for `last 3 of 195`.
    pub tool_calls: u32,
    /// Against the task's base. `None` for an agent with no worktree.
    pub diff: Option<DiffStat>,
    /// Only when the agent runs its own workflow.
    pub subagents: Option<SubagentsSection>,
    /// The report or final message, as Markdown.
    pub outcome: Option<String>,
    /// The goal it works toward, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<Box<AgentGoal>>,
    /// The plan it ended with, as it reported it: its tasks, and who does and who reviews
    /// each. The view draws it as a table above the outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<serde_json::Value>,
    /// What the agent view says after the model: the role, where it works, and the round it is on,
    /// such as `worker`, `worktree`, `round 2 (sent back)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
    /// The findings a review sent it back with, which it works on now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_fix: Option<Findings>,
    /// A reviewer's own verdict and findings, which are its outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewGiven>,
}

impl AgentView {
    /// One agent of a workflow `host`'s CLI runs itself, by its number there, from what the
    /// CLI reports of it: the start of its prompt and of its answer, how many tools it
    /// called and the last of them, the kind of agent it is, where it works, and which try
    /// it is. One that failed or was blocked ends with why. `None` when the workflow has no
    /// such agent.
    pub fn of_inner(
        run: &RunId,
        host: &AgentRow,
        workflow: &CliWorkflowRun,
        index: u32,
        now_ms: Option<u64>,
    ) -> Option<Self> {
        let agent = workflow.agents.iter().find(|agent| agent.index == index)?;
        let later_try = (agent.try_number > 1).then(|| format!("try {}", agent.try_number));
        let said = |fact: &Option<String>| {
            fact.as_deref()
                .map(str::trim)
                .filter(|fact| !fact.is_empty())
                .map(str::to_owned)
        };
        Some(Self {
            run: run.clone(),
            row: inner_row(host, workflow, agent, now_ms),
            prompt: agent.prompt.clone().unwrap_or_default(),
            // Whether the call answered isn't reported, so it has no result either way.
            activity: agent
                .last_tool
                .iter()
                .map(|tool| ActivityLine {
                    tool: tool.name.clone(),
                    summary: tool.summary.clone().unwrap_or_default(),
                    ok: None,
                })
                .collect(),
            tool_calls: agent.tool_calls,
            diff: None,
            subagents: None,
            outcome: match agent.state {
                _ if workflow.stopped_with_it(agent) => Some(STOPPED_WITH_ITS_WORKFLOW.to_owned()),
                CliAgentState::Failed | CliAgentState::Blocked => {
                    agent.error.clone().or_else(|| agent.result.clone())
                }
                // Nothing went wrong and nothing was answered: the view says it was skipped.
                CliAgentState::Skipped => None,
                _ => agent.result.clone(),
            },
            goal: None,
            plan: None,
            facts: (agent.state == CliAgentState::Blocked)
                .then(|| "blocked".to_owned())
                .into_iter()
                .chain(said(&agent.agent_type))
                .chain(said(&agent.isolation))
                .chain(later_try)
                .collect(),
            to_fix: None,
            review: None,
        })
    }
}

/// The findings of one review round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Findings {
    /// The reviewer's label, such as `review:due-flag`.
    pub from: String,
    /// From 1.
    pub round: u32,
    pub items: Vec<Finding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    /// The file and line it is about, such as `src/cli.js:42`. Empty when it names none.
    pub place: String,
    pub text: String,
}

/// What a reviewer decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewGiven {
    /// Such as `approved` or `sent it back`.
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Tone::is_plain")]
    pub tone: Tone,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityLine {
    pub tool: String,
    /// One line, sanitized.
    pub summary: String,
    /// `None` while it runs.
    pub ok: Option<bool>,
}

/// An agent's own internal workflow, whose structure the agent doesn't expose.
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
    /// `None` for conversation with the main agent.
    pub run: Option<RunId>,
    pub session: SessionKey,
    pub label: String,
    pub worker: ModelRef,
    /// In tokens.
    pub used: u64,
    pub window: Option<u64>,
    /// Where it rotates, from `sessions.rotate_at`.
    pub cap: Option<u64>,
    pub tool_calls: u32,
    pub spent: Spend,
    /// Fresh sessions it has rotated to so far.
    pub rotations: u32,
    /// `used` is worked out from the session's totals, because its CLI never says how large
    /// the last request was.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    /// Its CLI can say what fills it, as Claude Code can, whether or not it has yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub counts: bool,
    /// What fills it, by its CLI's own categories. Empty until the CLI has said, and for a
    /// CLI that says only how full.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<ContextCategory>,
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
    /// Whether your file is different from the built-in of the same name. `None` when there's no
    /// file of yours or no built-in to compare with.
    #[serde(default)]
    pub edited: Option<bool>,
    /// The one the `workflow` setting names.
    #[serde(default)]
    pub in_use: bool,
    /// Why `/<name>` doesn't run it, as when a command already has the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
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

/// One fallback checkpoint of a task, for the `/rewind` picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRow {
    pub task: TaskId,
    /// The `n` of `fallback-<n>`.
    pub n: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub commit: String,
    pub from: ModelRef,
    /// Several for a race.
    pub to: Vec<ModelRef>,
    pub reason: String,
    /// The task's branch was last rewound to this one.
    #[serde(default)]
    pub rewound_here: bool,
}

/// The four Learning pickers: `/records`, `/learned`, `/skills`, and `/memory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningArea {
    Records,
    LearnedWorkflows,
    Skills,
    Memory,
}

impl LearningArea {
    pub const ALL: [LearningArea; 4] = [
        LearningArea::Records,
        LearningArea::LearnedWorkflows,
        LearningArea::Skills,
        LearningArea::Memory,
    ];

    /// The slash command, without the slash.
    pub fn command(self) -> &'static str {
        match self {
            LearningArea::Records => "records",
            LearningArea::LearnedWorkflows => "learned",
            LearningArea::Skills => "skills",
            LearningArea::Memory => "memory",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            LearningArea::Records => "Records",
            LearningArea::LearnedWorkflows => "Learned workflows",
            LearningArea::Skills => "Skills",
            LearningArea::Memory => "Memory",
        }
    }

    /// The line under the title. It shares its row with the area's summary, so the two fit in
    /// 80 columns.
    pub fn description(self) -> &'static str {
        match self {
            LearningArea::Records => "Who is good at what, by kind of work",
            LearningArea::LearnedWorkflows => "Workflows learned from your runs",
            LearningArea::Skills => "Exported and imported skills, and any drift",
            LearningArea::Memory => "Notes the crew keeps, and the conventions",
        }
    }

    /// The setting sections its Settings tab lists, as dotted prefixes or whole keys.
    pub fn settings(self) -> &'static [&'static str] {
        match self {
            LearningArea::Records => &["routing", "outcomes.share"],
            LearningArea::LearnedWorkflows => &[
                "evolution",
                "approvals.workflow_promotion",
                "limits.evolution_usd_per_month",
                "prompts",
                "captain_or_crew.use_learned",
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
    /// One line under the header, such as how much of the month's evolution limit is left.
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
    /// Stable within its tab: a model and task type, a learned workflow, a candidate, a skill, a
    /// note.
    pub id: String,
    /// One per column.
    pub cells: Vec<String>,
    /// The lines the detail pane shows for the selected row.
    #[serde(default)]
    pub detail: Vec<String>,
    #[serde(default)]
    pub actions: Vec<LearningAction>,
    /// It waits for you, as `argonauts approvals` counts it.
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
    /// Fixes a model's first-pass rate on a task type at its current mean (`routing.pins`).
    Pin {
        worker: String,
        task_type: String,
    },
    /// Drops a model's outcomes before today (`routing.resets`).
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
        learned_workflow: String,
    },
    /// Turns a promoted candidate down after all: a reversal in the outcome log, and the
    /// version before runs again.
    Close {
        candidate: String,
    },
    /// Proposes the learned workflow's skill export on a branch for you.
    Export {
        learned_workflow: String,
    },
    /// Lets a checked task join the replay set, on the side Argonauts Code's seed draws.
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
