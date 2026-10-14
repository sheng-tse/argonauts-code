//! The workflow view, on Claude Code's workflow detail view: in the bottom region under the
//! conversation, the workflow's own name and description, its declared phases
//! numbered on the left, and the selected phase's agents on the right, each by its
//! `phase:target` label with its model, its stats, and its time. Every workflow draws here,
//! whoever runs it: the runner's, one of yours, or a CLI's own. Nested workflows are `◇` rows
//! that open their own view under a breadcrumb, and Enter on an agent opens it in the same
//! frame.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::agent_goal::{AgentGoal, GoalKeeper};
use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::events::run::WaitReason;
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::views::{
    AgentRow, InnerAgent, PhaseRow, RowStatus, StatusKind, StatusLine, WorkflowRow, WorkflowState,
    WorkflowView as WorkflowData,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::state::AppState;
use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

use super::agent::AgentView;
use super::dock::{self, Heading};
use super::follow::{Follower, Shown};
use super::goal::GoalView;
use super::goal_keys;
use super::goal_panel;
use super::list::{self, Nav};
use super::panes::{self, Footer, Marks, Side};
use super::session::{SessionView, Tab};
use super::workflow_rows::{
    self, Around, Cells, Models, PhaseColumns, RowColumns, row_label, row_status,
};
use super::workflow_tries;
use super::{View, ViewCtx, ViewOutcome};

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
const GOAL_BOX_MAX: u16 = 14;
const AGENTS_BOX_ROWS: u16 = 5;

#[derive(Debug)]
pub struct WorkflowView {
    /// The run's workflow at the bottom, nested ones above it; Esc pops one level.
    levels: Vec<Level>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Agent {
        session: SessionKey,
        inner: Option<InnerAgent>,
    },
    Workflow(WorkflowId),
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

    fn row(&self) -> Option<&WorkflowRow> {
        self.rows().get(self.row).copied()
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
    /// or one leaves the list.
    fn refresh(&mut self, data: WorkflowData) {
        let was = self.phase().map(|phase| phase.name.clone());
        let at = self.phase;
        self.data = data;
        self.phase = was
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
        self.row = self.row.min(self.rows().len().saturating_sub(1));
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
            levels: vec![Level::new(data)],
            agent: None,
            goal: None,
            pending: None,
            confirm: None,
            restart: None,
            follow: None,
        }
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
                root: self.levels.len() == 1 && !is_agent(&self.top().data.id),
            },
        }
    }

    /// How many workflows deep the view is: 1 for the run's own.
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    pub fn current(&self) -> &WorkflowData {
        &self.top().data
    }

    /// The agent shown in this frame, once its answer arrived.
    pub fn agent(&self) -> Option<&AgentView> {
        self.agent.as_ref()
    }

    fn top(&self) -> &Level {
        // `levels` is never empty: Esc on the last level closes the view instead.
        &self.levels[self.levels.len() - 1]
    }

    fn top_mut(&mut self) -> &mut Level {
        let last = self.levels.len() - 1;
        &mut self.levels[last]
    }

    fn run(&self) -> &RunId {
        &self.levels[0].data.run
    }

    /// The name, a nested workflow's parents before it, the description after what kind of
    /// workflow it is, and the totals: the agents this workflow started, done of all, its
    /// time, which for the run's own workflow is the run's, and where it stands once it isn't
    /// simply running. Tokens and spend stay on the row in the bottom list.
    fn heading(&self, ctx: &ViewCtx<'_>) -> Heading {
        let level = self.top();
        let data = &level.data;
        let mut names = data.breadcrumb.clone();
        if names.last().map(|name| name.trim()) != Some(data.name.trim()) {
            names.push(data.name.clone());
        }
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
            names,
            notes,
            description: data.description.clone(),
            totals,
        }
    }

    /// Where the run stands as the screen knows it, while the view is on the run's workflow.
    fn run_state(&self, state: &AppState) -> WorkflowState {
        use argonauts_core::events::run::RunOutcome;
        if self.levels.len() > 1 || is_agent(&self.top().data.id) {
            return WorkflowState::Running;
        }
        match state.runs.get(self.run()).and_then(|run| run.finished) {
            Some(RunOutcome::Merged | RunOutcome::PullRequest | RunOutcome::BranchLeft) => {
                WorkflowState::Done
            }
            Some(RunOutcome::Stopped | RunOutcome::Abandoned) => WorkflowState::Stopped,
            Some(RunOutcome::Failed) => WorkflowState::Failed,
            None if paused(state, self.run()) => WorkflowState::Paused,
            None => WorkflowState::Running,
        }
    }

    /// The run's own time, while the view is on the run's workflow.
    fn run_elapsed(&self, state: &AppState) -> Option<u64> {
        if self.levels.len() > 1 || is_agent(&self.top().data.id) {
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
            let enter = match level.side {
                Side::Left => "enter opens its agents",
                Side::Right => "⏎ open",
            };
            return Footer::hints(&[("↑↓ select", 0), (enter, 1), (panes::ESC_BACK, 0)]);
        }
        let pause = if paused(ctx.state, self.run()) {
            "p resume"
        } else {
            "p pause"
        };
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
            Side::Left if self.levels[0].data.style == Some(RunStyle::Goal) => Footer::hints(&[
                ("↑↓ select", 0),
                enter,
                ("tab goal", 4),
                ("x stop goal", 4),
                (pause, 2),
                (panes::ESC_BACK, 0),
                ("s save", 1),
            ]),
            Side::Left => Footer::hints(&[
                ("↑↓ select", 0),
                enter,
                ("x stop workflow", 4),
                (pause, 2),
                (panes::ESC_BACK, 0),
                ("s save", 1),
            ]),
            Side::Right => {
                let row = level.row();
                let mut hints = vec![("↑↓ select", 0), ("⏎ open", 6)];
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
                hints.extend([("m message", 4), ("x stop", 5), ("r restart", 4)]);
                if hold.is_none() {
                    hints.push((pause, 3));
                }
                hints.extend([("f filter", 2), (panes::ESC_BACK, 0), ("s save", 1)]);
                Footer::hints(&hints)
            }
        }
    }

    /// Enter or → on the right pane's row.
    fn open_row(&mut self) -> ViewOutcome {
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
            WorkflowRow::Workflow(nested) => {
                let id = nested.id.clone();
                self.pending = Some(Pending::Workflow(id.clone()));
                ViewOutcome::Request(ViewRequest::Workflow {
                    run,
                    workflow: Some(id),
                })
            }
            // A command has no session of its own to open.
            WorkflowRow::Command(_) => ViewOutcome::Consumed,
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
                let count = level.rows().len();
                level.row = step(level.row, count, nav);
            }
        }
    }

    /// ← and Esc go back a level: from the agents to the phases, out of a nested workflow,
    /// and from the phases out of the view.
    fn back(&mut self) -> ViewOutcome {
        self.confirm = None;
        if self.top().side == Side::Right {
            self.top_mut().side = Side::Left;
            return ViewOutcome::Consumed;
        }
        if self.levels.len() > 1 {
            self.levels.pop();
            self.pending = None;
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
                Stop::Workflow if ctx.state.attached.as_ref() == Some(self.run()) => {
                    ViewOutcome::Send(UiCommand::Stop)
                }
                Stop::Workflow => ViewOutcome::Notice(format!(
                    "Only the run on screen can be stopped. Resume {} first, then stop it",
                    sanitize_line(self.run().as_str())
                )),
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
            workflow: (self.levels.len() > 1).then(|| level.data.id.clone()),
        })
    }

    fn accept_agent(&mut self, view: &argonauts_core::views::AgentView) -> bool {
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
        let agent = match self.top().phase() {
            Some(phase) => AgentView::in_phase(view.clone(), phase),
            None => AgentView::new(view.clone()),
        };
        self.agent = Some(agent.lend(self.follow.as_ref().map(Follower::another)));
        true
    }

    fn accept_workflow(&mut self, view: &WorkflowData) -> bool {
        // Before the main agent picks the run's workflow, the view opens on Understand; the run's
        // own workflow takes its place once it starts.
        if self.levels.len() == 1
            && self.agent.is_none()
            && self.pending.is_none()
            && view.breadcrumb.is_empty()
            && !is_agent(&self.levels[0].data.id)
            && !is_agent(&view.id)
            && view.id != self.levels[0].data.id
        {
            self.levels[0] = Level::new(view.clone());
            self.pending = None;
            self.confirm = None;
            return true;
        }
        if self.pending == Some(Pending::Workflow(view.id.clone())) {
            self.pending = None;
            self.confirm = None;
            self.levels.push(Level::new(view.clone()));
            return true;
        }
        match self
            .levels
            .iter_mut()
            .find(|level| level.data.id == view.id)
        {
            Some(level) => {
                level.refresh(view.clone());
                true
            }
            None => false,
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

    /// Taller when the workflow has goals and the usual frame is too short for the goal panel
    /// under the agents. It goes by the workflow and not by the row selected, so the frame
    /// never jumps as you move.
    fn dock_height(&mut self, screen: ratatui::layout::Size, _ctx: &ViewCtx<'_>) -> Option<u16> {
        let data = &self.top().data;
        if self.agent.is_some() || self.goal.is_some() || data.goals.is_empty() {
            return None;
        }
        // The rule, the name, the description, a blank row, and the footer, around the boxes.
        let around = 5 + status_lines(data, 0).len() as u16;
        let wanted = around + agents_box_rows(data) + GOAL_BOX_ROWS;
        Some(dock::height(screen.height).max(wanted))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
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
            let models = Models::of(&self.levels[self.levels.len() - 1].data, ctx);
            agent.render_frame(area, buf, ctx, &heading, &status, &models);
            return;
        }
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
            let rows = goal_box_rows(&level.data, frame.panes.body.height);
            dock::carve_below(&mut frame, rows, AGENTS_BOX_ROWS)
        };
        dock::draw_rule(buf, &frame, theme);
        dock::draw_heading(buf, &frame, &heading, theme);
        dock::draw_status(buf, &frame, &status, theme);

        let panes = frame.panes;
        let phases_in = if two_panes { panes.left } else { panes.right };
        let phase_lines = phases.lines(
            &level.data,
            level.phase,
            level.side == Side::Left,
            usize::from(phases_in.width),
            theme,
        );
        let (first, last) = workflow_rows::window(
            level.phase,
            phase_lines.len(),
            usize::from(phases_in.height),
        );
        if two_panes || level.side == Side::Left {
            panes::put_lines(buf, phases_in, &phase_lines[first..last]);
        }
        if !two_panes && level.side == Side::Left {
            let mark = workflow_rows::scroll_mark(first, last, phase_lines.len(), theme);
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
            Some(_) if !shown.is_empty() => {
                let models = Models::of(&level.data, ctx);
                let around = Around {
                    run: &level.data.run,
                    models: &models,
                    ticking: level.ticking(ctx.now),
                    ctx,
                };
                let rows = RowColumns::fit(
                    workflow_rows::label_width(&level.data),
                    usize::from(panes.right.width),
                    theme,
                );
                let mut lines = Vec::new();
                for (i, row) in shown.iter().enumerate() {
                    let selected = level.side == Side::Right && i == level.row;
                    if i == level.row {
                        selected_line = lines.len();
                    }
                    let cells = Cells::of(row, level.goal_of(row), &around);
                    lines.push(rows.line(&cells, selected, theme));
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
        // What the phase waits for, a blank row under its rows.
        if let Some(note) = level.phase().and_then(|phase| phase.note.as_deref())
            && level.filter == Filter::All
        {
            let note = sanitize_line(note);
            if !note.trim().is_empty() {
                lines.push(Line::default());
                lines.push(dim_line(&format!("  {}", note.trim()), theme));
            }
        }
        let (first, last) = workflow_rows::window(selected_line, lines.len(), height);
        let mark = workflow_rows::scroll_mark(first, last, lines.len(), theme);
        dock::draw_box(buf, &frame, ["Phases", &right_title], &mark, theme);
        panes::put_lines(buf, panes.right, &lines[first..last]);
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
            KeyCode::Left if self.top().side == Side::Right && self.top_mut().fold_tries(false) => {
                ViewOutcome::Consumed
            }
            KeyCode::Right if self.top().side == Side::Right && self.top_mut().fold_tries(true) => {
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
                if paused(ctx.state, self.run()) {
                    ViewOutcome::Send(UiCommand::Resume {
                        run: Some(self.run().clone()),
                    })
                } else {
                    super::pause(self.run(), ctx)
                }
            }
            KeyCode::Char('f') if plain && self.top().side == Side::Right => {
                let level = self.top_mut();
                level.filter = level.filter.next();
                level.row = 0;
                ViewOutcome::Consumed
            }
            KeyCode::Char('s') if plain => self.save(),
            // Space closes the view from wherever it is, as in Claude Code.
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
            KeyCode::Tab if self.levels[0].data.style == Some(RunStyle::Goal) => {
                self.pending = Some(Pending::Goal);
                ViewOutcome::Request(super::goal::request(self.run()))
            }
            _ => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        match data {
            ViewData::Workflow { view } if view.run == *self.run() => self.accept_workflow(view),
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
        [elapsed, wait].into_iter().flatten().min()
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
fn goal_box_rows(data: &WorkflowData, body: u16) -> u16 {
    let left = body.saturating_sub(agents_box_rows(data));
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
fn agents_box_rows(data: &WorkflowData) -> u16 {
    let longest = data
        .phases
        .iter()
        .map(|phase| phase.rows.len())
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
            secs => crate::format::duration_slot(Duration::from_secs(secs)),
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

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::settings::ui::CliInternals;
    use argonauts_core::views::{
        AgentRow, AgentView as AgentData, NestedWorkflowRow, PhaseCount, RowNote, StatusKind, Tone,
    };
    use argonauts_core::workflow::StandardPhase;

    use super::*;
    use crate::stability::{assert_frames, buffer_text};
    use crate::state::{AppState, RunState};
    use crate::theme::Glyphs;

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn run() -> RunId {
        RunId::new("run-01").unwrap()
    }

    fn agent(session: &str, label: &str, status: RowStatus) -> AgentRow {
        AgentRow {
            session: SessionKey::parse(session).unwrap(),
            label: label.into(),
            worker: "claude-code/opus".parse().unwrap(),
            status,
            tokens: 381_000,
            spent: Spend::default(),
            elapsed_ms: 362_000,
            subagents: 0,
            context: None,
            fallback_for: None,
            answered: false,
            tries: Vec::new(),
            note: None,
            inner: None,
        }
    }

    fn phase(name: &str, status: RowStatus, rows: Vec<WorkflowRow>) -> PhaseRow {
        let done = rows
            .iter()
            .filter(|row| row_status(row) == RowStatus::Done)
            .count() as u32;
        PhaseRow {
            name: name.into(),
            standard: StandardPhase::Do,
            status,
            detail: String::new(),
            try_number: None,
            count: PhaseCount::of(done, rows.len() as u32),
            rows,
            note: None,
        }
    }

    fn workflow(id: u32, name: &str, rows: Vec<WorkflowRow>) -> WorkflowData {
        WorkflowData {
            run: run(),
            id: WorkflowId::numbered(id),
            name: name.into(),
            description: "Builds the kit".into(),
            breadcrumb: if id == 1 {
                Vec::new()
            } else {
                vec!["ui-kit".into()]
            },
            agents_done: 1,
            agents_total: 3,
            elapsed_ms: 60_000,
            spent: Spend::default(),
            phases: vec![phase("Build", RowStatus::Running, rows)],
            style: None,
            status_lines: Vec::new(),
            notes: Vec::new(),
            required: Vec::new(),
            goals: Vec::new(),
            worker: None,
            state: Default::default(),
        }
    }

    fn parent() -> WorkflowData {
        workflow(
            1,
            "ui-kit",
            vec![
                WorkflowRow::Agent(agent("s-001", "build:a", RowStatus::Done)),
                WorkflowRow::Agent(agent("s-002", "build:b", RowStatus::Running)),
                WorkflowRow::Workflow(NestedWorkflowRow {
                    id: WorkflowId::numbered(2),
                    name: "build:c".into(),
                    status: RowStatus::Running,
                    agents_done: 0,
                    agents_total: 2,
                    workers: vec!["codex/gpt-5.6".parse().unwrap()],
                    elapsed_ms: 720_000,
                    parent: None,
                    worker: None,
                }),
            ],
        )
    }

    /// Understand done, Fix running, Review and Merge still to come.
    fn fixing() -> WorkflowData {
        let mut fix_slugify = agent("s-006", "fix:slugify", RowStatus::Done);
        fix_slugify.worker = "claude-code/claude-haiku-4-5".parse().unwrap();
        fix_slugify.tokens = 18_100;
        fix_slugify.elapsed_ms = 130_000;
        let mut fix_median = agent("s-007", "fix:median", RowStatus::Running);
        fix_median.worker = "codex/gpt-5.6-luna".parse().unwrap();
        fix_median.tokens = 31_200;
        fix_median.elapsed_ms = 151_000;
        let read = |n: usize| {
            WorkflowRow::Agent(agent(
                &format!("s-00{n}"),
                &format!("read:{n}"),
                RowStatus::Done,
            ))
        };
        let mut data = workflow(1, "fix-slugify-median", Vec::new());
        data.description =
            "Fix slugify's dashes and median's even and empty lists, with tests".into();
        data.agents_done = 6;
        data.agents_total = 7;
        data.elapsed_ms = 271_000;
        data.phases = vec![
            phase("Understand", RowStatus::Done, (1..=5).map(read).collect()),
            phase(
                "Fix",
                RowStatus::Running,
                vec![
                    WorkflowRow::Agent(fix_slugify),
                    WorkflowRow::Agent(fix_median),
                ],
            ),
            phase("Review", RowStatus::Waiting, Vec::new()),
            phase("Merge", RowStatus::Waiting, Vec::new()),
        ];
        data
    }

    fn ctx_with<'a>(theme: &'a Theme, state: &'a AppState) -> ViewCtx<'a> {
        ViewCtx {
            theme,
            state,
            now: Instant::now(),
        }
    }

    fn press(view: &mut WorkflowView, code: KeyCode) -> ViewOutcome {
        let theme = theme();
        let state = AppState::default();
        view.handle_key(
            KeyEvent::new(code, KeyModifiers::NONE),
            &ctx_with(&theme, &state),
        )
    }

    fn press_in(view: &mut WorkflowView, code: KeyCode, state: &AppState) -> ViewOutcome {
        let theme = theme();
        view.handle_key(
            KeyEvent::new(code, KeyModifiers::NONE),
            &ctx_with(&theme, state),
        )
    }

    fn draw_with(view: &mut WorkflowView, width: u16, theme: &Theme, state: &AppState) -> String {
        let area = Rect::new(0, 0, width, 14);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx_with(theme, state));
        assert_frames(&buf);
        buffer_text(&buf)
    }

    fn screen(view: &mut WorkflowView) -> String {
        draw_with(view, 80, &theme(), &AppState::default())
    }

    fn row_with<'a>(text: &'a str, label: &str) -> &'a str {
        text.lines()
            .find(|line| line.contains(label))
            .unwrap_or_default()
    }

    /// A run on screen and going, so stopping and pausing act.
    fn going() -> AppState {
        let mut state = AppState::default();
        state.attached = Some(run());
        state.runs.insert(run(), RunState::new(run()));
        state
    }

    /// A review Claude Code's workflow asked Astra for through Argonauts Code is Astra's row:
    /// its tokens while Astra reads, and once answered what Astra decided after them.
    #[test]
    fn a_step_a_role_answered_says_what_it_decided() {
        let mut store = agent("s-002", "review:store", RowStatus::Running);
        store.worker = "astra".parse().unwrap();
        store.tokens = 41_200;
        store.answered = true;
        let mut cli = agent("s-003", "review:cli", RowStatus::Done);
        cli.worker = "astra".parse().unwrap();
        cli.answered = true;
        cli.note = Some(RowNote::new("approved", Tone::Good));
        let mut data = workflow(1, "add-due-dates", Vec::new());
        data.phases = vec![phase(
            "Review",
            RowStatus::Running,
            vec![WorkflowRow::Agent(store), WorkflowRow::Agent(cli)],
        )];
        let mut view = WorkflowView::new(data);
        let text = draw_with(&mut view, 100, &theme(), &going());
        let store = row_with(&text, "review:store");
        assert!(store.contains("astra · 41.2k tok"), "{text}");
        assert!(!store.contains("approved"), "{text}");
        let cli = row_with(&text, "review:cli");
        assert!(cli.contains("astra · 381k tok · approved"), "{text}");
    }

    /// The open view as Claude Code's: a rule six columns short, the name
    /// and the description with its totals inside it, numbered phases with counts only once
    /// started, `phase:target` rows with the model and tokens, and the time flush right. No
    /// tokens or spend in the header: they are on the run's row in the bottom list.
    #[test]
    fn the_view_reads_like_claude_codes() {
        let mut view = WorkflowView::new(fixing());
        let text = draw_with(&mut view, 100, &theme(), &going());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0].trim_end().chars().count(), 94, "{text}");
        assert!(lines[0].starts_with("──"), "{text}");
        assert_eq!(lines[1].trim_end(), " fix-slugify-median", "{text}");
        // The totals end under the box's right border.
        assert!(
            lines[2].trim_end().ends_with("6/7 agents · 4m31s"),
            "{text}"
        );
        assert_eq!(lines[2].trim_end().chars().count(), 93, "{text}");
        assert!(lines[3].trim().is_empty(), "{text}");
        assert!(
            lines[4].starts_with(" ╭ Phases ────────────┬ Fix · 2 agents ─"),
            "{text}"
        );
        assert!(lines[4].trim_end().ends_with('╮'), "{text}");
        assert_eq!(lines[4].trim_end().chars().count(), 93, "{text}");
        assert!(lines[5].starts_with(" │   ✔ Understand 5/5 │"), "{text}");
        assert!(lines[6].starts_with(" │ ❯ 2 Fix        1/2 │"), "{text}");
        assert!(lines[7].starts_with(" │   3 Review         │"), "{text}");
        assert!(lines[8].starts_with(" │   4 Merge          │"), "{text}");
        let slugify = row_with(&text, "fix:slugify");
        assert!(
            slugify.contains("│  ✔ fix:slugify  Haiku 4.5 · 18.1k tok"),
            "{text}"
        );
        assert!(slugify.trim_end().ends_with("2m10s │"), "{text}");
        assert!(
            row_with(&text, "fix:median").contains("⏺ fix:median   GPT 5.6 Luna · 31.2k tok"),
            "{text}"
        );
        assert!(!text.contains("0/0"), "{text}");
        assert!(!text.contains("task-"), "{text}");
        assert!(!text.contains('$'), "{text}");
        let footer = lines.last().unwrap_or(&"").trim_end();
        assert_eq!(
            footer,
            " ↑↓ select · enter opens its agents · x stop workflow · p pause · esc back · s save"
        );
    }

    #[test]
    fn the_frame_holds_at_every_width() {
        for width in [80, 100, 120, 200] {
            let mut view = WorkflowView::new(fixing());
            let text = draw_with(&mut view, width, &theme(), &going());
            assert!(text.contains("fix:slugify"), "{width}: {text}");
            let mut kit = WorkflowView::new(kit());
            draw_with(&mut kit, width, &theme(), &going());
        }
    }

    /// Every row of a pane starts its model and ends its time in the same cells, with long and
    /// CJK labels and a crew from three CLIs, at every width the snapshots use.
    #[test]
    fn rows_keep_their_columns_at_every_width() {
        let mut data = fixing();
        let mut cjk = agent("s-008", "修复:导出测试", RowStatus::Running);
        cjk.worker = "antigravity-cli/gemini-3.6-flash-low".parse().unwrap();
        cjk.tokens = 7_100;
        cjk.elapsed_ms = 58_000;
        data.phases[1].rows.push(WorkflowRow::Agent(cjk));
        let cell_of = |row: &str, needle: &str| {
            let at = row.find(needle)?;
            Some(crate::text::str_width(&row[..at]))
        };
        for width in [80, 100, 120, 200] {
            let text = draw_with(
                &mut WorkflowView::new(data.clone()),
                width,
                &theme(),
                &going(),
            );
            let rows = [
                ("fix:slugify", "Haiku"),
                ("fix:median", "GPT"),
                ("修复:导出测试", "Gemini"),
            ];
            let models: Vec<Option<usize>> = rows
                .iter()
                .map(|(label, model)| cell_of(row_with(&text, label), model))
                .collect();
            assert!(
                models.iter().all(|at| at.is_some() && *at == models[0]),
                "{width}: {text}"
            );
            // The time ends one cell before the right border.
            let ends: Vec<Option<usize>> = rows
                .iter()
                .map(|(label, _)| {
                    let row = row_with(&text, label);
                    let border = row.rfind(" │")?;
                    let time = row[..border].trim_end_matches(' ');
                    (time.len() == border && time.ends_with('s'))
                        .then(|| crate::text::str_width(time))
                })
                .collect();
            assert!(
                ends.iter().all(|end| end.is_some() && *end == ends[0]),
                "{width}: {text}"
            );
            assert!(!text.contains("0/0"), "{width}: {text}");
        }
    }

    /// A row names its model alone, as Claude Code's rows do, and the opened agent names the
    /// CLI. A narrow pane cuts the stats before the model.
    #[test]
    fn a_row_names_its_model_and_the_opened_agent_its_cli() {
        let wide = draw_with(&mut WorkflowView::new(fixing()), 100, &theme(), &going());
        assert!(
            row_with(&wide, "fix:slugify").contains("fix:slugify  Haiku 4.5 · 18.1k tok"),
            "{wide}"
        );
        assert!(!wide.contains("on Claude Code"), "{wide}");
        let narrow = draw_with(&mut WorkflowView::new(fixing()), 80, &theme(), &going());
        assert!(
            row_with(&narrow, "fix:median").contains("GPT 5.6 Luna · 31.2k tok"),
            "{narrow}"
        );
    }

    /// A phase still to come says `Not started yet`, as Claude Code's does, and nothing else.
    #[test]
    fn a_phase_still_to_come_has_not_started() {
        let mut view = WorkflowView::new(fixing());
        press(&mut view, KeyCode::Down);
        let text = draw_with(&mut view, 100, &theme(), &going());
        assert!(text.contains("┬ Review · 0 agents ─"), "{text}");
        assert!(
            row_with(&text, "Understand").contains("│ Not started yet"),
            "{text}"
        );
    }

    /// Claude Code's keys for each pane, the same whichever row is picked and whether or not
    /// the run still goes, so the line holds still. `p` says what it does now.
    #[test]
    fn the_keys_hold_still_for_each_pane() {
        let state = going();
        let mut view = WorkflowView::new(fixing());
        let footer = |view: &mut WorkflowView, state: &AppState| {
            let text = draw_with(view, 124, &theme(), state);
            text.lines().last().unwrap_or_default().trim().to_owned()
        };
        let phases = "↑↓ select · enter opens its agents · x stop workflow · p pause · esc back · \
                      s save";
        assert_eq!(footer(&mut view, &state), phases);
        press_in(&mut view, KeyCode::Right, &state);
        let agents = "↑↓ select · ⏎ open · m message · x stop · r restart · p pause · f filter · \
                      esc back · s save";
        // The first row is done and the second is running: the keys are the same.
        assert_eq!(footer(&mut view, &state), agents);
        press_in(&mut view, KeyCode::Down, &state);
        assert_eq!(footer(&mut view, &state), agents);
        // Paused, p resumes.
        let mut paused = state.clone();
        if let Some(run) = paused.runs.get_mut(&run()) {
            run.waiting = Some(WaitReason::Paused);
        }
        assert!(footer(&mut view, &paused).contains("p resume"));
        assert!(matches!(
            press_in(&mut view, KeyCode::Char('p'), &paused),
            ViewOutcome::Send(UiCommand::Resume { run: Some(_) })
        ));
        // Once the workflow ends the keys stay, and one that can't act says why.
        let mut done = fixing();
        for phase in &mut done.phases {
            phase.status = RowStatus::Done;
        }
        let mut ended = state.clone();
        if let Some(run) = ended.runs.get_mut(&run()) {
            run.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        }
        let mut view = WorkflowView::new(done);
        assert_eq!(footer(&mut view, &ended), phases);
        assert!(matches!(
            press_in(&mut view, KeyCode::Char('x'), &ended),
            ViewOutcome::Notice(_)
        ));
    }

    /// On a screen too narrow for the whole line, the phases' keys give up saving and pausing
    /// first, and keep what Enter opens and how to stop.
    #[test]
    fn a_narrow_screen_keeps_what_enter_opens() {
        let state = going();
        let mut view = WorkflowView::new(fixing());
        let text = draw_with(&mut view, 80, &theme(), &state);
        assert_eq!(
            text.lines().last().unwrap_or_default().trim(),
            "↑↓ select · enter opens its agents · x stop workflow · esc back"
        );
    }

    /// Under 64 columns the view draws one pane, as Claude Code's does: the phases, and once
    /// Enter goes to them, the open phase's agents across the whole box, with room for what
    /// each row says.
    #[test]
    fn a_screen_under_64_columns_draws_one_pane() {
        let state = going();
        let mut view = WorkflowView::new(fixing());
        let wide = draw_with(&mut view, 64, &theme(), &state);
        assert!(wide.contains("╭ Phases ─") && wide.contains("┬"), "{wide}");

        let phases = draw_with(&mut view, 63, &theme(), &state);
        let top = phases
            .lines()
            .find(|line| line.contains('╭'))
            .unwrap_or_else(|| panic!("{phases}"));
        assert!(top.contains("╭ Phases ─"), "{phases}");
        assert!(!phases.contains('┬') && !phases.contains('┴'), "{phases}");
        // Enter is the only way to a phase's agents here, so the keys name it beside stopping,
        // and on a narrower screen still it outlasts every key but the way out.
        let keys = |text: &str| text.lines().last().unwrap_or_default().trim().to_owned();
        assert_eq!(
            keys(&phases),
            "↑↓ select · enter opens · x stop workflow · esc back"
        );
        assert_eq!(
            keys(&draw_with(&mut view, 44, &theme(), &state)),
            "↑↓ select · enter opens · esc back"
        );
        // Its rows have the whole box: the cursor, the phase, and its count at the far end.
        let fix = row_with(&phases, "2 Fix");
        assert!(
            fix.contains("❯") && fix.trim_end().ends_with("1/2 │"),
            "{phases}"
        );

        press_in(&mut view, KeyCode::Enter, &state);
        let agents = draw_with(&mut view, 63, &theme(), &state);
        let top = agents
            .lines()
            .find(|line| line.contains('╭'))
            .unwrap_or_else(|| panic!("{agents}"));
        assert!(top.contains("╭ Fix · "), "{agents}");
        assert!(!agents.contains("╭ Phases"), "{agents}");
        assert!(!agents.contains('┬'), "{agents}");
        // Esc goes back to the phases.
        press_in(&mut view, KeyCode::Esc, &state);
        let back = draw_with(&mut view, 63, &theme(), &state);
        assert!(back.contains("╭ Phases ─"), "{back}");
    }

    /// `r` says why when there's nothing to restart: an agent that has finished, or one with
    /// no task of its own.
    #[test]
    fn r_says_why_when_there_is_nothing_to_restart() {
        let state = going();
        let mut view = WorkflowView::new(fixing());
        press_in(&mut view, KeyCode::Right, &state);
        match press_in(&mut view, KeyCode::Char('r'), &state) {
            ViewOutcome::Notice(why) => assert!(why.contains("isn't running"), "{why}"),
            other => panic!("a finished agent restarted: {other:?}"),
        }
        press_in(&mut view, KeyCode::Down, &state);
        // The screen holds no task for it, so there's nothing to start again.
        match press_in(&mut view, KeyCode::Char('r'), &state) {
            ViewOutcome::Notice(why) => assert!(why.contains("no task of its own"), "{why}"),
            other => panic!("an agent without a task restarted: {other:?}"),
        }
    }

    #[test]
    fn x_stops_the_workflow_or_the_agent_after_asking() {
        let state = going();
        let mut view = WorkflowView::new(fixing());
        assert!(matches!(
            press_in(&mut view, KeyCode::Char('x'), &state),
            ViewOutcome::Consumed
        ));
        let text = draw_with(&mut view, 100, &theme(), &state);
        assert!(
            text.contains("x again to stop the workflow · esc to keep"),
            "{text}"
        );
        assert!(matches!(
            press_in(&mut view, KeyCode::Char('x'), &state),
            ViewOutcome::Send(UiCommand::Stop)
        ));
        // On the agents, the selected running agent.
        press_in(&mut view, KeyCode::Right, &state);
        press_in(&mut view, KeyCode::Down, &state);
        press_in(&mut view, KeyCode::Char('x'), &state);
        let outcome = press_in(&mut view, KeyCode::Char('x'), &state);
        assert!(
            matches!(&outcome, ViewOutcome::Send(UiCommand::StopAgent { session, .. }) if session.as_str() == "s-007"),
            "{outcome:?}"
        );
        // Esc between the two keeps it.
        press_in(&mut view, KeyCode::Char('x'), &state);
        assert!(matches!(
            press_in(&mut view, KeyCode::Esc, &state),
            ViewOutcome::Consumed
        ));
        assert!(view.confirm.is_none());
    }

    #[test]
    fn f_filters_the_agents_and_the_title_says_so() {
        let state = going();
        let mut view = WorkflowView::new(fixing());
        // The filter is the agents' key, as in Claude Code.
        press_in(&mut view, KeyCode::Right, &state);
        press_in(&mut view, KeyCode::Char('f'), &state);
        let text = draw_with(&mut view, 100, &theme(), &state);
        assert!(text.contains("Fix · showing 1 running"), "{text}");
        assert!(!text.contains("fix:slugify"), "{text}");
        assert!(text.contains("fix:median"), "{text}");
        for _ in 0..3 {
            press_in(&mut view, KeyCode::Char('f'), &state);
        }
        let text = draw_with(&mut view, 100, &theme(), &state);
        assert!(text.contains("Fix · 2 agents"), "{text}");
    }

    #[test]
    fn space_closes_the_view_from_either_pane() {
        let mut view = WorkflowView::new(fixing());
        assert!(matches!(
            press(&mut view, KeyCode::Char(' ')),
            ViewOutcome::Close
        ));
        press(&mut view, KeyCode::Right);
        assert!(matches!(
            press(&mut view, KeyCode::Char(' ')),
            ViewOutcome::Close
        ));
    }

    #[test]
    fn s_saves_the_workflow() {
        let mut view = WorkflowView::new(fixing());
        assert!(matches!(
            press(&mut view, KeyCode::Char('s')),
            ViewOutcome::Send(UiCommand::SaveWorkflow { workflow: None, .. })
        ));
        // Tab isn't a key here, as in Claude Code.
        assert!(matches!(
            press(&mut view, KeyCode::Tab),
            ViewOutcome::Ignored
        ));
    }

    /// The status lines sit under the description: what waits for you, the latest note, or
    /// how the run ended.
    #[test]
    fn status_lines_sit_under_the_description() {
        let mut data = fixing();
        data.status_lines = vec![StatusLine::new(
            StatusKind::Note,
            "fix:slugify passed its tests, so review:slugify started",
        )];
        let mut view = WorkflowView::new(data);
        let text = draw_with(&mut view, 100, &theme(), &going());
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[3].starts_with(" › fix:slugify passed its tests"),
            "{text}"
        );
        assert!(lines[4].trim().is_empty(), "{text}");
        assert!(lines[5].starts_with(" ╭ Phases"), "{text}");
    }

    /// When a phase is added above the cursor, the cursor stays on its phase.
    #[test]
    fn the_cursor_stays_on_its_phase_when_one_is_added() {
        let mut view = WorkflowView::new(fixing());
        assert_eq!(view.top().phase().map(|p| p.name.as_str()), Some("Fix"));
        let mut more = fixing();
        more.phases
            .insert(1, phase("Plan", RowStatus::Done, Vec::new()));
        assert!(view.accept_workflow(&more));
        assert_eq!(view.top().phase().map(|p| p.name.as_str()), Some("Fix"));
    }

    #[test]
    fn vim_keys_move_open_and_go_up_only_when_on() {
        let off = list::keys_state(false);
        let on = list::keys_state(true);
        let mut view = WorkflowView::new(parent());
        for c in ['j', 'k', 'g', 'G', 'h'] {
            assert!(
                matches!(
                    press_in(&mut view, KeyCode::Char(c), &off),
                    ViewOutcome::Ignored
                ),
                "{c}"
            );
        }
        assert_eq!(view.top().side, Side::Left);
        press_in(&mut view, KeyCode::Char('l'), &on);
        assert_eq!((view.top().side, view.top().row), (Side::Right, 0));
        press_in(&mut view, KeyCode::Char('j'), &on);
        assert_eq!(view.top().row, 1);
        press_in(&mut view, KeyCode::Char('G'), &on);
        assert_eq!(view.top().row, 2);
        press_in(&mut view, KeyCode::Char('g'), &on);
        assert_eq!(view.top().row, 0);
        // `h` goes back a level as ← does, and closes from the phases.
        press_in(&mut view, KeyCode::Char('h'), &on);
        assert_eq!(view.top().side, Side::Left);
        assert!(matches!(
            press_in(&mut view, KeyCode::Char('h'), &on),
            ViewOutcome::Close
        ));
        press(&mut view, KeyCode::Right);
        press(&mut view, KeyCode::End);
        assert_eq!(view.top().row, 2);
        press(&mut view, KeyCode::Home);
        assert_eq!(view.top().row, 0);
    }

    /// Esc goes back a level, as ← does: the agents, a nested workflow, then the view.
    #[test]
    fn esc_goes_back_a_level() {
        let mut view = WorkflowView::new(parent());
        press(&mut view, KeyCode::Right);
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Down);
        let outcome = press(&mut view, KeyCode::Enter);
        assert!(
            matches!(
                &outcome,
                ViewOutcome::Request(ViewRequest::Workflow { workflow: Some(id), .. })
                    if *id == WorkflowId::numbered(2)
            ),
            "{outcome:?}"
        );
        let nested = workflow(
            2,
            "build:c",
            vec![WorkflowRow::Agent(agent(
                "s-004",
                "test:a",
                RowStatus::Running,
            ))],
        );
        assert!(view.accept(&ViewData::Workflow {
            view: Box::new(nested)
        }));
        assert_eq!(view.depth(), 2);
        let text = screen(&mut view);
        assert!(
            text.lines()
                .nth(1)
                .unwrap_or_default()
                .starts_with(" ui-kit › build:c"),
            "{text}"
        );
        press(&mut view, KeyCode::Right);
        assert!(matches!(
            press(&mut view, KeyCode::Esc),
            ViewOutcome::Consumed
        ));
        assert_eq!((view.depth(), view.top().side), (2, Side::Left));
        assert!(matches!(
            press(&mut view, KeyCode::Esc),
            ViewOutcome::Consumed
        ));
        assert_eq!(view.depth(), 1);
        assert_eq!(view.current().name, "ui-kit");
        press(&mut view, KeyCode::Esc);
        assert!(matches!(press(&mut view, KeyCode::Esc), ViewOutcome::Close));
    }

    #[test]
    fn agent_answer_accepted_by_open_view() {
        let mut view = WorkflowView::new(parent());
        // An answer nobody asked for, for an agent, stays with the runtime.
        let data = AgentData {
            run: run(),
            row: agent("s-002", "build:b", RowStatus::Running),
            prompt: "Build the history cells.".into(),
            activity: Vec::new(),
            tool_calls: 0,
            diff: None,
            subagents: None,
            outcome: None,
            goal: None,
            plan: None,
            facts: Vec::new(),
            to_fix: None,
            review: None,
        };
        let answer = ViewData::Agent {
            view: Box::new(data.clone()),
        };
        assert!(!view.accept(&answer));

        press(&mut view, KeyCode::Right);
        press(&mut view, KeyCode::Down);
        let outcome = press(&mut view, KeyCode::Enter);
        assert!(
            matches!(&outcome, ViewOutcome::Request(ViewRequest::Agent { session, .. }) if session.as_str() == "s-002"),
            "{outcome:?}"
        );
        assert!(view.accept(&answer));
        assert_eq!(
            view.agent().map(|agent| agent.data.row.label.as_str()),
            Some("build:b")
        );
        let text = screen(&mut view);
        // Two agents of its own and the nested workflow's two, as the workflow view counts.
        assert!(text.contains("Build · 4 agents"), "{text}");
        assert!(text.contains("⏺ Running"), "{text}");
        assert!(text.contains("│ Prompt "), "{text}");

        // Another run's answer isn't this view's.
        let mut other = data;
        other.run = RunId::new("run-02").unwrap();
        assert!(!view.accept(&ViewData::Agent {
            view: Box::new(other)
        }));

        // `s` in the agent's detail saves the workflow, as in Claude Code.
        assert!(matches!(
            press(&mut view, KeyCode::Char('s')),
            ViewOutcome::Send(UiCommand::SaveWorkflow { .. })
        ));
        // Esc leaves the agent for the workflow's frame.
        assert!(matches!(
            press(&mut view, KeyCode::Esc),
            ViewOutcome::Consumed
        ));
        assert!(view.agent().is_none());
    }

    fn kit() -> WorkflowData {
        let mut rows = Vec::new();
        for (session, label, by) in [
            ("s-002", "build:a-terminal-bottom", "claude-code/opus"),
            ("s-003", "build:b-history-cells", "codex/gpt-5.6"),
            ("s-005", "build:d-plain-mode", "agy/gemini-3.6"),
        ] {
            let mut row = agent(session, label, RowStatus::Done);
            row.worker = by.parse().unwrap();
            rows.push(WorkflowRow::Agent(row));
        }
        if let WorkflowRow::Agent(row) = &mut rows[1] {
            row.subagents = 2;
        }
        rows.insert(2, parent().phases[0].rows[2].clone());
        workflow(1, "argonauts-ui-kit", rows)
    }

    #[test]
    fn ascii_frames_join_and_keep_their_columns() {
        let mut ascii = Theme::dark();
        ascii.glyphs = Glyphs::ASCII;
        let state = AppState::default();
        let text = draw_with(&mut WorkflowView::new(kit()), 100, &ascii, &state);
        let box_drawing = |c: char| ('\u{2500}'..='\u{257F}').contains(&c);
        assert!(!text.chars().any(box_drawing), "{text}");
        assert!(text.contains("+ Phases ---"), "{text}");
        assert!(!text.contains(['↑', '↓', '→']), "{text}");
        assert!(text.contains("up/down select"), "{text}");
        // The model sits in one column, after the widest label.
        let models = [
            ("build:a", "Opus"),
            ("build:b", "GPT"),
            ("build:d", "Gemini"),
        ]
        .map(|(label, model)| row_with(&text, label).find(model));
        assert!(
            models.iter().all(|at| at.is_some() && *at == models[0]),
            "{text}"
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 14));
        WorkflowView::new(kit()).render(buf.area, &mut buf, &ctx_with(&ascii, &state));
        assert!(crate::stability::check_with(&buf, 2).is_empty(), "{text}");
    }

    #[test]
    fn hidden_cli_internals_leave_out_subagents() {
        let mut state = AppState::default();
        let text = draw_with(&mut WorkflowView::new(kit()), 120, &theme(), &state);
        assert!(text.contains("+2 subagents"), "{text}");
        let mut ui = argonauts_core::settings::ui::UiSettings::default();
        ui.panel.subagents = CliInternals::Hidden;
        state.ui = Some(ui);
        let text = draw_with(&mut WorkflowView::new(kit()), 120, &theme(), &state);
        assert!(!text.contains("subagent"), "{text}");
    }

    /// `ui.workflow.row_cost` adds what a row cost after its tokens; without it no row shows
    /// spend.
    #[test]
    fn row_cost_is_an_opt_in() {
        let mut data = fixing();
        if let WorkflowRow::Agent(row) = &mut data.phases[1].rows[0] {
            row.spent.cost_usd = 0.12;
        }
        let mut state = going();
        let text = draw_with(&mut WorkflowView::new(data.clone()), 100, &theme(), &state);
        assert!(!text.contains("$0.12"), "{text}");
        let mut ui = argonauts_core::settings::ui::UiSettings::default();
        ui.workflow.row_cost = true;
        state.ui = Some(ui);
        let text = draw_with(&mut WorkflowView::new(data), 100, &theme(), &state);
        assert!(
            row_with(&text, "fix:slugify").contains("18.1k tok · $0.12"),
            "{text}"
        );
    }

    #[test]
    fn pause_acts_only_on_the_run_on_screen() {
        let mut view = WorkflowView::new(parent());
        let outcome = press(&mut view, KeyCode::Char('p'));
        assert!(
            matches!(&outcome, ViewOutcome::Notice(text) if text.starts_with("Only the run on screen")),
            "{outcome:?}"
        );
        let outcome = press_in(&mut view, KeyCode::Char('p'), &going());
        assert!(
            matches!(outcome, ViewOutcome::Send(UiCommand::Pause { now: false })),
            "{outcome:?}"
        );
    }

    #[test]
    fn the_shown_level_or_agent_is_what_refreshes() {
        let refresh = crate::views::follow::Refresh::default();
        let mut view = WorkflowView::new(parent()).following(refresh.follower());
        screen(&mut view);
        assert_eq!(
            view.shown(),
            Shown::Workflow {
                run: run(),
                id: WorkflowId::numbered(1),
                root: true,
            }
        );
        view.pending = Some(Pending::Workflow(WorkflowId::numbered(2)));
        view.accept(&ViewData::Workflow {
            view: Box::new(workflow(2, "build:c", Vec::new())),
        });
        assert_eq!(
            view.shown(),
            Shown::Workflow {
                run: run(),
                id: WorkflowId::numbered(2),
                root: false,
            }
        );
        // An agent's own workflow is asked for by its id.
        let mut agent_id = workflow(1, "probe", Vec::new());
        agent_id.id = argonauts_core::views::cli_workflow_run_id("w4d273qmn");
        let view = WorkflowView::new(agent_id);
        assert!(matches!(view.shown(), Shown::Workflow { root: false, .. }));
    }

    /// A workflow an agent's own CLI runs is drawn the same way, but only read: its agents
    /// open, and its CLI stops and saves it.
    #[test]
    fn a_agents_own_workflow_only_reads() {
        let mut data = fixing();
        data.id = argonauts_core::views::cli_workflow_run_id("w4d273qmn");
        let mut view = WorkflowView::new(data);
        let state = going();
        let keys = |view: &mut WorkflowView| {
            let text = draw_with(view, 100, &theme(), &state);
            text.lines().last().unwrap_or_default().trim().to_owned()
        };
        assert_eq!(
            keys(&mut view),
            "↑↓ select · enter opens its agents · esc back"
        );
        press_in(&mut view, KeyCode::Right, &state);
        assert_eq!(keys(&mut view), "↑↓ select · ⏎ open · esc back");
        press_in(&mut view, KeyCode::Left, &state);
        assert!(matches!(
            press_in(&mut view, KeyCode::Char('x'), &state),
            ViewOutcome::Notice(_)
        ));
        assert!(matches!(
            press_in(&mut view, KeyCode::Char('s'), &state),
            ViewOutcome::Notice(_)
        ));
    }

    /// After the finish, the view opens on the last phase whose agents ran.
    #[test]
    fn a_finished_workflow_opens_on_its_last_phase_with_agents() {
        let mut data = workflow(1, "council", Vec::new());
        let done = |session: &str| WorkflowRow::Agent(agent(session, "build:x", RowStatus::Done));
        data.phases = vec![
            phase("Understand", RowStatus::Done, vec![done("s-001")]),
            phase("Build", RowStatus::Done, vec![done("s-002"), done("s-003")]),
            phase("Merge", RowStatus::Done, Vec::new()),
        ];
        let view = WorkflowView::new(data.clone());
        assert_eq!(view.top().phase().map(|p| p.name.as_str()), Some("Build"));

        // While it runs, it opens where the work is, and a merge waiting on you says so.
        data.phases[2].status = RowStatus::NeedsYou;
        let mut view = WorkflowView::new(data.clone());
        assert_eq!(view.top().phase().map(|p| p.name.as_str()), Some("Merge"));
        let text = screen(&mut view);
        assert!(
            text.contains("Waiting for your answer in the conversation"),
            "{text}"
        );

        // With the run's question known, it names the question.
        data.phases[2].status = RowStatus::Running;
        let mut state = AppState::default();
        let mut run_state = RunState::new(run());
        run_state.approvals.push(crate::state::OpenApproval {
            opened: Box::new(asking("Merge this run into main?")),
            set_aside: false,
            replayed: false,
        });
        state.runs.insert(run(), run_state);
        let mut view = WorkflowView::new(data);
        let text = draw_with(&mut view, 100, &theme(), &state);
        assert!(
            text.contains("Waiting for your answer: Merge this run into main?"),
            "{text}"
        );
    }

    fn asking(title: &str) -> argonauts_core::events::approval::ApprovalOpened {
        use argonauts_core::events::approval::{
            ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText, QuestionPacket,
        };
        use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};

        ApprovalOpened {
            id: argonauts_core::names::ApprovalId::numbered(1),
            kind: ApprovalKind::Merge,
            title: title.into(),
            task: None,
            packet: ApprovalPacket::Question(QuestionPacket {
                from: "codex".parse().unwrap(),
                task: argonauts_core::TaskId::new("task-001").unwrap(),
                text: title.into(),
                options: Vec::new(),
            }),
            evidence_sha256: "0".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::None,
            needs_you: true,
        }
    }

    /// During Understand the view shows it, and the run's own workflow takes its place once the
    /// main agent picks it.
    #[test]
    fn the_runs_own_workflow_replaces_understand() {
        let mut understand = workflow(2, "understanding", Vec::new());
        understand.breadcrumb = vec!["understanding".into()];
        let mut view = WorkflowView::new(understand);
        let own = workflow(1, "council", Vec::new());
        assert!(view.accept_workflow(&own));
        assert_eq!(view.depth(), 1);
        assert_eq!(view.current().name, "council");
        // A nested workflow the view didn't ask for changes nothing.
        let mut nested = workflow(3, "review", Vec::new());
        nested.breadcrumb = vec!["council".into(), "review".into()];
        assert!(!view.accept_workflow(&nested));
        assert_eq!(view.current().name, "council");
    }

    /// The totals count the agents this workflow started and the run's time, as the panel
    /// row times it. Spend stays on the panel row.
    #[test]
    fn the_totals_count_the_workflows_agents_and_the_runs_time() {
        let theme = theme();
        let wall = OffsetDateTime::now_utc();
        let mut state = AppState::default();
        let mut run = RunState::new(run());
        run.started = Some(wall - time::Duration::seconds(125));
        run.ended = Some(wall);
        run.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        state.runs.insert(run.id.clone(), run);
        let mut view = WorkflowView::new(workflow(1, "council", Vec::new()));
        let text = draw_with(&mut view, 120, &theme, &state);
        assert!(text.contains("1/3 agents · 2m05s · done"), "{text}");
        assert!(!text.contains('$'), "{text}");

        // A nested level keeps its own numbers.
        let mut nested = workflow(2, "review", Vec::new());
        nested.breadcrumb = vec!["council".into(), "review".into()];
        view.pending = Some(Pending::Workflow(nested.id.clone()));
        assert!(view.accept_workflow(&nested));
        let text = draw_with(&mut view, 120, &theme, &state);
        assert!(text.contains("1/3 agents · 1m00s"), "{text}");
        // A nested workflow isn't the run, so the run's end isn't its own.
        assert!(!text.contains("· done"), "{text}");
    }
}
