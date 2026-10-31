//! The workflow view, on Claude Code's workflow detail view: in the bottom region under the
//! conversation, the workflow's own name and description, its declared phases
//! numbered on the left, and the selected phase's agents on the right, each by its
//! `phase:target` label with its model, its stats, and its time. Every workflow draws here,
//! whoever runs it: the runner's, one of yours, or a CLI's own. A workflow inside another is a
//! row of the phase where it started, which opens and folds in place, and Enter on an agent
//! opens it in the same frame.
#[cfg(test)]
pub(crate) mod examples;
pub(crate) mod outline;
pub mod page;
pub(crate) mod peek;
pub(crate) mod rows;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod tries;

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::app::agent_goal::{AgentGoal, GoalKeeper};
use argonauts_core::app::views::{
    AgentRow, InnerAgent, NestedWorkflowRow, PhaseRow, RowStatus, StatusKind, StatusLine,
    WorkflowRow, WorkflowState, WorkflowView as WorkflowData,
};
use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::base::names::{SessionKey, WorkflowId};
use argonauts_core::runs::events::approval::ApprovalPacket;
use argonauts_core::runs::events::run::WaitReason;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::workflows::RunStyle;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::base::text::sanitize_line;
use crate::base::theme::{Role, Theme};
use crate::council::Faces;
use crate::council::work::task_header::{self, TaskHeader};
use crate::frame::state::AppState;

use super::super::base::draw::panes::{self, Footer, Marks, Side};
use super::super::frame::view::dock::{self, Heading};
use super::super::frame::view::list::{self, Nav};
use super::agent::AgentView;
use super::follow::{Follower, Shown};
use super::goal::GoalView;
use super::goal::keys as goal_keys;
use super::goal::panel as goal_panel;
use super::session::{SessionView, Tab};
use super::workflow::outline::{self as workflow_outline, AgentOutline, Item};
use super::workflow::peek::{self as workflow_peek, Peek};
use super::workflow::rows::{
    self as workflow_rows, Around, Cells, Models, PhaseColumns, RowColumns, row_label, row_status,
};
use super::workflow::tries as workflow_tries;
use crate::frame::view::{View, ViewCtx, ViewOutcome};

/// Rows the goal panel's box wants, borders included: the frame grows to give it these when
/// the usual frame is too short.
const GOAL_BOX_ROWS: u16 = 11;

/// The goal panel's box is this tall at the least, borders included, and at the most; the box
/// above it keeps at least [`AGENTS_BOX_ROWS`]. A frame too short for both shows the agents
/// alone.
/// The narrowest screen Claude Code's view draws both panes on, and the fewest cells its
/// right pane takes. Under either it draws one pane.
const TWO_PANES_MIN_COLUMNS: u16 = 64;
const RIGHT_PANE_MIN_CELLS: u16 = 20;

const GOAL_BOX_MIN: u16 = 5;

/// The agents pane's hint for → and ←, where its phase holds something to open or fold.
const OPEN_OR_FOLD: &str = "→  ←  open or fold";
const PEEK: &str = "space peek";
const ENTER_OPEN: &str = "enter open";
const GOAL_BOX_MAX: u16 = 14;
const AGENTS_BOX_ROWS: u16 = 5;

#[derive(Debug)]
pub struct WorkflowView {
    level: Level,
    /// An agent opened from the top level, drawn in this frame.
    agent: Option<AgentView>,
    /// A goal run's goal, which Tab shows in this frame.
    goal: Option<GoalView>,
    pending: Option<Pending>,
    /// What a second `x` stops.
    confirm: Option<Stop>,
    /// A restart `r` asked for, which a second `r` sends.
    restart: Option<super::Restart>,
    follow: Option<Follower>,
    /// The peek Space opened under the selected agent's row.
    peek: Option<Peek>,
    /// The last frame drew a mark of something at work, with motion full and glyphs that
    /// aren't plain, so its marks turn.
    turns: bool,
    /// The council task it was opened from, with the council's faces to name its seat by.
    council_task: Option<(TaskHeader, Faces)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stop {
    Workflow,
    Agent(SessionKey, String),
}

/// Which agents the right pane lists: `f` cycles them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    All,
    Running,
    Done,
    Failed,
}

impl Filter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Running,
            Self::Running => Self::Done,
            Self::Done => Self::Failed,
            Self::Failed => Self::All,
        }
    }

    fn keeps(self, row: &WorkflowRow) -> bool {
        let status = row_status(row);
        match self {
            Self::All => true,
            Self::Running => matches!(
                status,
                RowStatus::Running | RowStatus::Queued | RowStatus::NeedsYou
            ),
            Self::Done => status == RowStatus::Done,
            Self::Failed => status == RowStatus::Failed,
        }
    }

    fn word(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Running => Some("running"),
            Self::Done => Some("done"),
            Self::Failed => Some("failed"),
        }
    }
}

#[derive(Debug)]
struct Level {
    data: WorkflowData,
    phase: usize,
    row: usize,
    side: Side,
    filter: Filter,
    /// When this data was first drawn, so a running workflow's times keep counting.
    seen_at: Option<Instant>,
    /// The steps whose earlier tries are unfolded under their row, by the newest try's session.
    unfolded: Vec<SessionKey>,
    /// Workflows inside that you opened once they ended, and those you folded.
    opened: Vec<WorkflowId>,
    folded: Vec<WorkflowId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Agent {
        session: SessionKey,
        inner: Option<InnerAgent>,
    },
    Goal,
}

impl Pending {
    fn agent(row: &AgentRow) -> Self {
        Self::Agent {
            session: row.session.clone(),
            inner: row.inner.clone(),
        }
    }
}

impl Level {
    fn new(data: WorkflowData) -> Self {
        // Open on the phase that's going on, as Claude Code does, and once it's over, on the
        // last phase whose agents ran.
        let started = |phase: &PhaseRow| phase.status != RowStatus::Waiting;
        let phase = data
            .phases
            .iter()
            .position(|phase| matches!(phase.status, RowStatus::Running | RowStatus::NeedsYou))
            .or_else(|| {
                data.phases
                    .iter()
                    .rposition(|phase| started(phase) && !phase.rows.is_empty())
            })
            .or_else(|| data.phases.iter().rposition(started))
            .unwrap_or(0);
        Self {
            data,
            phase,
            row: 0,
            side: Side::Left,
            filter: Filter::All,
            seen_at: None,
            unfolded: Vec::new(),
            opened: Vec::new(),
            folded: Vec::new(),
        }
    }

    fn phase(&self) -> Option<&PhaseRow> {
        self.data.phases.get(self.phase)
    }

    /// The selected phase's rows that the filter keeps.
    fn rows(&self) -> Vec<&WorkflowRow> {
        self.phase().map_or_else(Vec::new, |phase| {
            phase
                .rows
                .iter()
                .filter(|row| self.filter.keeps(row))
                .collect()
        })
    }

    /// The selected phase's outline: its rows, and what each open workflow inside holds.
    fn outline(&self) -> Vec<Item<'_>> {
        workflow_outline::items(&self.rows(), self.data.worker.as_deref(), &|nested| {
            self.is_open(nested)
        })
    }

    /// The outline's lines the cursor rests on, every one but a heading.
    fn stops(&self) -> Vec<Item<'_>> {
        self.outline()
            .into_iter()
            .filter(|item| item.row().is_some())
            .collect()
    }

    fn row(&self) -> Option<&WorkflowRow> {
        self.stops().get(self.row).and_then(Item::row)
    }

    /// A workflow inside is open while it runs, and folds itself once it ends, unless you
    /// opened it or the cursor was in it. Folding it holds until you open it.
    fn is_open(&self, nested: &NestedWorkflowRow) -> bool {
        !self.folded.contains(&nested.id)
            && (self.opened.contains(&nested.id) || workflow_outline::at_work(nested))
    }

    /// The worker whose CLI runs the workflow the selected row is or is in, by its label: that
    /// CLI messages, stops and pauses its own agents, from the worker's row.
    fn cli_host(&self) -> Option<String> {
        let stops = self.stops();
        let Some(Item::Row { row, within, .. }) = stops.get(self.row) else {
            return None;
        };
        let outermost = match row {
            WorkflowRow::Workflow(nested) if within.is_empty() => &nested.id,
            _ => *within.first()?,
        };
        let rows = self.rows();
        let host = rows.iter().find_map(|row| match row {
            WorkflowRow::Workflow(nested) if nested.id == *outermost => nested.parent.as_ref(),
            _ => None,
        })?;
        rows.iter().find_map(|row| match row {
            WorkflowRow::Agent(agent) if agent.session == *host && agent.inner.is_none() => {
                Some(sanitize_line(&agent.label).trim().to_owned())
            }
            _ => None,
        })
    }

    /// Whether the open phase holds something → and ← open or fold: a workflow inside, or a
    /// step tried more than once.
    fn folds(&self) -> bool {
        self.outline().iter().any(|item| match item.row() {
            Some(WorkflowRow::Workflow(_)) => true,
            Some(WorkflowRow::Agent(agent)) => !agent.tries.is_empty(),
            _ => false,
        })
    }

    /// → or ← on the selected row: opens or folds the workflow it is, or for ← one of the
    /// rows a workflow holds, folds that workflow with the cursor on it. Says whether it did.
    fn open_or_fold(&mut self, open: bool) -> bool {
        let stops = self.stops();
        let Some(item) = stops.get(self.row) else {
            return false;
        };
        let (id, at) = match (item.row(), item) {
            (Some(WorkflowRow::Workflow(nested)), _) if self.is_open(nested) != open => {
                (nested.id.clone(), None)
            }
            (_, Item::Row { within, .. }) if !open && !within.is_empty() => {
                let id = (*within[within.len() - 1]).clone();
                let at = stops.iter().position(|stop| {
                    matches!(stop.row(), Some(WorkflowRow::Workflow(nested)) if nested.id == id)
                });
                (id, at)
            }
            _ => return false,
        };
        self.opened.retain(|known| *known != id);
        self.folded.retain(|known| *known != id);
        if open {
            self.opened.push(id);
        } else {
            self.folded.push(id);
        }
        if let Some(at) = at {
            self.row = at;
        }
        true
    }

    /// The selected step's newest session, when the step was tried more than once.
    fn tried_row(&self) -> Option<SessionKey> {
        match self.row() {
            Some(WorkflowRow::Agent(agent)) if !agent.tries.is_empty() => {
                Some(agent.session.clone())
            }
            _ => None,
        }
    }

    fn is_unfolded(&self, session: &SessionKey) -> bool {
        self.unfolded.contains(session)
    }

    /// Unfolds or folds the selected step's tries, and says whether it had any.
    fn fold_tries(&mut self, unfold: bool) -> bool {
        let Some(session) = self.tried_row() else {
            return false;
        };
        let open = self.is_unfolded(&session);
        if open == unfold {
            return false;
        }
        if unfold {
            self.unfolded.push(session);
        } else {
            self.unfolded.retain(|known| *known != session);
        }
        true
    }

    /// The cursor stays on the same phase, found by its name, when a phase is added above it
    /// or one leaves the list, and on the same row of it when rows above it come or go, as when
    /// a workflow inside folds itself. A workflow inside that holds the cursor stays open once
    /// it ends.
    fn refresh(&mut self, data: WorkflowData) {
        let was = self.phase().map(|phase| phase.name.clone());
        let at = self.phase;
        let on = self.row().map(RowKey::of);
        let holding: Vec<WorkflowId> = match self.stops().get(self.row) {
            Some(Item::Row { within, .. }) => within.iter().map(|id| (*id).clone()).collect(),
            _ => Vec::new(),
        };
        for id in holding {
            if !self.folded.contains(&id) && !self.opened.contains(&id) {
                self.opened.push(id);
            }
        }
        self.data = data;
        self.phase = was
            .clone()
            .and_then(|name| {
                let mut same = self
                    .data
                    .phases
                    .iter()
                    .enumerate()
                    .filter(|(_, phase)| phase.name == name)
                    .map(|(i, _)| i);
                let first = same.next()?;
                // Of two phases with one name, the nearer to where the cursor was.
                Some(same.fold(first, |best, i| {
                    if i.abs_diff(at) < best.abs_diff(at) {
                        i
                    } else {
                        best
                    }
                }))
            })
            .unwrap_or(at)
            .min(self.data.phases.len().saturating_sub(1));
        let same_phase = self.phase().map(|phase| &phase.name) == was.as_ref();
        let found = on.filter(|_| same_phase).and_then(|on| {
            self.stops()
                .iter()
                .position(|item| item.row().is_some_and(|row| RowKey::of(row) == on))
        });
        self.row = found.unwrap_or(self.row.min(self.stops().len().saturating_sub(1)));
        self.seen_at = None;
    }

    fn running(&self) -> bool {
        self.data
            .phases
            .iter()
            .any(|phase| matches!(phase.status, RowStatus::Running | RowStatus::NeedsYou))
    }

    /// The goal `row`'s agent works toward, when it has one.
    fn goal_of(&self, row: &WorkflowRow) -> Option<&AgentGoal> {
        match row {
            WorkflowRow::Agent(agent) => self.data.goal_of(agent),
            _ => None,
        }
    }

    /// The goal the panel under the agents shows: the selected agent's while the agents have
    /// the keys, and from the phases the first of the phase still running, else its first.
    fn shown_goal(&self) -> Option<&AgentGoal> {
        if self.side == Side::Right {
            return self.row().and_then(|row| self.goal_of(row));
        }
        let goals: Vec<&AgentGoal> = self
            .rows()
            .into_iter()
            .filter_map(|row| self.goal_of(row))
            .collect();
        goals
            .iter()
            .find(|goal| goal.is_running())
            .or(goals.first())
            .copied()
    }

    /// What the panel says when it has no goal to show.
    fn no_goal_words(&self) -> String {
        match (self.side, self.row()) {
            (Side::Right, Some(row)) => format!("{} works without a goal", row_label(row)),
            _ => "No agent in this phase works toward a goal".to_owned(),
        }
    }

    /// Time since this data was drawn first, while the workflow still runs.
    fn ticking(&self, now: Instant) -> u64 {
        match self.seen_at {
            Some(at) if self.running() => millis(now.saturating_duration_since(at)),
            _ => 0,
        }
    }

    /// Time since this data was drawn first, whether or not the workflow runs: a wait its
    /// status line counts down goes on while every agent is paused.
    fn shown_for(&self, now: Instant) -> u64 {
        self.seen_at
            .map_or(0, |at| millis(now.saturating_duration_since(at)))
    }
}

impl WorkflowView {
    pub fn new(data: WorkflowData) -> Self {
        Self {
            level: Level::new(data),
            agent: None,
            goal: None,
            pending: None,
            confirm: None,
            restart: None,
            follow: None,
            peek: None,
            turns: false,
            council_task: None,
        }
    }

    /// Opens on the workflow inside the run that `id` is: its phase, the workflow open, and the
    /// cursor on its row in the agents pane. The view as it is when no phase holds it.
    pub fn on_inside(mut self, id: &WorkflowId) -> Self {
        let level = &mut self.level;
        for (at, phase) in level.data.phases.iter().enumerate() {
            let rows: Vec<&WorkflowRow> = phase.rows.iter().collect();
            let items = workflow_outline::items(&rows, None, &|_| true);
            let Some(Item::Row { within, .. }) = items.iter().find(|item| {
                matches!(item.row(), Some(WorkflowRow::Workflow(nested)) if nested.id == *id)
            }) else {
                continue;
            };
            let opened: Vec<WorkflowId> = within
                .iter()
                .map(|outer| (*outer).clone())
                .chain(std::iter::once(id.clone()))
                .collect();
            level.phase = at;
            level.side = Side::Right;
            level.folded.retain(|known| !opened.contains(known));
            level.opened.extend(opened);
            level.row = level
                .stops()
                .iter()
                .position(|stop| {
                    matches!(stop.row(), Some(WorkflowRow::Workflow(nested)) if nested.id == *id)
                })
                .unwrap_or(0);
            break;
        }
        self
    }

    /// Keeps the view current with its run's events while it's on top.
    pub fn following(mut self, follower: Follower) -> Self {
        self.follow = Some(follower);
        self
    }

    /// What the next refresh asks for: the agent shown in this frame, else the top level.
    fn shown(&self) -> Shown {
        let run = self.run().clone();
        match &self.agent {
            Some(agent) => Shown::agent(run, agent.selected_row()),
            None => Shown::Workflow {
                run,
                id: self.top().data.id.clone(),
                root: !is_agent(&self.top().data.id),
            },
        }
    }

    pub fn current(&self) -> &WorkflowData {
        &self.top().data
    }

    /// The agent shown in this frame, once its answer arrived.
    pub fn agent(&self) -> Option<&AgentView> {
        self.agent.as_ref()
    }

    fn top(&self) -> &Level {
        &self.level
    }

    fn top_mut(&mut self) -> &mut Level {
        &mut self.level
    }

    fn run(&self) -> &RunId {
        &self.level.data.run
    }

    /// The name, the description after what kind of workflow it is, and the totals: the agents
    /// this workflow started, done of all, its time, which for the run's own workflow is the
    /// run's, and where it stands once it isn't simply running. Tokens and spend stay on the row
    /// in the bottom list.
    fn heading(&self, ctx: &ViewCtx<'_>) -> Heading {
        let level = self.top();
        let data = &level.data;
        let elapsed_ms = self
            .run_elapsed(ctx.state)
            .unwrap_or_else(|| data.elapsed_ms + level.ticking(ctx.now));
        // A workflow that has started no agent yet has none to count.
        let mut totals = if data.agents_total > 0 {
            format!(
                "{}/{} agents · {}",
                data.agents_done,
                data.agents_total,
                panes::elapsed(elapsed_ms)
            )
        } else {
            panes::elapsed(elapsed_ms)
        };
        // The screen can know the run ended or paused before the view's next answer says so.
        let state = match data.state {
            WorkflowState::Running => self.run_state(ctx.state),
            state => state,
        };
        if let Some(word) = state.word() {
            totals.push_str(&format!(" · {word}"));
        }
        let mut notes = data.notes.clone();
        notes.extend(goals_note(&data.goals));
        Heading {
            name: data.name.clone(),
            notes,
            description: data.description.clone(),
            totals,
            lines: self
                .council_task()
                .map(|(task, faces)| task.lines(faces, ctx.theme)),
        }
    }

    /// The council task this view was opened from, while it shows that task's workflow.
    fn council_task(&self) -> Option<(&TaskHeader, &Faces)> {
        let (task, faces) = self.council_task.as_ref()?;
        (self.top().data.id == task.workflow && self.agent.is_none()).then_some((task, faces))
    }

    /// Opened from a council's task: its head names the way, the task, its seat and the
    /// council, and Esc says it goes back to the council.
    pub fn for_council_task(mut self, task: TaskHeader, faces: Faces) -> Self {
        self.council_task = Some((task, faces));
        self
    }

    /// Where the run stands as the screen knows it, while the view is on the run's workflow.
    fn run_state(&self, state: &AppState) -> WorkflowState {
        use argonauts_core::runs::events::run::RunOutcome;
        if is_agent(&self.top().data.id) {
            return WorkflowState::Running;
        }
        match state.runs.get(self.run()).and_then(|run| run.finished) {
            Some(RunOutcome::Merged | RunOutcome::PullRequest | RunOutcome::BranchLeft) => {
                WorkflowState::Done
            }
            Some(RunOutcome::Stopped | RunOutcome::Abandoned) => WorkflowState::Stopped,
            Some(RunOutcome::Failed) => WorkflowState::Failed,
            None if paused(state, self.run()) => WorkflowState::Paused,
            None if stopped(state, self.run()) => WorkflowState::Stopped,
            None => WorkflowState::Running,
        }
    }

    /// The run's own time, while the view is on the run's workflow.
    fn run_elapsed(&self, state: &AppState) -> Option<u64> {
        if is_agent(&self.top().data.id) {
            return None;
        }
        let run = state.runs.get(self.run())?;
        let elapsed = run.totals(OffsetDateTime::now_utc()).elapsed?;
        Some(millis(elapsed))
    }

    /// Claude Code's keys for whichever pane has them, the same whatever is selected, so the
    /// line holds still as you move. A key that can't act now says why when pressed.
    /// `one_pane` is a screen too narrow for the agents beside the phases.
    fn footer(&self, ctx: &ViewCtx<'_>, one_pane: bool) -> Footer {
        let footer = self.footer_keys(ctx, one_pane);
        // From a council's task, Esc goes back to the council.
        match footer {
            Footer::Hints(mut parts) if self.council_task().is_some() => {
                for (part, _) in &mut parts {
                    if part == panes::ESC_BACK {
                        *part = task_header::BACK.to_owned();
                    }
                }
                Footer::Hints(parts)
            }
            footer => footer,
        }
    }

    fn footer_keys(&self, ctx: &ViewCtx<'_>, one_pane: bool) -> Footer {
        if let Some(stop) = &self.confirm {
            let ask = match stop {
                Stop::Workflow => "x again to stop the workflow".to_owned(),
                Stop::Agent(_, label) => format!("x again to stop {label}"),
            };
            return Footer::hints(&[(&ask, 0), ("esc to keep", 0)]);
        }
        if let Some(restart) = &self.restart {
            return Footer::hints(&[(&restart.ask(), 0), ("esc to keep", 0)]);
        }
        let level = self.top();
        // An agent's own workflow is its CLI's to stop and save, so only moving, opening and
        // leaving act on it here.
        if is_agent(&level.data.id) {
            return match level.side {
                Side::Left => Footer::hints(&[
                    ("↑↓  select", 0),
                    ("enter opens its agents", 1),
                    (panes::ESC_BACK, 0),
                ]),
                Side::Right => Footer::hints(&[
                    ("↑↓  select", 0),
                    (PEEK, 1),
                    (ENTER_OPEN, 1),
                    (panes::ESC_BACK, 0),
                ]),
            };
        }
        let pause = super::pause_hint(ctx.state, self.run());
        // A run that ended has nothing left to stop or pause, as in Claude Code's view.
        let ended = super::run_ended(ctx.state, self.run()) || self.level.data.state.has_ended();
        // A narrow screen gives up saving and pausing before what Enter opens, which no other
        // row says, and stopping last, as Claude Code's view always offers it. With one pane
        // Enter is the only way to a phase's agents, so there it says so in fewer words and
        // is the last to go.
        let enter = if one_pane {
            ("enter opens", 5)
        } else {
            ("enter opens its agents", 3)
        };
        match level.side {
            Side::Left if self.level.data.style == Some(RunStyle::Goal) => Footer::hints(&[
                ("↑↓  select", 0),
                enter,
                ("tab goal", 4),
                ("x stop goal", 4),
                (pause, 2),
                (panes::ESC_BACK, 0),
                ("s save", 1),
            ]),
            Side::Left if ended => Footer::hints(&[
                ("↑↓  select", 0),
                enter,
                (panes::ESC_BACK, 0),
                ("s save", 1),
            ]),
            Side::Left => Footer::hints(&[
                ("↑↓  select", 0),
                enter,
                ("x stop workflow", 4),
                (pause, 2),
                (panes::ESC_BACK, 0),
                ("s save", 1),
            ]),
            Side::Right => {
                let row = level.row();
                // A CLI's own workflow is that CLI's to message, stop and pause.
                if let Some(host) = level.cli_host() {
                    let elsewhere = format!("to message, stop or pause it, select {host}");
                    return Footer::hints(&[
                        ("↑↓  select", 0),
                        (PEEK, 3),
                        (ENTER_OPEN, 3),
                        (OPEN_OR_FOLD, 2),
                        (panes::ESC_BACK, 0),
                        (&elsewhere, 1),
                    ]);
                }
                let mut hints = vec![("↑↓  select", 0), (PEEK, 6), (ENTER_OPEN, 6)];
                if level.folds() {
                    hints.push((OPEN_OR_FOLD, 6));
                }
                // With vim keys on, `g` goes to the top of the list, as it does in every list.
                let has_goal = row.is_some_and(|row| level.goal_of(row).is_some());
                if has_goal && !list::vim(ctx.state) {
                    hints.push(("g its goal", 5));
                }
                // `p` holds the selected agent's goal where it can, and the run otherwise.
                let hold = self.goal_hold();
                if let Some(hold) = &hold {
                    hints.push((hold.hint, 5));
                }
                if self.editable_goal().is_some() {
                    hints.push((goal_keys::EDIT_HINT, 5));
                }
                // Claude Code's view always offers stopping, so a narrow screen gives up
                // messaging and restarting first.
                hints.push(("m message", 4));
                if !ended {
                    hints.extend([("x stop", 5), ("r restart", 4)]);
                }
                if hold.is_none() && !ended {
                    hints.push((pause, 3));
                }
                // `f` filters the list without a hint of its own, as the title says it does.
                hints.extend([(panes::ESC_BACK, 0), ("s save", 1)]);
                Footer::hints(&hints)
            }
        }
    }

    /// Enter or → on the right pane's row.
    fn open_row(&mut self) -> ViewOutcome {
        self.peek = None;
        let run = self.run().clone();
        let Some(row) = self.top().row() else {
            return ViewOutcome::Consumed;
        };
        match row {
            WorkflowRow::Agent(agent) => {
                let request = ViewRequest::agent(&run, agent);
                self.pending = Some(Pending::agent(agent));
                ViewOutcome::Request(request)
            }
            // A workflow inside opens in place.
            WorkflowRow::Workflow(_) => {
                self.top_mut().open_or_fold(true);
                ViewOutcome::Consumed
            }
            // A command has no session of its own to open.
            WorkflowRow::Command(_) => ViewOutcome::Consumed,
        }
    }

    /// The first time the view draws, while its run waits on you, the cursor goes to what the
    /// oldest question is about: the agent that asked it, or the first agent a CLI that is down
    /// holds. A question about the whole run leaves it on the phases.
    fn settle_on_what_needs_you(&mut self, state: &AppState) {
        let Some(run) = state.runs.get(self.run()) else {
            return;
        };
        let Some(open) = run.approvals.iter().find(|open| open.opened.needs_you) else {
            return;
        };
        let task_of = |agent: &AgentRow| {
            run.sessions
                .get(&agent.session)
                .and_then(|session| session.task.as_ref())
        };
        let level = self.top_mut();
        let first = |matches: &dyn Fn(&AgentRow) -> bool| {
            level
                .data
                .phases
                .iter()
                .enumerate()
                .find_map(|(at, phase)| {
                    phase.rows.iter().find_map(|row| match row {
                        WorkflowRow::Agent(agent) if matches(agent) => {
                            Some((at, agent.session.clone()))
                        }
                        _ => None,
                    })
                })
        };
        let found = match &open.opened.packet {
            // The agent whose task asked it, and only where none is found, the first agent on
            // the asking model whose task the screen doesn't know: an earlier phase may have run
            // the same model.
            ApprovalPacket::Question(question) => first(&|agent| {
                task_of(agent) == Some(&question.task)
            })
            .or_else(|| first(&|agent| task_of(agent).is_none() && agent.worker == question.from)),
            ApprovalPacket::AgentDown(down) => first(&|agent| {
                agent.status == RowStatus::Paused && agent.worker.agent == down.agent_id.0
            }),
            _ => None,
        };
        let Some((phase, session)) = found else {
            return;
        };
        level.phase = phase;
        level.side = Side::Right;
        let stop = level.stops().iter().position(|item| {
            matches!(item.row(), Some(WorkflowRow::Agent(agent)) if agent.session == session)
        });
        level.row = stop.unwrap_or(0);
    }

    /// Space on the right pane: a peek under the selected agent's row, which asks for the agent's
    /// view for the findings it gave, or closes the one open.
    fn peek_at_row(&mut self) -> ViewOutcome {
        if self.peek.take().is_some() {
            return ViewOutcome::Consumed;
        }
        let run = self.run().clone();
        match self.top().row() {
            Some(WorkflowRow::Agent(agent)) => {
                let request = ViewRequest::agent(&run, agent);
                self.peek = Some(Peek::of(agent));
                ViewOutcome::Request(request)
            }
            _ => ViewOutcome::Consumed,
        }
    }

    /// The selected agent, when it is one of a workflow its CLI runs itself, which has no
    /// session of ours for a key to act on.
    fn inner_agent(&self) -> Option<&AgentRow> {
        let level = self.top();
        match level.row().filter(|_| level.side == Side::Right) {
            Some(WorkflowRow::Agent(agent)) if agent.inner.is_some() => Some(agent),
            _ => None,
        }
    }

    /// The session `m` would message: the selected agent's, when the screen holds it and it
    /// can still take a message. Else why not, in plain words.
    fn message_target(&self, ctx: &ViewCtx<'_>) -> Result<SessionKey, String> {
        if let Some(agent) = self.inner_agent() {
            return Err(super::inner_why(ctx.state, agent));
        }
        let level = self.top();
        let agent = match level.row().filter(|_| level.side == Side::Right) {
            Some(WorkflowRow::Agent(agent)) if !is_agent(&level.data.id) => agent,
            _ => return Err("Select one of the run's own agents to message it".to_owned()),
        };
        if ctx.state.session(self.run(), &agent.session).is_none() {
            return Err(format!(
                "{} isn't on screen, so it can't take a message from here",
                sanitize_line(&agent.label).trim()
            ));
        }
        match super::steer_refusal(ctx.state, self.run(), &agent.session) {
            Some(why) => Err(why),
            None => Ok(agent.session.clone()),
        }
    }

    /// `m` on a running agent: the agent opened, with the message line ready.
    fn message(&self, ctx: &ViewCtx<'_>) -> ViewOutcome {
        match self.message_target(ctx) {
            Ok(session) => ViewOutcome::Open(Box::new(
                SessionView::new(self.run().clone(), session).messaging(),
            )),
            Err(why) => ViewOutcome::Notice(why),
        }
    }

    /// What `p` does to the selected agent's goal, when it has one that can be held.
    fn goal_hold(&self) -> Option<goal_keys::GoalKey> {
        let level = self.top();
        let row = level.row().filter(|_| level.side == Side::Right)?;
        goal_keys::hold(self.run(), level.goal_of(row)?)
    }

    /// The selected agent's goal, when its lines can be rewritten.
    fn editable_goal(&self) -> Option<&AgentGoal> {
        let level = self.top();
        let row = level.row().filter(|_| level.side == Side::Right)?;
        level.goal_of(row).filter(|goal| goal_keys::can_edit(goal))
    }

    /// `g` on an agent with a goal: the agent, opened on its Goal tab. `e` opens it there with
    /// the goal's lines ready to change.
    fn open_goal(&self, ctx: &ViewCtx<'_>, edit: bool) -> ViewOutcome {
        let level = self.top();
        let Some(goal) = level
            .row()
            .filter(|_| level.side == Side::Right)
            .and_then(|row| level.goal_of(row))
        else {
            return ViewOutcome::Notice("Select an agent that works toward a goal".to_owned());
        };
        if ctx.state.session(self.run(), &goal.session).is_none() {
            return ViewOutcome::Notice(format!(
                "{} isn't on screen, so its goal shows only here",
                sanitize_line(&goal.label).trim()
            ));
        }
        let mut view = SessionView::new(self.run().clone(), goal.session.clone()).on(Tab::Goal);
        if edit {
            view = view.editing_lines(goal);
        }
        if let Some(follow) = &self.follow {
            view = view.following(follow.another());
        }
        ViewOutcome::Open(Box::new(view))
    }

    fn move_selection(&mut self, nav: Nav) {
        self.peek = None;
        let level = self.top_mut();
        match level.side {
            Side::Left => {
                let count = level.data.phases.len();
                let next = step(level.phase, count, nav);
                if next != level.phase {
                    level.phase = next;
                    level.row = 0;
                }
            }
            Side::Right => {
                let count = level.stops().len();
                level.row = step(level.row, count, nav);
            }
        }
    }

    /// ← and Esc go back: from the agents to the phases, and from the phases out of the view.
    fn back(&mut self) -> ViewOutcome {
        self.confirm = None;
        if self.peek.take().is_some() {
            return ViewOutcome::Consumed;
        }
        if self.top().side == Side::Right {
            self.top_mut().side = Side::Left;
            return ViewOutcome::Consumed;
        }
        ViewOutcome::Close
    }

    /// `x` asks first, then stops what it names: the workflow from the phases, the selected
    /// agent from the agents.
    fn stop(&mut self, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if let Some(agent) = self.inner_agent() {
            return ViewOutcome::Notice(super::inner_why(ctx.state, agent));
        }
        if is_agent(&self.top().data.id) {
            return ViewOutcome::Notice(
                "This workflow is the agent's own; its CLI stops it with the agent".to_owned(),
            );
        }
        if finished(ctx.state, self.run()) {
            return ViewOutcome::Notice("This workflow has ended".to_owned());
        }
        if let Some(stop) = self.confirm.take() {
            return match stop {
                Stop::Workflow => ViewOutcome::Send(super::stop(self.run(), ctx.state)),
                Stop::Agent(session, _) => ViewOutcome::Send(UiCommand::StopAgent {
                    run: self.run().clone(),
                    session,
                }),
            };
        }
        let level = self.top();
        self.confirm = match (level.side, level.row()) {
            (Side::Left, _) => Some(Stop::Workflow),
            (Side::Right, Some(row @ WorkflowRow::Agent(agent)))
                if agent.status == RowStatus::Running =>
            {
                Some(Stop::Agent(agent.session.clone(), row_label(row)))
            }
            _ => return ViewOutcome::Notice("Select a running agent to stop it".to_owned()),
        };
        ViewOutcome::Consumed
    }

    fn agent_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> Option<ViewOutcome> {
        let agent = self.agent.as_mut()?;
        Some(match agent.handle_key(key, ctx) {
            ViewOutcome::Close => {
                self.agent = None;
                ViewOutcome::Consumed
            }
            ViewOutcome::Request(
                request @ (ViewRequest::Agent { .. } | ViewRequest::InnerAgent { .. }),
            ) => {
                self.pending = Some(Pending::agent(agent.selected_row()));
                ViewOutcome::Request(request)
            }
            ViewOutcome::Ignored if key.code == KeyCode::Char('s') => self.save(),
            other => other,
        })
    }

    /// `s` saves the workflow as it is now, so it runs again by name.
    fn save(&self) -> ViewOutcome {
        let level = self.top();
        if is_agent(&level.data.id) {
            return ViewOutcome::Notice(
                "This workflow is the agent's own; its CLI saves it".to_owned(),
            );
        }
        ViewOutcome::Send(UiCommand::SaveWorkflow {
            run: self.run().clone(),
            workflow: None,
        })
    }

    fn accept_agent(&mut self, view: &argonauts_core::app::views::AgentView) -> bool {
        if let Some(agent) = self.agent.as_mut()
            && agent.show(view.clone())
        {
            self.pending = None;
            return true;
        }
        if self.pending != Some(Pending::agent(&view.row)) {
            return false;
        }
        self.pending = None;
        let level = self.top();
        let agent = match level.phase() {
            Some(phase) => {
                let outline = AgentOutline::of(&level.outline(), &|nested| level.is_open(nested));
                AgentView::in_outline(view.clone(), phase, outline)
            }
            None => AgentView::new(view.clone()),
        };
        let mut agent = agent.lend(self.follow.as_ref().map(Follower::another));
        agent.run_state = Some(self.level.data.state);
        self.agent = Some(agent);
        true
    }

    fn accept_workflow(&mut self, view: &WorkflowData) -> bool {
        // Before the main agent picks the run's workflow, the view opens on Understand; the run's
        // own workflow takes its place once it starts.
        if self.agent.is_none()
            && self.pending.is_none()
            && !is_agent(&self.level.data.id)
            && !is_agent(&view.id)
            && view.id != self.level.data.id
        {
            self.level = Level::new(view.clone());
            self.pending = None;
            self.confirm = None;
            return true;
        }
        if self.level.data.id != view.id {
            return false;
        }
        self.level.refresh(view.clone());
        true
    }
}

/// Which row a row is, whatever moved around it: an agent by its session and its place in
/// the workflow its CLI runs, a workflow inside by its id, a step by its label.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowKey {
    Agent(SessionKey, Option<argonauts_core::app::views::InnerAgent>),
    Workflow(WorkflowId),
    Command(String),
}

impl RowKey {
    fn of(row: &WorkflowRow) -> Self {
        match row {
            WorkflowRow::Agent(agent) => Self::Agent(agent.session.clone(), agent.inner.clone()),
            WorkflowRow::Workflow(nested) => Self::Workflow(nested.id.clone()),
            WorkflowRow::Command(command) => Self::Command(command.label.clone()),
        }
    }
}

/// A workflow an agent's own CLI runs, which the views name apart from Argonauts Code's own.
fn is_agent(id: &WorkflowId) -> bool {
    id.as_str().starts_with("cli-")
}

/// Whether `run` has ended, as far as the screen knows it.
fn finished(state: &AppState, run: &RunId) -> bool {
    state
        .runs
        .get(run)
        .is_some_and(|run| run.finished.is_some())
}

/// Whether `run` is paused or pausing, so `p` resumes it.
/// Whether nothing could take a job the run needs, so it stopped fully until you carry it on.
fn stopped(state: &AppState, run: &RunId) -> bool {
    state
        .runs
        .get(run)
        .is_some_and(|run| matches!(run.waiting, Some(WaitReason::NoFallback { .. })))
}

fn paused(state: &AppState, run: &RunId) -> bool {
    state
        .runs
        .get(run)
        .is_some_and(|run| run.pausing || matches!(run.waiting, Some(WaitReason::Paused)))
}

fn step(at: usize, count: usize, nav: Nav) -> usize {
    let last = count.saturating_sub(1);
    match nav {
        Nav::By(delta) => at.saturating_add_signed(delta).min(last),
        Nav::Top => 0,
        Nav::Bottom => last,
    }
}

impl View for WorkflowView {
    fn docked(&self) -> bool {
        true
    }

    /// A question about the run reads beside what it is about: which tasks wait, and on whom.
    fn stays_over_question(&self) -> bool {
        true
    }

    /// The box fits what it holds, up to the usual frame, where it scrolls; a workflow with
    /// goals takes more when the usual frame is too short for the goal panel under the agents.
    /// It goes by the workflow and not by the row selected, so the frame never jumps as you
    /// move.
    fn dock_height(&mut self, screen: ratatui::layout::Size, _ctx: &ViewCtx<'_>) -> Option<u16> {
        if self.agent.is_some() || self.goal.is_some() {
            return None;
        }
        let level = self.top();
        let data = &level.data;
        // The rule, the name, the description, a blank row, and the footer, around the boxes.
        let around = 5 + status_lines(data, 0).len() as u16;
        let usual = dock::height(screen.height);
        let agents = around + agents_box_rows(level);
        if data.goals.is_empty() {
            return Some(usual.min(agents));
        }
        Some(usual.max(agents + GOAL_BOX_ROWS))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        if self.top().seen_at.is_none() && self.agent.is_none() && self.goal.is_none() {
            self.settle_on_what_needs_you(ctx.state);
        }
        self.top_mut().seen_at.get_or_insert(ctx.now);
        if let Some(follow) = &self.follow {
            follow.show(self.shown());
        }
        let heading = self.heading(ctx);
        if let Some(goal) = self.goal.as_mut() {
            goal.render(area, buf, ctx);
            return;
        }
        let status = status_lines(&self.top().data, self.top().shown_for(ctx.now));
        if let Some(agent) = self.agent.as_mut() {
            let models = Models::of(&self.level.data, ctx);
            agent.render_frame(area, buf, ctx, &heading, &status, &models);
            return;
        }
        let motion = !ctx.theme.reduced_motion && !ctx.theme.glyphs.is_ascii();
        self.turns = motion && self.top().data.phases.iter().any(workflow_rows::turns);
        let level = self.top();
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let phases = PhaseColumns::fit(&level.data, theme);
        let mut frame = dock::split(area, phases.width() as u16, status.len() as u16);
        // A screen too narrow for both panes gets one, as Claude Code's view does: the phases,
        // and once Enter goes to them, the open phase's agents across the whole box.
        let two_panes =
            area.width >= TWO_PANES_MIN_COLUMNS && frame.panes.right.width >= RIGHT_PANE_MIN_CELLS;
        if !two_panes {
            frame = dock::split(area, 0, status.len() as u16);
        }
        let footer = self.footer(ctx, !two_panes);
        let goal_box = if level.data.goals.is_empty() {
            None
        } else {
            let rows = goal_box_rows(level, frame.panes.body.height);
            dock::carve_below(&mut frame, rows, AGENTS_BOX_ROWS)
        };
        dock::draw_rule(buf, &frame, theme);
        dock::draw_heading(buf, &frame, &heading, theme);
        dock::draw_status(buf, &frame, &status, theme);

        let panes = frame.panes;
        let phases_in = if two_panes { panes.left } else { panes.right };
        let phase_lines = phases.lines(
            &level.data,
            (level.phase, level.side == Side::Left),
            usize::from(phases_in.width),
            !two_panes,
            level.ticking(ctx.now),
            theme,
        );
        let (first, last) = panes::window(
            level.phase,
            phase_lines.len(),
            usize::from(phases_in.height),
        );
        if two_panes || level.side == Side::Left {
            panes::put_lines(buf, phases_in, &phase_lines[first..last]);
        }
        if !two_panes && level.side == Side::Left {
            let mark = panes::scroll_mark(first, last, phase_lines.len(), theme);
            dock::draw_box(buf, &frame, ["", "Phases"], &mark, theme);
            if let Some(area) = goal_box {
                draw_goal_box(buf, area, level, theme);
            }
            dock::draw_footer(buf, &frame, footer, theme);
            return;
        }

        let shown = level.rows();
        let right_title = level.phase().map_or_else(String::new, |phase| {
            let name = sanitize_line(&phase.name).trim().to_owned();
            let size = workflow_rows::phase_size(phase);
            let mut title = match (level.filter.word(), phase.try_number) {
                (Some(filter), _) => format!("{name} · showing {} {filter}", shown.len()),
                (None, Some(try_number)) => format!("{name} · try {try_number} · {size}"),
                (None, None) => format!("{name} · {size}"),
            };
            if level.data.requires(phase) {
                title.push_str(&format!(" · {} a step you require", marks.required));
            }
            if let Some(note) = phase_goals_note(level, phase) {
                title.push_str(&format!(" · {note}"));
            }
            title
        });
        let height = usize::from(panes.right.height);
        let mut selected_line = level.row;
        let mut lines: Vec<Line<'static>> = match level.phase() {
            Some(open) if !shown.is_empty() => {
                let around = Around {
                    run: &level.data.run,
                    own_cli: level.data.worker.as_deref(),
                    ticking: level.ticking(ctx.now),
                    ctx,
                };
                let rows = RowColumns::fit(
                    workflow_rows::label_width(open),
                    usize::from(panes.right.width),
                    theme,
                );
                let mut lines = Vec::new();
                let mut stop = 0;
                for item in &level.outline() {
                    let (row, indent, own_cli) = match item {
                        Item::Heading {
                            phase,
                            number,
                            indent,
                        } => {
                            lines
                                .push(workflow_outline::heading(phase, *number, *indent, 1, theme));
                            continue;
                        }
                        Item::Row {
                            row,
                            indent,
                            own_cli,
                            ..
                        } => (*row, *indent, *own_cli),
                    };
                    let selected = level.side == Side::Right && stop == level.row;
                    if stop == level.row {
                        selected_line = lines.len();
                    }
                    stop += 1;
                    let around = Around { own_cli, ..around };
                    let mut cells = Cells::of(row, level.goal_of(row), &around);
                    if let WorkflowRow::Workflow(nested) = row
                        && level.is_open(nested)
                    {
                        cells = cells.opened(row, theme);
                    }
                    lines.push(rows.line(&cells, indent, selected, theme));
                    if let WorkflowRow::Agent(agent) = row
                        && level.is_unfolded(&agent.session)
                    {
                        lines.extend(workflow_tries::lines(agent, &ctx.state.names, theme));
                    }
                }
                lines
            }
            Some(_) if level.filter != Filter::All => {
                vec![dim_line("No agents match this filter", theme)]
            }
            Some(phase) => vec![dim_line(&empty_phase(phase, ctx.state, self.run()), theme)],
            None => vec![dim_line("No phases yet", theme)],
        };
        // Once a workflow inside is as deep as a run nests, the pane says so under its rows.
        // What a row's agent gave is the peek's, so nothing else stands there.
        let deepest = workflow_outline::deepest_note(&level.outline());
        if let Some(note) = deepest.filter(|_| level.filter == Filter::All) {
            lines.push(Line::default());
            lines.push(dim_line(
                &format!("  {}", sanitize_line(&note).trim()),
                theme,
            ));
        }
        // The peek opens under its row, and the list scrolls to show as much of it as fits.
        let names = &ctx.state.names;
        let peeked = match (&self.peek, level.row()) {
            (Some(peek), Some(WorkflowRow::Agent(agent)))
                if level.side == Side::Right && peek.is_of(agent) && !lines.is_empty() =>
            {
                Some((peek, agent))
            }
            _ => None,
        };
        let peek_rows = peeked.map_or(0, |(peek, agent)| peek.height(agent, names));
        let under = (selected_line + 1).min(lines.len());
        lines.splice(
            under..under,
            std::iter::repeat_n(Line::default(), peek_rows),
        );
        let (first, last) = panes::window(selected_line + peek_rows / 2, lines.len(), height);
        let mark = panes::scroll_mark(first, last, lines.len(), theme);
        dock::draw_box(buf, &frame, ["Phases", &right_title], &mark, theme);
        panes::put_lines(buf, panes.right, &lines[first..last]);
        if let Some((peek, agent)) = peeked
            && (first..last).contains(&under)
        {
            let shown = (peek_rows).min(last - under) as u16;
            let area = Rect::new(
                panes.right.x + workflow_peek::INDENT,
                panes.right.y + (under - first) as u16,
                panes.right.width.saturating_sub(workflow_peek::INDENT),
                shown,
            );
            peek.draw(buf, area, agent, names, theme);
        }
        if let Some(area) = goal_box {
            draw_goal_box(buf, area, level, theme);
        }
        dock::draw_footer(buf, &frame, footer, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if let Some(goal) = self.goal.as_mut() {
            return match goal.handle_key(key, ctx) {
                ViewOutcome::Close => {
                    self.goal = None;
                    ViewOutcome::Consumed
                }
                other => other,
            };
        }
        if let Some(outcome) = self.agent_key(key, ctx) {
            return outcome;
        }
        let vim = list::vim(ctx.state);
        let key = list::standard(key, vim, KeyCode::Left);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if self.confirm.is_some() && !(plain && key.code == KeyCode::Char('x')) {
            self.confirm = None;
            if key.code == KeyCode::Esc {
                return ViewOutcome::Consumed;
            }
        }
        // A second `r` restarts, Esc keeps the agent, and any other key acts as itself.
        if let Some(restart) = self.restart.take() {
            match key.code {
                KeyCode::Char('r') if plain => return ViewOutcome::Send(restart.command),
                KeyCode::Esc => return ViewOutcome::Consumed,
                _ => {}
            }
        }
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => {
                let nav = match key.code {
                    KeyCode::Up => Nav::By(-1),
                    KeyCode::Down => Nav::By(1),
                    KeyCode::Home => Nav::Top,
                    _ => Nav::Bottom,
                };
                self.move_selection(nav);
                ViewOutcome::Consumed
            }
            KeyCode::Left
                if self.top().side == Side::Right
                    && (self.top_mut().open_or_fold(false) || self.top_mut().fold_tries(false)) =>
            {
                ViewOutcome::Consumed
            }
            KeyCode::Right
                if self.top().side == Side::Right
                    && (self.top_mut().open_or_fold(true) || self.top_mut().fold_tries(true)) =>
            {
                ViewOutcome::Consumed
            }
            KeyCode::Left | KeyCode::Esc => self.back(),
            KeyCode::Right | KeyCode::Enter => {
                let level = self.top_mut();
                if level.side == Side::Left {
                    level.side = Side::Right;
                    level.row = 0;
                    return ViewOutcome::Consumed;
                }
                self.open_row()
            }
            KeyCode::Char('x') if plain => self.stop(ctx),
            KeyCode::Char('p') if plain => {
                if let Some(hold) = self.goal_hold() {
                    return ViewOutcome::Send(hold.command);
                }
                // Argonauts Code can't pause a workflow a CLI runs itself: that CLI does, from
                // its own screen.
                let own = self
                    .inner_agent()
                    .map(|agent| agent.worker.agent.clone())
                    .or_else(|| {
                        is_agent(&self.top().data.id)
                            .then(|| self.top().data.worker.clone())
                            .flatten()
                    });
                if let Some(cli) = own {
                    let cli = sanitize_line(&ctx.state.names.cli(&cli)).trim().to_owned();
                    return ViewOutcome::Notice(format!(
                        "{cli} pauses its own workflow from its own screen"
                    ));
                }
                super::pause_key(self.run(), ctx)
            }
            KeyCode::Char('f') if plain && self.top().side == Side::Right => {
                let level = self.top_mut();
                level.filter = level.filter.next();
                level.row = 0;
                ViewOutcome::Consumed
            }
            KeyCode::Char('s') if plain => self.save(),
            // Space peeks at the selected agent, and again closes the peek. On the phases it
            // closes the view, as in Claude Code.
            KeyCode::Char(' ') if plain && self.top().side == Side::Right => self.peek_at_row(),
            KeyCode::Char(' ') if plain => ViewOutcome::Close,
            KeyCode::Char('g') if plain && !self.top().data.goals.is_empty() => {
                self.open_goal(ctx, false)
            }
            KeyCode::Char('e') if plain && self.editable_goal().is_some() => {
                self.open_goal(ctx, true)
            }
            KeyCode::Char('m') if plain && self.top().side == Side::Right => self.message(ctx),
            KeyCode::Char('r') if plain && self.top().side == Side::Right => {
                let asked = match self.top().row() {
                    Some(WorkflowRow::Agent(agent))
                        if agent.inner.is_some() || !is_agent(&self.top().data.id) =>
                    {
                        super::restart(ctx.state, self.run(), agent)
                    }
                    _ => Err("Select one of the run's own agents to restart it".to_owned()),
                };
                match asked {
                    Ok(restart) => {
                        self.confirm = None;
                        self.restart = Some(restart);
                        ViewOutcome::Consumed
                    }
                    Err(why) => ViewOutcome::Notice(why),
                }
            }
            // A goal run's goal, as Claude Code has no Tab here.
            KeyCode::Tab if self.level.data.style == Some(RunStyle::Goal) => {
                self.pending = Some(Pending::Goal);
                ViewOutcome::Request(super::goal::request(self.run()))
            }
            _ => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        match data {
            ViewData::Workflow { view } if view.run == *self.run() => self.accept_workflow(view),
            ViewData::Agent { view }
                if view.run == *self.run()
                    && self.agent.is_none()
                    && self.peek.as_mut().is_some_and(|peek| peek.accept(view)) =>
            {
                true
            }
            ViewData::Agent { view } if view.run == *self.run() => self.accept_agent(view),
            ViewData::Goal { view } if view.run == *self.run() => match self.goal.as_mut() {
                Some(goal) => goal.accept(data),
                None if self.pending == Some(Pending::Goal) => {
                    self.pending = None;
                    self.goal = Some(GoalView::new((**view).clone()).from_workflow());
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    fn next_change(&self, now: Instant) -> Option<Instant> {
        if let Some(agent) = &self.agent {
            return agent.next_change(now);
        }
        let level = self.top();
        let elapsed = level.seen_at.filter(|_| level.running()).map(|_| {
            let shown = level.data.elapsed_ms + level.ticking(now);
            now + Duration::from_millis(1000 - shown % 1000)
        });
        // A wait the status line counts down loses a second as its last one runs out.
        let wait = level.seen_at.and_then(|_| {
            let left = said_line(&level.data)?.left.as_ref()?;
            let left = left.ms.checked_sub(level.shown_for(now))?;
            let into_second = match left % 1000 {
                _ if left == 0 => return None,
                0 => 1000,
                part => part,
            };
            Some(now + Duration::from_millis(into_second))
        });
        // The marks of what is at work take their next frame.
        let turn = level
            .seen_at
            .filter(|_| self.turns && level.running())
            .map(|_| {
                let period = millis(workflow_rows::TURN_PERIOD);
                now + Duration::from_millis(period - level.ticking(now) % period)
            });
        [elapsed, wait, turn].into_iter().flatten().min()
    }
}

/// `workers on Codex's own goal`, after the workflow's name: who works toward goals here and
/// who keeps them going. Nothing when no agent has a goal.
fn goals_note(goals: &[AgentGoal]) -> Option<String> {
    let first = goals.first()?;
    let same = |pick: fn(&AgentGoal) -> String| goals.iter().all(|goal| pick(goal) == pick(first));
    let who = |goal: &AgentGoal| goal.role.clone().unwrap_or_default();
    let role = sanitize_line(&who(first)).trim().to_owned();
    let who_words = if same(who) && !role.is_empty() {
        CrewRole::parse(&role).map_or(role, |crew| crew.plural().to_owned())
    } else {
        "agents".to_owned()
    };
    let keeper = |goal: &AgentGoal| goal.kept_by.words();
    Some(if same(keeper) {
        match &first.kept_by {
            GoalKeeper::Cli { .. } => format!(
                "{who_words} on {}",
                sanitize_line(&first.kept_by.words()).trim()
            ),
            GoalKeeper::Argonauts => format!("{who_words} on goals kept by Argonauts Code"),
        }
    } else {
        format!("{who_words} on goals")
    })
}

/// `each task worked as a goal`, on a phase whose every agent has one, or how many do.
fn phase_goals_note(level: &Level, phase: &PhaseRow) -> Option<String> {
    let agents: Vec<&WorkflowRow> = phase
        .rows
        .iter()
        .filter(|row| matches!(row, WorkflowRow::Agent(_)))
        .collect();
    let on_goals = agents
        .iter()
        .filter(|row| level.goal_of(row).is_some())
        .count();
    match on_goals {
        0 => None,
        n if n == agents.len() => Some("each task worked as a goal".to_owned()),
        n => Some(format!("{n} worked as a goal")),
    }
}

/// Rows for the goal panel's box when the two boxes share `body` rows. The agents' box takes
/// what its longest list needs, so the split holds still as the selection moves, and the goal
/// panel the rest, up to [`GOAL_BOX_MAX`]. 0 when there's no room for both.
fn goal_box_rows(level: &Level, body: u16) -> u16 {
    let left = body.saturating_sub(agents_box_rows(level));
    if left >= GOAL_BOX_MIN {
        return left.min(GOAL_BOX_MAX);
    }
    // A long list gives up rows before the panel goes.
    let squeezed = body.saturating_sub(AGENTS_BOX_ROWS);
    if squeezed >= GOAL_BOX_MIN {
        squeezed
    } else {
        0
    }
}

/// Rows the phases and agents box needs, borders included: its longest list, whichever pane
/// that is.
fn agents_box_rows(level: &Level) -> u16 {
    let data = &level.data;
    // Each phase's outline as it stands, and the blank row and line of its note under it.
    let longest = data
        .phases
        .iter()
        .map(|phase| {
            let rows: Vec<&WorkflowRow> = phase.rows.iter().collect();
            let outline = workflow_outline::items(&rows, data.worker.as_deref(), &|nested| {
                level.is_open(nested)
            });
            let notes = usize::from(phase.note.is_some())
                + usize::from(workflow_outline::deepest_note(&outline).is_some());
            outline.len() + if notes > 0 { notes + 1 } else { 0 }
        })
        .max()
        .unwrap_or(0)
        .max(data.phases.len());
    u16::try_from(longest)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .max(AGENTS_BOX_ROWS)
}

/// The goal panel: a second box under the agents, titled with whose goal it is and who keeps
/// it, with the lines, the tries, and the last check inside.
fn draw_goal_box(buf: &mut Buffer, area: Rect, level: &Level, theme: &Theme) {
    let bold = Style::new()
        .patch(theme.style(Role::Bright))
        .add_modifier(Modifier::BOLD);
    let Some(goal) = level.shown_goal() else {
        let title = [Span::styled("Goal", bold)];
        let inner = dock::draw_panel(buf, area, &title, &[], theme);
        panes::put_lines(buf, inner, &[dim_line(&level.no_goal_words(), theme)]);
        return;
    };
    let title = [
        Span::styled(theme.glyphs.question, theme.style(Role::Question)),
        Span::styled(
            format!(" Goal of {}", sanitize_line(&goal.label).trim()),
            bold,
        ),
    ];
    let note = goal_panel::kept_by(goal, theme);
    let inner = dock::draw_panel(buf, area, &title, &note, theme);
    let lines = goal_panel::panel(
        goal,
        usize::from(inner.width),
        usize::from(inner.height),
        theme,
    );
    panes::put_lines(buf, inner, &lines);
}

/// The one line under the header: what needs you, how the run ended, or the latest note. A
/// goal's own line shows when nothing else is said, since its panel under the agents says more.
///
/// A wait the line counts down is written out as it stands `shown_ms` after the data was
/// built: `tried again in 4m48s`, and `in a moment` once it is due.
fn status_lines(data: &WorkflowData, shown_ms: u64) -> Vec<StatusLine> {
    let Some(mut line) = said_line(data).cloned() else {
        return Vec::new();
    };
    if let Some(left) = line.left.take() {
        // A second shows for as long as any of it is left.
        let secs = left.ms.saturating_sub(shown_ms).div_ceil(1000);
        let time = match secs {
            0 => "a moment".to_owned(),
            secs => crate::base::format::duration_slot(Duration::from_secs(secs)),
        };
        line.text = format!("{} · {} {time}", line.text.trim(), left.words.trim());
    }
    vec![line]
}

fn said_line(data: &WorkflowData) -> Option<&StatusLine> {
    let said = data
        .status_lines
        .iter()
        .rev()
        .find(|line| line.kind != StatusKind::Goal);
    said.or(data.status_lines.first())
}

/// What a phase with no agents says: `Not started yet`, as Claude Code's does, or the question it
/// waits on you for, which only Argonauts Code has.
fn empty_phase(phase: &PhaseRow, state: &AppState, run: &RunId) -> String {
    let asks = state.runs.get(run).and_then(|run| {
        run.approvals
            .iter()
            .rev()
            .find(|open| open.opened.needs_you)
    });
    match (phase.status, asks) {
        (RowStatus::Running | RowStatus::NeedsYou, Some(open)) => format!(
            "Waiting for your answer: {}",
            sanitize_line(&open.opened.title).trim()
        ),
        (RowStatus::NeedsYou, None) => "Waiting for your answer in the conversation".to_owned(),
        (RowStatus::Done, _) => "No agents ran in this phase".to_owned(),
        _ => {
            let detail = sanitize_line(&phase.detail);
            match detail.trim() {
                "" => "Not started yet".to_owned(),
                detail => format!("Not started yet · {detail}"),
            }
        }
    }
}

fn dim_line(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), theme.style(Role::Dim)))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
