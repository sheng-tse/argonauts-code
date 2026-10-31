//! What the workflow view, the agent view, `/context`, `/rewind`, and the Learning pickers draw.
//! The controller builds these from the run's events and the learning crate's files, so the
//! views need no model call.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::app::agent_goal::AgentGoal;
use crate::base::names::{ModelRef, SessionKey, WorkflowId};
use crate::runs::events::context::ContextCategory;
use crate::runs::events::goal::{Assumption, Criterion, CriterionState, GoalStatus, Try};
use crate::runs::events::session::{
    CHILD_GROUP, CliAgent, CliAgentState, CliWorkflowRun, EndReason, SubagentStatus, SubagentUpdate,
};
use crate::settings::workflows::RunStyle;
use crate::workflows::steps::StepName;
use crate::workflows::steps::plan::Severity;
use crate::workflows::workflow::{StandardPhase, Step, StepKind, StepRef, Workflow};
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

    /// Done, stopped or failed: nothing is left to stop or pause.
    pub fn has_ended(&self) -> bool {
        matches!(self, Self::Done | Self::Stopped | Self::Failed)
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

/// How words Argonauts Code adds to a row or a line read: dim, in the text's own color for an
/// outcome that is neither good nor bad news, such as the proposal a judge chose, or in the
/// color of good news, of something to look at, or of a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Plain,
    Text,
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
    /// Dim words after it, each after ` · `, such as `1 note` after `approved`, or `passing it
    /// on` while the agent that asked still has the answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub then: Vec<String>,
}

impl RowNote {
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
            then: Vec::new(),
        }
    }

    /// The same note with `words` after it, dim.
    pub fn then(mut self, words: impl Into<String>) -> Self {
        self.then.push(words.into());
        self
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
    /// The pull request the run opened, while it's followed: `◐ #57 · test failed · back to
    /// its builder, round 1 of 3`. The pull request and what came of it are the headline.
    PullRequest,
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
    /// A workflow inside counts its own agents.
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
    /// A workflow inside this one, started in this phase: a `▸` row that opens to its phases
    /// and their agents, or to its agents for a workflow that reports no phases.
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
    /// Why it runs in `fallback_for`'s place, as the agent's view says it, such as `Qwen3 Coder
    /// 30B can't hold this review`. Kept once the verdict takes the row's note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_why: Option<String>,
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
    /// A job its workflow can do without that no model could take. It never ran, so it has no
    /// tokens or time and no count counts it; `worker` is the model it was meant for, and its
    /// note says why it was left out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left_out: bool,
    /// An agent stopped once the run no longer needed its work, such as the commit-message
    /// writer when the run merged at once. Its note says why, in its model's place, and no
    /// count counts it, as neither done nor unfinished.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_needed: bool,
}

/// The label of the merger's session that writes a merge's commit message.
pub const COMMIT_MESSAGE: &str = "commit message";

/// What the commit-message writer's row says once the run merged without its message.
pub const MERGED_AT_ONCE: &str = "not needed, merged at once";

/// Whether a session working `step` is the merger's that writes a merge's commit message. Its
/// step says so, never its label, which a workflow's script may give any agent.
pub fn writes_the_message(step: Option<StepName>) -> bool {
    step == Some(StepName::CommitMessage)
}

/// Whether an agent working `step` and `stopped` before it finished was one the run then did
/// without: the commit-message writer, stopped once the run `merged`, since the merge lands
/// with the message code writes rather than wait for it.
pub fn not_needed(step: Option<StepName>, stopped: bool, merged: bool) -> bool {
    stopped && merged && writes_the_message(step)
}

/// Agents done and in all among `rows`, by the one rule every count of a workflow keeps: its
/// phases, its pane titles, its header and its row in the bottom list. A row is one agent,
/// whatever tries and rounds fold under it, and it is done once it finished, unless `again`
/// says a later round of its own is coming, as for one of Argonauts Code's reviewers that sent
/// the work back. A workflow inside counts its own agents, and a step Argonauts Code runs
/// itself, such as a check or the merge, none, as a job left out or an agent not needed.
pub fn agents_counted(rows: &[WorkflowRow], again: impl Fn(&AgentRow) -> bool) -> (u32, u32) {
    rows.iter().fold((0, 0), |(done, total), row| match row {
        // A job left out never ran, and is no agent of the workflow's; one stopped once it
        // wasn't needed is neither done nor unfinished.
        WorkflowRow::Agent(agent) if agent.left_out || agent.not_needed => (done, total),
        WorkflowRow::Agent(agent) => {
            let finished = agent.status == RowStatus::Done && !again(agent);
            (done + u32::from(finished), total + 1)
        }
        WorkflowRow::Workflow(nested) => (done + nested.agents_done, total + nested.agents_total),
        WorkflowRow::Command(_) => (done, total),
    })
}

/// The step's label inside a later round's, such as `review:per-key` for `review:per-key-2`,
/// which the run made unique, or for `review:per-key · round 2`, named for its round.
pub fn round_stem(label: &str) -> Option<&str> {
    fn numbered<'a>((stem, n): (&'a str, &str)) -> Option<&'a str> {
        n.parse::<u32>().is_ok().then_some(stem)
    }
    label
        .rsplit_once(" · round ")
        .and_then(numbered)
        .or_else(|| label.rsplit_once('-').and_then(numbered))
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
    /// Whose workflow it is when it isn't Argonauts Code's: the CLI, such as `claude-code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Its phases, each a heading over its agents, as its row opens to them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseRow>,
    /// Its agents straight under its row, for a workflow that reports no phases of its own, as
    /// Claude Code reports a child workflow its script started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<WorkflowRow>,
}

/// How full a session's context is, in tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUse {
    pub used: u64,
    /// Its window, once its CLI or server has said. Screens show `used` against it, and the
    /// count alone while it isn't known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// Where it rotates, from `sessions.rotate_at`, once that can be worked out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<u64>,
}

impl ContextUse {
    /// How full its window is, as a share of it.
    pub fn of_window(&self) -> Option<f64> {
        let window = self.window.filter(|window| *window > 0)?;
        Some(self.used as f64 / window as f64)
    }

    /// How close it is to rotating, as a share of where it rotates.
    pub fn of_cap(&self) -> Option<f64> {
        let cap = self.cap.filter(|cap| *cap > 0)?;
        Some(self.used as f64 / cap as f64)
    }
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

/// What opening a run's workflow says while it is still being written.
pub const BEING_WRITTEN: &str = "The workflow is being written; it opens when its phases are known";

/// Whether a step of `workflow` writes the phases it runs as it goes: a script, which the runner
/// or a CLI's own workflow carries out. Its declared phases then only hold a place for them.
pub fn writes_its_phases(workflow: &Workflow) -> bool {
    fn writes(steps: &[Step]) -> bool {
        steps
            .iter()
            .any(|step| step.kind == StepKind::Script || writes(&step.steps))
    }
    workflow.phases.iter().any(|phase| writes(&phase.steps))
}

/// Whether a run's workflow has its shape, so anything of it is drawn: its row, its view and
/// its line in the conversation. A workflow whose phases are fixed has it from its start. One
/// whose step writes them, `written`, has it at the first report that lists phases: the
/// script's, or the CLI's own workflow a task's session runs. A script that declares no phase
/// has it once its first agent shows. Until then only the working line moves.
pub fn shape_known<'a>(
    written: bool,
    reports: impl IntoIterator<Item = &'a CliWorkflowRun>,
) -> bool {
    !written
        || reports
            .into_iter()
            .any(|report| !report.phases.is_empty() || !report.agents.is_empty())
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
        let phases = cli_phases(&host.session, &host.worker, workflow, now_ms);
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

/// The phases of a workflow `session`'s CLI runs itself, on `worker`, each with its agents'
/// rows. A child workflow its script started, which the CLI reports as one more phase such as
/// `▸ date-cases`, is a row of the phase that started it, with its agents under it, and that
/// phase counts them and is at work while it runs. Once the workflow ends, a phase that never
/// ran leaves the list, as ours do. `now_ms` times what's still running.
pub fn cli_phases(
    session: &SessionKey,
    worker: &ModelRef,
    workflow: &CliWorkflowRun,
    now_ms: Option<u64>,
) -> Vec<PhaseRow> {
    let ended = workflow.ended();
    let rows_in = |index: u32| -> Vec<WorkflowRow> {
        workflow
            .agents_in(index)
            .map(|agent| WorkflowRow::Agent(inner_row(session, worker, workflow, agent, now_ms)))
            .collect()
    };
    let mut phases: Vec<(u32, PhaseRow)> = Vec::new();
    for phase in &workflow.phases {
        let rows = rows_in(phase.index);
        if ended && rows.is_empty() {
            continue;
        }
        let row = PhaseRow {
            name: crate::base::text::shown_name(&phase.title),
            detail: phase.detail.clone(),
            standard: StandardPhase::Do,
            status: cli_phase_status(&rows),
            try_number: None,
            count: workflow
                .count(phase.index)
                .and_then(|(done, total)| PhaseCount::of(done, total)),
            rows,
            note: None,
        };
        phases.push((phase.index, row));
    }
    children_inside(phases, workflow, Some(&worker.agent), now_ms)
}

/// `phases`, each by its index among `workflow`'s, with each child workflow its script
/// started, which the workflow reports as one more phase named `▸` and the child's name, made
/// a row of the phase that started it: the last whose agents began before the child's did.
/// That phase counts the child's agents and is at work while it runs. `whose` is the CLI whose
/// workflow it is, `None` for one of Argonauts Code's own.
pub fn children_inside(
    phases: Vec<(u32, PhaseRow)>,
    workflow: &CliWorkflowRun,
    whose: Option<&str>,
    now_ms: Option<u64>,
) -> Vec<PhaseRow> {
    let first_start = |index: u32| {
        workflow
            .agents_in(index)
            .filter_map(|agent| agent.started_at_ms)
            .min()
    };
    let is_child = |index: u32| {
        workflow
            .phases
            .iter()
            .any(|phase| phase.index == index && phase.child().is_some())
    };
    let (children, mut phases): (Vec<_>, Vec<_>) =
        phases.into_iter().partition(|(index, _)| is_child(*index));
    for (index, child) in children {
        let started = first_start(index);
        let at = phases
            .iter()
            .rposition(|(index, _)| match (first_start(*index), started) {
                (Some(phase), Some(child)) => phase <= child,
                (Some(_), None) => true,
                (None, _) => false,
            });
        let Some(at) = at.or_else(|| phases.len().checked_sub(1)) else {
            // A workflow that reports nothing but its child keeps it as a phase.
            phases.push((index, child));
            continue;
        };
        let nested = child_row(whose, workflow, child, started, now_ms);
        let holder = &mut phases[at].1;
        holder.rows.push(WorkflowRow::Workflow(nested));
        let (done, total) = agents_counted(&holder.rows, |_| false);
        holder.count = PhaseCount::of(done, total);
        holder.status = cli_phase_status(&holder.rows);
    }
    phases.into_iter().map(|(_, phase)| phase).collect()
}

/// The row of a child workflow, from the phase its workflow reported it as.
fn child_row(
    whose: Option<&str>,
    workflow: &CliWorkflowRun,
    phase: PhaseRow,
    started: Option<u64>,
    now_ms: Option<u64>,
) -> NestedWorkflowRow {
    let name = phase
        .name
        .strip_prefix(CHILD_GROUP)
        .unwrap_or(&phase.name)
        .trim()
        .to_owned();
    let (agents_done, agents_total) = agents_counted(&phase.rows, |_| false);
    let mut workers: Vec<ModelRef> = Vec::new();
    for row in &phase.rows {
        if let WorkflowRow::Agent(agent) = row
            && !workers.contains(&agent.worker)
        {
            workers.push(agent.worker.clone());
        }
    }
    // From its first agent's start to now while it runs, else to its last agent's end.
    let elapsed_ms = match (started, now_ms) {
        (Some(started), Some(now)) if phase.status == RowStatus::Running => {
            now.saturating_sub(started)
        }
        _ => workflow
            .agents
            .iter()
            .filter(|agent| agent.started_at_ms.is_some())
            .filter(|agent| {
                phase.rows.iter().any(|row| {
                    matches!(row, WorkflowRow::Agent(known)
                        if known.inner.as_ref().is_some_and(|inner| inner.index == agent.index))
                })
            })
            .filter_map(|agent| {
                let from = started?;
                Some(agent.started_at_ms?.saturating_sub(from) + agent.duration_ms)
            })
            .max()
            .unwrap_or(0),
    };
    NestedWorkflowRow {
        id: cli_workflow_run_id(&format!("{}-{name}", workflow.id)),
        name,
        status: phase.status,
        agents_done,
        agents_total,
        workers,
        elapsed_ms,
        parent: None,
        worker: whose.map(str::to_owned),
        phases: Vec::new(),
        rows: phase.rows,
    }
}

/// What an agent of a CLI's own workflow ended with when the workflow was over before it
/// was, in Claude Code's words for it.
const STOPPED_WITH_ITS_WORKFLOW: &str = "The workflow stopped before this agent finished.";

/// The row of one agent of a workflow `host`'s CLI runs itself: its own label, model, state
/// and counts as the CLI reports them, on the host's session.
fn inner_row(
    session: &SessionKey,
    worker: &ModelRef,
    workflow: &CliWorkflowRun,
    agent: &CliAgent,
    now_ms: Option<u64>,
) -> AgentRow {
    // An agent that names no model runs on its session's.
    let on = |model: &Option<String>| {
        ModelRef::new(
            worker.agent.clone(),
            model.clone().or_else(|| worker.model.clone()),
        )
    };
    let stopped = workflow.stopped_with_it(agent);
    // The model at work is the one that took over, once one did.
    let (at_work, first) = match &agent.fallback_model {
        Some(_) => (&agent.fallback_model, Some(&agent.model)),
        None => (&agent.model, None),
    };
    AgentRow {
        session: session.clone(),
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
        left_out: false,
        not_needed: false,
        fallback_why: None,
    }
}

fn cli_phase_status(rows: &[WorkflowRow]) -> RowStatus {
    // A child workflow at work keeps the phase that started it at work, whatever its own
    // agents have done.
    let statuses: Vec<RowStatus> = rows
        .iter()
        .filter_map(|row| match row {
            WorkflowRow::Agent(agent) => Some(agent.status),
            WorkflowRow::Workflow(nested) => Some(nested.status),
            WorkflowRow::Command(_) => None,
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
    /// The agent of a CLI's own workflow that handed this session the job and passed its
    /// answer back, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_for: Option<AskedFor>,
    /// For a reviewer, the earlier round's findings it was given and how each was answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earlier: Option<EarlierFindings>,
    /// Why its job came to its model, when it took another's place or its job was left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved: Option<Moved>,
    /// How a session that ran with no tools was given its work, such as `Reviewed with the
    /// change and the checks in its prompt, no tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub given: Option<String>,
    /// Where the prompt's own words start and what its later lines hold, for its preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_about: Option<PromptAbout>,
    /// What an orchestrator began from, when your words in a conversation started the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_from: Option<StartedFrom>,
}

/// What an orchestrator's session began from, for the first section of its agent view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartedFrom {
    /// A copy of your main session, with how many tokens of conversation it carried, when the
    /// CLI said.
    Fork {
        tokens: Option<u64>,
    },
    Brief(crate::runs::events::run::BriefFrom),
}

/// What a prompt holds, as its preview says it: past the frame every agent of its role gets,
/// the line its own words start on, from 0, and what its other lines hold, such as `your
/// request` and `the change`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAbout {
    pub first: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<String>,
}

/// Why an agent's job came to the model that does it, or to none, for its agent view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Moved {
    /// It does the job in `asked`'s place: `which` says what became of that one, such as
    /// `crashed twice as it started`, and `lines` what more is known, such as the error.
    FallbackFor {
        asked: ModelRef,
        which: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lines: Vec<String>,
    },
    /// No model could take the job, and the workflow goes on without it: `because` such as
    /// `agy is signed out`, and `lines` what that means for the run.
    LeftOut {
        because: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lines: Vec<String>,
    },
}

/// An earlier round of the review a reviewer gives, as it was given to it: the findings, who
/// worked on them, and the answer to each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EarlierFindings {
    /// The reviewer's label, such as `review:money`, and its round, from 1.
    pub from: String,
    pub round: u32,
    /// The agent that worked on them, such as `fix:review-findings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
    pub items: Vec<Finding>,
    /// Each finding's answer, in the order of `items`; empty where it had none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
    /// What that review said of the work as a whole, for a judge, who is given the latest
    /// review whatever it found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<EarlierVerdict>,
}

/// `approved` by GPT 6 Luna on Codex, and the review's one-line summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EarlierVerdict {
    /// Such as `approved` or `sent it back`.
    pub said: String,
    #[serde(default, skip_serializing_if = "Tone::is_plain")]
    pub tone: Tone,
    pub by: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// A hand-over by design: an agent of a CLI's own workflow, such as Claude Code's
/// `review:final`, gave another CLI the job and passed its answer back. The agent view tells it
/// once, as `Asked for by Claude Code · its agent review:final, on Haiku 5.5`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskedFor {
    /// The CLI whose workflow asked, such as `claude-code`.
    pub cli: String,
    /// Its agent's label and model, and that agent's own tokens and time.
    pub label: String,
    pub worker: ModelRef,
    pub tokens: u64,
    pub elapsed_ms: u64,
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
            row: inner_row(&host.session, &host.worker, workflow, agent, now_ms),
            prompt: agent.prompt.clone().unwrap_or_default(),
            // Whether the call answered isn't reported, so it has no result either way.
            activity: agent
                .last_tool
                .iter()
                .map(|tool| ActivityLine {
                    tool: tool.name.clone(),
                    summary: tool.summary.clone().unwrap_or_default(),
                    ok: None,
                    note: None,
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
            asked_for: None,
            earlier: None,
            moved: None,
            given: None,
            prompt_about: None,
            started_from: None,
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
    /// Dim words after the call, such as why no command ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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

/// Every live session's context, for `/context`, and the sessions of the run you follow that
/// ended, as each left its window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextView {
    pub rows: Vec<ContextRow>,
    /// Runs listed though they finished, with when each did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finished: Vec<RunFinishedAt>,
    /// The servers on this machine by endpoint id, such as `ollama`. A session runs on one
    /// when its CLI is one, or its model's route names one, as `ollama/qwen3-coder:30b` does
    /// on OpenCode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local: Vec<String>,
    /// What holds the main session's window to its size, where its server and CLI have said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_window: Option<WindowFigures>,
    /// What the main session holds at its start, for the tab before its CLI counts it and for
    /// a CLI that says only how full the session is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_start: Option<crate::runs::events::context::StartingCount>,
}

impl ContextView {
    /// The sessions, with nothing known of where they run.
    pub fn of(rows: Vec<ContextRow>) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }

    /// Whether `worker` runs on a server on this machine.
    pub fn runs_locally(&self, worker: &crate::base::names::ModelRef) -> bool {
        let routed = worker
            .model
            .as_deref()
            .and_then(|model| model.split_once('/'))
            .map(|(provider, _)| provider);
        self.local
            .iter()
            .any(|id| *id == worker.agent || Some(id.as_str()) == routed)
    }
}

/// When a run listed on the Context tab finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFinishedAt {
    pub run: RunId,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// What holds a session's window to its size, each figure where something has said it: the
/// most the model can take, the cap its server holds it to, and its CLI's own. The window is
/// the smallest of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowFigures {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerCap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<u64>,
}

/// A server's cap on a model's window, such as the 65,536 Ollama's `qwen3-coder:30b-64k` tag
/// sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCap {
    /// The server by name, such as `Ollama`.
    pub name: String,
    pub tokens: u64,
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
    /// Read from a resumed conversation's record, until a turn of it runs again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded: Option<RecordedContext>,
    /// How a run's session ended. `None` while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<SessionLeft>,
    /// The session whose question this one, a tie-breaker, answered: the tab counts it under
    /// that session's agent, not as an agent of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_for: Option<SessionKey>,
}

/// How a run's session ended, for its row once it has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionLeft {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub how: EndedHow,
    /// The model it stood in for, where it is a fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_for: Option<ModelRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "how")]
pub enum EndedHow {
    Done,
    /// In a few words, such as `no valid report in 2 tries`.
    Failed {
        why: String,
    },
    /// You stopped it, or the run paused.
    Stopped,
}

/// What a conversation's record says of its session, for a row read from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedContext {
    /// When its last answer ended.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub turns: u32,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(label: &str, status: RowStatus) -> WorkflowRow {
        WorkflowRow::Agent(AgentRow {
            session: SessionKey::numbered(1),
            label: label.to_owned(),
            worker: ModelRef::new("codex", None),
            status,
            tokens: 0,
            spent: Spend::default(),
            elapsed_ms: 0,
            subagents: 0,
            context: None,
            fallback_for: None,
            answered: false,
            tries: Vec::new(),
            note: None,
            inner: None,
            left_out: false,
            not_needed: false,
            fallback_why: None,
        })
    }

    /// A fallback's row keeps why it runs in another's place, and a row written before rows
    /// kept a reason reads without one.
    #[test]
    fn a_fallbacks_reason_is_kept_and_an_older_row_reads_without_one() {
        let WorkflowRow::Agent(mut row) = agent("review:due-flag", RowStatus::Running) else {
            unreachable!()
        };
        row.fallback_for = Some(ModelRef::new("ollama", Some("qwen3-coder:30b".to_owned())));
        row.fallback_why = Some("Qwen3 Coder 30B can't hold this review".to_owned());
        let written = serde_json::to_value(&row).unwrap();
        assert_eq!(
            written["fallback_why"],
            "Qwen3 Coder 30B can't hold this review"
        );
        assert_eq!(
            serde_json::from_value::<AgentRow>(written.clone()).unwrap(),
            row
        );
        let mut older = written;
        older.as_object_mut().unwrap().remove("fallback_why");
        let read: AgentRow = serde_json::from_value(older).unwrap();
        assert_eq!(read.fallback_why, None);
        row.fallback_why = None;
        assert!(
            serde_json::to_value(&row)
                .unwrap()
                .get("fallback_why")
                .is_none()
        );
    }

    /// One rule for every count: an agent is done once it finished, unless its next round is
    /// coming; a workflow inside counts its own agents; a check or the merge counts none, nor
    /// does an agent stopped once it wasn't needed.
    #[test]
    fn agents_count_by_one_rule() {
        let inside = WorkflowRow::Workflow(NestedWorkflowRow {
            id: WorkflowId::numbered(2),
            name: "date-cases".to_owned(),
            status: RowStatus::Running,
            agents_done: 1,
            agents_total: 3,
            workers: Vec::new(),
            elapsed_ms: 0,
            parent: None,
            worker: None,
            phases: Vec::new(),
            rows: Vec::new(),
        });
        let check = WorkflowRow::Command(CommandRow {
            label: "final check".to_owned(),
            command: "npm test".to_owned(),
            result: None,
            tone: Tone::Plain,
            status: RowStatus::Done,
            elapsed_ms: 0,
        });
        let skipped = match agent(COMMIT_MESSAGE, RowStatus::Skipped) {
            WorkflowRow::Agent(mut writer) => {
                writer.not_needed = true;
                WorkflowRow::Agent(writer)
            }
            other => other,
        };
        let rows = vec![
            agent("build:due-flag", RowStatus::Done),
            agent("review:due-flag", RowStatus::Done),
            agent("review:due-tests", RowStatus::Running),
            inside,
            check,
            skipped,
        ];
        assert_eq!(agents_counted(&rows, |_| false), (3, 6));
        let sent_back = |row: &AgentRow| row.label == "review:due-flag";
        assert_eq!(agents_counted(&rows, sent_back), (2, 6));
    }

    /// Claude Code reports a child its script started as one more phase, `▸ date-cases`. It is
    /// a row of the phase that started it instead, with its agents under it, and that phase
    /// counts them and is at work while it runs.
    #[test]
    fn a_child_workflow_is_a_row_of_the_phase_that_started_it() {
        let workflow: CliWorkflowRun = serde_json::from_value(serde_json::json!({
            "id": "wf_1", "name": "add-due-flag", "description": "",
            "phases": [
                { "index": 1, "title": "write" },
                { "index": 2, "title": "Check" },
                { "index": 3, "title": "▸ date-cases" },
            ],
            "agents": [
                { "index": 1, "label": "write:add", "phase": 1, "state": "done", "tokens": 0, "tool_calls": 0,
                  "started_at_ms": 1_000, "duration_ms": 41_000, "try_number": 1 },
                { "index": 2, "label": "check:dates", "phase": 2, "state": "done", "tokens": 0, "tool_calls": 0,
                  "started_at_ms": 50_000, "duration_ms": 31_000, "try_number": 1 },
                { "index": 3, "label": "case:leap-day", "phase": 3, "state": "done", "tokens": 0, "tool_calls": 0,
                  "started_at_ms": 60_000, "duration_ms": 14_000, "try_number": 1 },
                { "index": 4, "label": "case:bad-date", "phase": 3, "state": "running", "tokens": 0, "tool_calls": 0,
                  "started_at_ms": 70_000, "duration_ms": 0, "try_number": 1 },
            ],
            "status": "running", "tokens": 0, "tool_calls": 0, "duration_ms": 0,
        }))
        .unwrap();
        let phases = cli_phases(
            &SessionKey::numbered(1),
            &ModelRef::new("claude-code", None),
            &workflow,
            Some(82_000),
        );
        let names: Vec<&str> = phases.iter().map(|phase| phase.name.as_str()).collect();
        // A phase's name starts with a capital, whatever case the script wrote it in.
        assert_eq!(names, ["Write", "Check"]);
        let check = &phases[1];
        assert_eq!(check.status, RowStatus::Running);
        assert_eq!(check.count, PhaseCount::of(2, 3));
        let WorkflowRow::Workflow(child) = &check.rows[1] else {
            panic!("the child is a row of Check");
        };
        assert_eq!(child.name, "date-cases");
        assert_eq!(child.worker.as_deref(), Some("claude-code"));
        assert_eq!((child.agents_done, child.agents_total), (1, 2));
        assert_eq!(child.rows.len(), 2);
        assert_eq!(child.elapsed_ms, 22_000);
    }

    fn workflow(steps: serde_json::Value) -> Workflow {
        serde_json::from_value(serde_json::json!({
            "name": "any",
            "phases": [
                { "name": "Work", "standard": "do", "steps": steps },
                { "name": "Merge", "standard": "decide", "steps": [{ "kind": "merge" }] },
            ],
        }))
        .unwrap()
    }

    fn reported(phases: &[&str], agents: u32) -> CliWorkflowRun {
        CliWorkflowRun {
            id: "wf_1".to_owned(),
            call: None,
            name: "fix-money-rounding".to_owned(),
            description: String::new(),
            phases: phases
                .iter()
                .zip(1..)
                .map(|(title, index)| crate::runs::events::session::CliPhase {
                    index,
                    title: (*title).to_owned(),
                    detail: String::new(),
                })
                .collect(),
            agents: (1..=agents)
                .map(|index| CliAgent {
                    index,
                    ..CliAgent::default()
                })
                .collect(),
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        }
    }

    /// A workflow whose phases are fixed has its shape from its start. One a script writes has
    /// it at the first report that names its phases, or its first agent when it names none.
    #[test]
    fn a_workflow_has_its_shape_once_its_phases_are_known() {
        let fixed = workflow(serde_json::json!([{ "kind": "build" }]));
        let script = workflow(serde_json::json!([{ "kind": "script" }]));
        assert!(!writes_its_phases(&fixed));
        assert!(writes_its_phases(&script));

        assert!(shape_known(writes_its_phases(&fixed), []));
        let written = writes_its_phases(&script);
        assert!(!shape_known(written, []));
        let empty = reported(&[], 0);
        assert!(!shape_known(written, [&empty]));
        let phases = reported(&["Build", "Review", "Final"], 0);
        assert!(shape_known(written, [&empty, &phases]));
        assert!(shape_known(written, [&reported(&[], 1)]));
    }
}
