//! The workflow view, as Claude Code's open workflow view: in the bottom region under the
//! conversation, the workflow's own name and description, its declared phases
//! numbered on the left, and the selected phase's agents on the right, each by its
//! `phase:target` label with its model, tokens, and time. Nested workflows are `◇` rows that
//! open their own view under a breadcrumb, and Enter on an agent shows its agent view in the
//! same frame.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::agent_goal::{AgentGoal, GoalKeeper};
use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::events::run::ParkReason;
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::settings::ui::VendorInternals;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::views::{
    AgentRow, PhaseRow, RowStatus, StatusLine, WorkflowRow, WorkflowView as WorkflowData,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::format;
use crate::state::AppState;
use crate::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::{Role, Theme};

use super::agent::{AgentView, needs_review, vendor_internals};
use super::dock::{self, Heading};
use super::follow::{Follower, Shown};
use super::goal::GoalView;
use super::goal_keys;
use super::goal_panel;
use super::layout::{cut, left, right};
use super::list::{self, Nav};
use super::panes::{self, Footer, Marks, Side};
use super::session::{SessionView, Tab};
use super::states;
use super::workflow_own::{self, Own};
use super::workflow_tries;
use super::{View, ViewCtx, ViewOutcome};

/// Phase names and row labels past this are cut, so one long name can't take the pane.
const MAX_PHASE_NAME: usize = 24;
const MAX_LABEL: usize = 40;

/// A narrow pane cuts the model to no less than this before cutting the label, and the label
/// to no less than [`CUT_LABEL`] before the model goes.
const CUT_MODEL: usize = 12;
const CUT_LABEL: usize = 12;

/// Rows the goal panel's box wants, borders included: the frame grows to give it these when
/// the usual frame is too short.
const GOAL_BOX_ROWS: u16 = 11;

/// The goal panel's box is this tall at the least, borders included, and at the most; the box
/// above it keeps at least [`AGENTS_BOX_ROWS`]. A frame too short for both shows the agents
/// alone.
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
    Agent(SessionKey),
    Workflow(WorkflowId),
    Goal,
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
}

impl WorkflowView {
    pub fn new(data: WorkflowData) -> Self {
        Self {
            levels: vec![Level::new(data)],
            agent: None,
            goal: None,
            pending: None,
            confirm: None,
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
            Some(agent) => Shown::Agent {
                run,
                session: agent.session().clone(),
            },
            None => Shown::Workflow {
                run,
                id: self.top().data.id.clone(),
                root: self.levels.len() == 1 && !is_vendor(&self.top().data.id),
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

    /// The name, a nested workflow's parents before it, the description, and the totals:
    /// the agents this workflow started, done of all, and its time, which for the run's own
    /// workflow is the run's, as its panel row times it. Spend stays on the panel row.
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
                "{}/{} steps · {}",
                data.agents_done,
                data.agents_total,
                panes::elapsed(elapsed_ms)
            )
        } else {
            panes::elapsed(elapsed_ms)
        };
        let tokens = data.tokens();
        if tokens > 0 {
            totals.push_str(&format!(
                " · {} {} tokens",
                ctx.theme.glyphs.tokens,
                format::tokens(tokens)
            ));
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

    /// The run's own time, while the view is on the run's workflow.
    fn run_elapsed(&self, state: &AppState) -> Option<u64> {
        if self.levels.len() > 1 || is_vendor(&self.top().data.id) {
            return None;
        }
        let run = state.runs.get(self.run())?;
        let elapsed = run.totals(OffsetDateTime::now_utc()).elapsed?;
        Some(millis(elapsed))
    }

    /// Claude Code's keys, and only those that act now: stopping and pausing need the
    /// workflow going, and stopping an agent needs it running.
    fn footer(&self, ctx: &ViewCtx<'_>) -> Footer {
        if let Some(stop) = &self.confirm {
            let ask = match stop {
                Stop::Workflow => "x again to stop the workflow".to_owned(),
                Stop::Agent(_, label) => format!("x again to stop {label}"),
            };
            return Footer::hints(&[(&ask, 0), ("esc to keep", 0)]);
        }
        let level = self.top();
        // An agent's own workflow is its CLI's to stop and save, so only moving and leaving
        // act on it here.
        if is_vendor(&level.data.id) {
            return Footer::hints(&[("↑↓ select", 0), (panes::ESC_BACK, 0)]);
        }
        let going = level.running() && !finished(ctx.state, self.run());
        if !going {
            return Footer::hints(&[("↑↓ select", 0), (panes::ESC_BACK, 0), ("s save", 1)]);
        }
        let pause = if paused(ctx.state, self.run()) {
            "p resume"
        } else {
            "p pause"
        };
        match level.side {
            Side::Left if self.levels[0].data.style == Some(RunStyle::Goal) => Footer::hints(&[
                ("↑↓ select", 0),
                ("tab goal", 4),
                ("x stop goal", 4),
                (pause, 3),
                (panes::ESC_BACK, 0),
                ("s save", 1),
            ]),
            Side::Left => Footer::hints(&[
                ("↑↓ select", 0),
                ("x stop workflow", 4),
                (pause, 3),
                (panes::ESC_BACK, 0),
                ("s save", 1),
            ]),
            Side::Right => {
                let row = level.row();
                let running = row.is_some_and(|row| {
                    matches!(row, WorkflowRow::Agent(agent) if agent.status == RowStatus::Running)
                });
                let mut hints = vec![("↑↓ agent", 0)];
                if !matches!(row, Some(WorkflowRow::Command(_)) | None) {
                    hints.push(("enter open it", 6));
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
                if self.message_target(ctx).is_ok() {
                    hints.push(("m message it", 4));
                }
                // Claude Code's view always offers stopping, so a narrow screen gives up
                // messaging first.
                if running {
                    hints.push(("x stop it", 5));
                }
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
                let session = agent.session.clone();
                self.pending = Some(Pending::Agent(session.clone()));
                ViewOutcome::Request(ViewRequest::Agent { run, session })
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

    /// The session `m` would message: the selected agent's, when the screen holds it and it
    /// can still take a message. Else why not, in plain words.
    fn message_target(&self, ctx: &ViewCtx<'_>) -> Result<SessionKey, String> {
        let level = self.top();
        let agent = match level.row().filter(|_| level.side == Side::Right) {
            Some(WorkflowRow::Agent(agent)) if !is_vendor(&level.data.id) => agent,
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

    /// Whether the run is one session's CLI running its own workflow, drawn as that CLI draws
    /// it: every step in one list, with the phases beside them.
    fn own_frame(&self) -> bool {
        self.levels.len() == 1 && self.top().data.owner.is_some()
    }

    /// ↑↓ over every step of the workflow, across its phases. The first press picks the first
    /// step going down, or the last going up.
    fn pick(&mut self, nav: Nav) {
        let level = self.top_mut();
        let all = workflow_own::steps(&level.data);
        if all.is_empty() {
            return;
        }
        let next = match level.side {
            Side::Left => match nav {
                Nav::By(by) if by < 0 => all.len() - 1,
                Nav::Bottom => all.len() - 1,
                _ => 0,
            },
            Side::Right => {
                let at = all
                    .iter()
                    .position(|step| *step == (level.phase, level.row))
                    .unwrap_or(0);
                step(at, all.len(), nav)
            }
        };
        (level.phase, level.row) = all[next];
        level.side = Side::Right;
    }

    fn render_own(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let footer = match self.confirm {
            Some(_) => self.footer(ctx),
            None => {
                let going = self.top().running() && !finished(ctx.state, self.run());
                workflow_own::footer(going, paused(ctx.state, self.run()))
            }
        };
        let level = self.top();
        let own = Own {
            data: &level.data,
            picked: (level.side == Side::Right).then_some((level.phase, level.row)),
            elapsed_ms: self
                .run_elapsed(ctx.state)
                .unwrap_or_else(|| level.data.elapsed_ms + level.ticking(ctx.now)),
            ticking: level.ticking(ctx.now),
        };
        workflow_own::draw(buf, area, &own, footer, ctx);
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
        if is_vendor(&self.top().data.id) {
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
            _ if self.own_frame() => Some(Stop::Workflow),
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
            ViewOutcome::Request(request @ ViewRequest::Agent { .. }) => {
                if let ViewRequest::Agent { session, .. } = &request {
                    self.pending = Some(Pending::Agent(session.clone()));
                }
                ViewOutcome::Request(request)
            }
            ViewOutcome::Ignored if key.code == KeyCode::Char('s') => self.save(),
            other => other,
        })
    }

    /// `s` saves the workflow as it is now, so it runs again by name.
    fn save(&self) -> ViewOutcome {
        let level = self.top();
        if is_vendor(&level.data.id) {
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
        let session = &view.row.session;
        if let Some(agent) = self.agent.as_mut()
            && agent.show(view.clone())
        {
            self.pending = None;
            return true;
        }
        if self.pending != Some(Pending::Agent(session.clone())) {
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
        // Before the lead picks the run's workflow, the view opens on the goal's alignment; the
        // run's own workflow takes its place once it starts.
        if self.levels.len() == 1
            && self.agent.is_none()
            && self.pending.is_none()
            && view.breadcrumb.is_empty()
            && !is_vendor(&self.levels[0].data.id)
            && !is_vendor(&view.id)
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

/// A workflow an agent's own CLI runs, which the views name apart from Argonauts' own.
fn is_vendor(id: &WorkflowId) -> bool {
    id.as_str().starts_with("vendor-")
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
        .is_some_and(|run| run.pausing || matches!(run.parked, Some(ParkReason::Paused)))
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

    /// Taller when the workflow has goals and the usual frame is too short for the goal panel
    /// under the agents. It goes by the workflow and not by the row selected, so the frame
    /// never jumps as you move.
    fn dock_height(&mut self, screen: ratatui::layout::Size, _ctx: &ViewCtx<'_>) -> Option<u16> {
        let data = &self.top().data;
        if self.agent.is_none() && self.goal.is_none() && self.own_frame() {
            return Some(workflow_own::rows(data).min(dock::height(screen.height)));
        }
        if self.agent.is_some() || self.goal.is_some() || data.goals.is_empty() {
            return None;
        }
        // The rule, the name, the description, a blank row, and the footer, around the boxes.
        let around = 5 + status_lines(data).len() as u16;
        let wanted = around + agents_box_rows(data) + GOAL_BOX_ROWS;
        Some(dock::height(screen.height).max(wanted))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        self.top_mut().seen_at.get_or_insert(ctx.now);
        if let Some(follow) = &self.follow {
            follow.show(self.shown());
        }
        let heading = self.heading(ctx);
        let footer = self.footer(ctx);
        if let Some(goal) = self.goal.as_mut() {
            goal.render(area, buf, ctx);
            return;
        }
        let status = status_lines(&self.top().data);
        if let Some(agent) = self.agent.as_mut() {
            agent.render_frame(area, buf, ctx, &heading, &status);
            return;
        }
        if self.own_frame() {
            self.render_own(area, buf, ctx);
            return;
        }
        let level = self.top();
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let phases = PhaseColumns::fit(&level.data, theme);
        let mut frame = dock::split(area, phases.width() as u16, status.len() as u16);
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
        let phase_lines = phases.lines(level, usize::from(panes.left.width), theme);
        let skip = (level.phase + 1).saturating_sub(usize::from(panes.left.height));
        panes::put_lines(buf, panes.left, &phase_lines[skip.min(phase_lines.len())..]);

        let shown = level.rows();
        let right_title = level.phase().map_or_else(String::new, |phase| {
            let name = sanitize_line(&phase.name).trim().to_owned();
            let agents = panes::count(steps_in_phase(phase), "step");
            let mut title = match (level.filter.word(), phase.attempt) {
                (Some(filter), _) => format!("{name} · {filter} · {agents}"),
                (None, Some(attempt)) => format!("{name} · attempt {attempt} · {agents}"),
                (None, None) => format!("{name} · {agents}"),
            };
            if level.data.requires(phase) {
                title.push_str(&format!(" · {} a step you require", marks.required));
            }
            if let Some(note) = phase_goals_note(level, phase) {
                title.push_str(&format!(" · {note}"));
            }
            title
        });
        // Who does the phase's work, as the workflow itself put it.
        let title_note = level
            .phase()
            .filter(|phase| !phase.rows.is_empty())
            .map(|phase| phase.detail.clone())
            .unwrap_or_default();
        let height = usize::from(panes.right.height);
        let mut selected_line = level.row;
        let lines: Vec<Line<'static>> = match level.phase() {
            Some(_) if !shown.is_empty() => {
                let cells: Vec<Cells> = shown
                    .iter()
                    .map(|row| Cells::of(row, level.goal_of(row), level.ticking(ctx.now), ctx))
                    .collect();
                let cells = with_pages(cells, &shown, &level.data.run, ctx);
                let rows = RowColumns::fit(&cells, usize::from(panes.right.width), ctx);
                let mut lines = Vec::new();
                for (i, (cells, row)) in cells.iter().zip(&shown).enumerate() {
                    let selected = level.side == Side::Right && i == level.row;
                    if i == level.row {
                        selected_line = lines.len();
                    }
                    lines.push(rows.line(cells, selected, theme));
                    if let WorkflowRow::Agent(agent) = row {
                        let unfolded = level.is_unfolded(&agent.session);
                        lines.extend(workflow_tries::lines(
                            agent,
                            selected,
                            unfolded,
                            &ctx.state.names,
                            theme,
                        ));
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
        let skip = (selected_line + 1).saturating_sub(height);
        let mark = dock::position(skip, height, lines.len(), theme);
        dock::draw_box(buf, &frame, ["Phases", &right_title], &mark, theme);
        dock::draw_title_note(buf, &frame, &title_note, str_width(&right_title), theme);
        panes::put_lines(buf, panes.right, &lines[skip.min(lines.len())..]);
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
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => {
                let nav = match key.code {
                    KeyCode::Up => Nav::By(-1),
                    KeyCode::Down => Nav::By(1),
                    KeyCode::Home => Nav::Top,
                    _ => Nav::Bottom,
                };
                if self.own_frame() {
                    self.pick(nav);
                } else {
                    self.move_selection(nav);
                }
                ViewOutcome::Consumed
            }
            KeyCode::Left if self.top().side == Side::Right && self.top_mut().fold_tries(false) => {
                ViewOutcome::Consumed
            }
            KeyCode::Right if self.top().side == Side::Right && self.top_mut().fold_tries(true) => {
                ViewOutcome::Consumed
            }
            KeyCode::Left | KeyCode::Esc => self.back(),
            KeyCode::Right | KeyCode::Enter if self.own_frame() => {
                if self.top().side == Side::Left {
                    self.pick(Nav::Top);
                }
                self.open_row()
            }
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
            KeyCode::Char('f') if plain && !self.own_frame() => {
                let level = self.top_mut();
                level.filter = level.filter.next();
                level.row = 0;
                ViewOutcome::Consumed
            }
            KeyCode::Char('s') if plain => self.save(),
            KeyCode::Char('g') if plain && !self.top().data.goals.is_empty() => {
                self.open_goal(ctx, false)
            }
            KeyCode::Char('e') if plain && self.editable_goal().is_some() => {
                self.open_goal(ctx, true)
            }
            KeyCode::Char('m') if plain && self.top().side == Side::Right => self.message(ctx),
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
        level.seen_at.filter(|_| level.running())?;
        let shown = level.data.elapsed_ms + level.ticking(now);
        Some(now + Duration::from_millis(1000 - shown % 1000))
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
        role
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
            GoalKeeper::Argonauts => format!("{who_words} on goals kept by Argonauts"),
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
        Span::styled(theme.glyphs.card, theme.style(Role::Card)),
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

/// The status lines the header carries, at most two.
fn status_lines(data: &WorkflowData) -> Vec<StatusLine> {
    data.status_lines
        .iter()
        .take(dock::MAX_STATUS)
        .cloned()
        .collect()
}

/// What a phase with no agents says: `Not started yet`, as Claude Code's does, or the card it
/// waits on you for, which only Argonauts has.
fn empty_phase(phase: &PhaseRow, state: &AppState, run: &RunId) -> String {
    let asks = state.runs.get(run).and_then(|run| {
        run.checkpoints
            .iter()
            .rev()
            .find(|open| open.opened.needs_you)
    });
    match (phase.status, asks) {
        (RowStatus::Running | RowStatus::NeedsYou, Some(open)) => format!(
            "Waiting for your answer: {}",
            sanitize_line(&open.opened.title).trim()
        ),
        (RowStatus::NeedsYou, None) => "Waiting for your answer in the chat".to_owned(),
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

/// The Phases pane's columns: the cursor, the number or mark, the mark of a step you require
/// when the workflow has one, the name, and `done/started`.
struct PhaseColumns {
    mark: usize,
    /// Cells for `★ ` before the name, or none when you require no step here.
    required: usize,
    name: usize,
    count: usize,
}

impl PhaseColumns {
    fn fit(data: &WorkflowData, theme: &Theme) -> Self {
        let marks = Marks::for_theme(theme);
        let phases = &data.phases;
        let digits = phases.len().to_string().len();
        let required = if phases.iter().any(|phase| data.requires(phase)) {
            str_width(marks.required) + 1
        } else {
            0
        };
        // A state's mark takes the number's place, and every one of them is one cell.
        let mark = 1;
        Self {
            mark: mark.max(digits),
            required,
            name: phases
                .iter()
                .map(|phase| str_width(sanitize_line(&phase.name).trim()))
                .max()
                .unwrap_or(0)
                .min(MAX_PHASE_NAME),
            count: phases
                .iter()
                .map(|phase| phase_count(phase).len())
                .max()
                .unwrap_or(0),
        }
    }

    fn width(&self) -> usize {
        let count = if self.count > 0 { 1 + self.count } else { 0 };
        2 + self.mark + 1 + self.required + self.name + count
    }

    /// Every phase numbered from 1 in subtle grey, a finished one a green `✔` in its place, and
    /// the selected one in the accent color, whole, with `❯` while the phases have the keys.
    /// A phase still to come is dim, and one that has started reads in the text color.
    fn lines(&self, level: &Level, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let accent = theme.style(Role::Card);
        let subtle = theme.style(Role::Faint);
        // A narrow pane cuts the names before the counts, and drops the counts last.
        let fixed = 2 + self.mark + 1 + self.required + 1 + self.count;
        let name_width = self.name.min(width.saturating_sub(fixed));
        let show_count = self.count > 0 && width > fixed;
        level
            .data
            .phases
            .iter()
            .enumerate()
            .map(|(i, phase)| {
                let selected = i == level.phase;
                let pick = |style: Style| if selected { accent } else { style };
                let (mark, mark_style) = match phase.status {
                    RowStatus::Done | RowStatus::NeedsYou | RowStatus::Failed => {
                        let (glyph, role) = states::mark(states::of_row(phase.status), theme);
                        (glyph.to_owned(), theme.style(role))
                    }
                    _ => ((i + 1).to_string(), subtle),
                };
                let name_style = if phase.status == RowStatus::Waiting {
                    theme.style(Role::Dim)
                } else {
                    theme.style(Role::Text)
                };
                let cursor = if selected && level.side == Side::Left {
                    Span::styled(format!("{} ", marks.selected), accent)
                } else {
                    Span::raw("  ")
                };
                let mut spans = vec![
                    cursor,
                    Span::styled(left(&mark, self.mark), pick(mark_style)),
                    Span::raw(" "),
                ];
                if self.required > 0 {
                    let star = if level.data.requires(phase) {
                        marks.required
                    } else {
                        ""
                    };
                    spans.push(Span::styled(
                        left(star, self.required),
                        theme.style(Role::Accent),
                    ));
                }
                spans.push(Span::styled(
                    left(sanitize_line(&phase.name).trim(), name_width),
                    pick(name_style),
                ));
                if show_count {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        left(&phase_count(phase), self.count),
                        pick(subtle),
                    ));
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// One row's cells, before they're laid out.
struct Cells {
    glyph: &'static str,
    glyph_role: Role,
    status: RowStatus,
    label: String,
    /// The model, a command such as `pytest`, or `workflow`.
    model: String,
    /// The model with the CLI it runs on, such as `GPT 5.6 on Codex`, for a pane with room.
    model_on_cli: String,
    /// Tokens, a command's result, a nested workflow's agents, or where a goal stands.
    detail: String,
    /// Where a goal stands in fewer words, for a pane too narrow for `detail`. The detail
    /// itself for a row with no goal.
    detail_brief: String,
    /// A square per line of the agent's goal, before the detail. Empty without a goal.
    squares: Vec<Span<'static>>,
    markers: Vec<Span<'static>>,
    cost: String,
    /// Left out when it's unknown, and `queued` for an agent waiting for a slot.
    time: String,
}

impl Cells {
    fn of(row: &WorkflowRow, goal: Option<&AgentGoal>, ticking: u64, ctx: &ViewCtx<'_>) -> Self {
        let marks = Marks::for_theme(ctx.theme);
        let names = &ctx.state.names;
        let (glyph, glyph_role, status, elapsed_ms) = match row {
            WorkflowRow::Agent(agent) => {
                let (glyph, role) = states::mark(states::of_row(agent.status), ctx.theme);
                (glyph, role, agent.status, agent.elapsed_ms)
            }
            WorkflowRow::Workflow(nested) => {
                (marks.nested, Role::Card, nested.status, nested.elapsed_ms)
            }
            WorkflowRow::Command(command) => {
                let (glyph, role) = states::mark(states::of_row(command.status), ctx.theme);
                (glyph, role, command.status, command.elapsed_ms)
            }
        };
        let time = match status {
            RowStatus::Queued => "queued".to_owned(),
            RowStatus::Waiting => String::new(),
            RowStatus::Running => panes::elapsed(elapsed_ms + ticking),
            _ => panes::elapsed(elapsed_ms),
        };
        let named = |agent: &AgentRow, name: String| {
            let name = sanitize_line(&name).into_owned();
            if agent.standing_in_for.is_some() {
                format!("{} {name}", marks.stands_in)
            } else {
                name
            }
        };
        let (model, detail, markers, cost) = match row {
            WorkflowRow::Agent(agent) => {
                let model = named(agent, names.short(&agent.worker));
                // An agent on a goal says where the goal stands, where its tokens would be.
                let tokens = match goal {
                    Some(goal) => goal_panel::row_words(goal),
                    None if agent.tokens > 0 => panes::tokens(agent.tokens),
                    None => String::new(),
                };
                let tokens = answered(agent, tokens);
                let cost = if row_cost(ctx) {
                    panes::spend(&agent.spent).unwrap_or_default()
                } else {
                    String::new()
                };
                (model, tokens, agent_markers(agent, ctx), cost)
            }
            WorkflowRow::Workflow(nested) => (
                nested.owner.as_deref().map_or_else(
                    || "workflow".to_owned(),
                    |owner| format!("{}'s own", names.harness(owner)),
                ),
                format!(
                    "{}/{} {}",
                    nested.agents_done,
                    nested.agents_total,
                    if nested.owner.is_some() {
                        "agents"
                    } else {
                        "steps"
                    }
                ),
                Vec::new(),
                String::new(),
            ),
            WorkflowRow::Command(command) => (
                sanitize_line(&command.command).into_owned(),
                command
                    .result
                    .as_deref()
                    .map(|result| sanitize_line(result).into_owned())
                    .unwrap_or_default(),
                Vec::new(),
                String::new(),
            ),
        };
        Self {
            glyph,
            glyph_role,
            status,
            detail_brief: match goal {
                Some(goal) => goal_panel::row_words_brief(goal),
                None => detail.clone(),
            },
            // A workflow an agent's own CLI runs hangs under that agent's row.
            label: match row {
                WorkflowRow::Workflow(nested) if nested.parent.is_some() => {
                    format!("{} {}", ctx.theme.glyphs.result, row_label(row))
                }
                _ => row_label(row),
            },
            // Only an agent's model runs on a CLI: a command or a nested workflow reads the
            // same either way.
            model_on_cli: match row {
                WorkflowRow::Agent(agent) => named(agent, names.worker(&agent.worker)),
                _ => model.clone(),
            },
            model,
            detail,
            squares: goal
                .map(|goal| goal_panel::squares(goal, ctx.theme))
                .unwrap_or_default(),
            markers,
            cost,
            time,
        }
    }

    /// What follows the model's ` · `: the squares and the detail, then the markers after
    /// another ` · `.
    fn rest_width(&self, markers: bool) -> usize {
        self.rest_width_of(&self.detail, markers)
    }

    fn rest_width_of(&self, detail: &str, markers: bool) -> usize {
        let squares = match spans_width(&self.squares) {
            0 => 0,
            cells => cells + 1,
        };
        let detail = squares + str_width(detail);
        let markers = if markers {
            spans_width(&self.markers)
        } else {
            0
        };
        match (detail, markers) {
            (0, 0) => 0,
            (d, 0) => d,
            (0, m) => m,
            (d, m) => d + 3 + m,
        }
    }
}

/// A step a role answered through Argonauts: `asked through Argonauts · 41.2k tok` while it
/// reads, then its verdict alone.
fn answered(agent: &AgentRow, tokens: String) -> String {
    let Some(words) = agent.answered.as_deref() else {
        return tokens;
    };
    let words = sanitize_line(words.trim()).into_owned();
    if agent.status == RowStatus::Running && !tokens.is_empty() {
        format!("{words} · {tokens}")
    } else {
        words
    }
}

fn row_cost(ctx: &ViewCtx<'_>) -> bool {
    ctx.state.ui.as_ref().is_some_and(|ui| ui.workflow.row_cost)
}

/// The right pane's columns, as Claude Code's rows: a cursor slot, the glyph, the label padded
/// to the longest in the phase, one space, the model padded to the widest, ` · `, the tokens,
/// and the time flush right. The model names its CLI too where every row has room for it. Then
/// room runs out from the right: markers go, a goal's words shorten, the tokens and then the
/// model go, and then the label is cut. The time always stays.
struct RowColumns {
    width: usize,
    slot: usize,
    label: usize,
    model: usize,
    /// Whether the model column names the CLI too.
    on_cli: bool,
    /// Whether a goal's words are the short ones.
    brief: bool,
    rest: bool,
    markers: bool,
    cost: usize,
    time: usize,
}

impl RowColumns {
    fn fit(cells: &[Cells], width: usize, ctx: &ViewCtx<'_>) -> Self {
        let marks = Marks::for_theme(ctx.theme);
        let widest = |cell: &dyn Fn(&Cells) -> usize| cells.iter().map(cell).max().unwrap_or(0);
        let slot = marks.slot();
        let time = widest(&|c| str_width(&c.time));
        let cost = widest(&|c| str_width(&c.cost));
        let label = widest(&|c| str_width(&c.label)).min(MAX_LABEL);
        let model = widest(&|c| str_width(&c.model));
        let room = width
            .saturating_sub(1 + slot + 1)
            .saturating_sub(if time > 0 { 1 + time } else { 0 })
            .saturating_sub(if cost > 0 { 1 + cost } else { 0 });
        let need = |label: usize, model: usize, rest: usize| {
            label
                + if model > 0 { 1 + model } else { 0 }
                + if rest > 0 && model > 0 { 3 + rest } else { 0 }
        };
        let mut fitted = Self {
            width,
            slot,
            label,
            model,
            on_cli: false,
            brief: false,
            rest: true,
            markers: true,
            cost,
            time,
        };
        let with_markers = widest(&|c| c.rest_width(true));
        let without = widest(&|c| c.rest_width(false));
        // The CLI's name is the first to go: the model says more, and the opened agent names
        // both.
        let on_cli = widest(&|c| str_width(&c.model_on_cli));
        if need(label, on_cli, with_markers) <= room {
            fitted.model = on_cli;
            fitted.on_cli = true;
            return fitted;
        }
        if need(label, model, with_markers) <= room {
            return fitted;
        }
        fitted.markers = false;
        if need(label, model, without) <= room {
            return fitted;
        }
        // A goal's row keeps its squares with fewer words before it says nothing of its goal.
        let brief = widest(&|c| c.rest_width_of(&c.detail_brief, false));
        if need(label, model, brief) <= room {
            fitted.brief = true;
            return fitted;
        }
        fitted.rest = false;
        if need(label, model, 0) <= room {
            return fitted;
        }
        // The model is cut before it goes, and the label before the model does, so each row
        // still says whose model ran.
        let model_room = room.saturating_sub(label + 1);
        if model_room >= CUT_MODEL.min(model) {
            fitted.model = model_room.min(model);
            return fitted;
        }
        let label_room = room.saturating_sub(1 + CUT_MODEL.min(model));
        if label_room >= CUT_LABEL.min(label) {
            fitted.label = label_room.min(label);
            fitted.model = room.saturating_sub(fitted.label + 1).min(model);
            return fitted;
        }
        fitted.model = 0;
        fitted.label = label.min(room);
        fitted
    }

    fn line(&self, cells: &Cells, selected: bool, theme: &Theme) -> Line<'static> {
        let marks = Marks::for_theme(theme);
        let accent = theme.style(Role::Card);
        let dim = theme.style(Role::Dim);
        let mut spans = vec![
            if selected {
                Span::styled(marks.selected.to_owned(), accent)
            } else {
                Span::raw(" ")
            },
            Span::styled(
                panes::pad_glyph(cells.glyph, self.slot),
                theme.style(cells.glyph_role),
            ),
            Span::raw(" "),
        ];
        let label_style = if selected {
            accent
        } else {
            match cells.status {
                RowStatus::Done | RowStatus::Failed | RowStatus::NeedsYou => {
                    theme.style(Role::Text)
                }
                _ => dim,
            }
        };
        let label = if self.model > 0 {
            left(&cells.label, self.label)
        } else {
            cut(&cells.label, self.label)
        };
        spans.push(Span::styled(label, label_style));
        if self.model > 0 {
            let model_style = if selected { accent } else { dim };
            let detail = if self.brief {
                &cells.detail_brief
            } else {
                &cells.detail
            };
            let rest = if self.rest {
                cells.rest_width_of(detail, self.markers)
            } else {
                0
            };
            let name = if self.on_cli {
                &cells.model_on_cli
            } else {
                &cells.model
            };
            let model = if rest > 0 {
                left(name, self.model)
            } else {
                cut(name, self.model)
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(model, model_style));
            if rest > 0 && cells.squares.is_empty() {
                spans.push(Span::styled(" · ", dim));
                if !detail.is_empty() {
                    spans.push(Span::styled(detail.clone(), dim));
                }
            } else if rest > 0 {
                // The squares stand where the ` · ` would, three cells in, so both kinds of
                // row keep one column.
                spans.push(Span::raw("   "));
                spans.extend(cells.squares.iter().cloned());
                let words = match (selected, cells.status) {
                    (true, _) => accent.add_modifier(Modifier::BOLD),
                    (false, RowStatus::Done) => dim,
                    (false, _) => theme.style(Role::Text),
                };
                spans.push(Span::styled(format!(" {detail}"), words));
            }
            if rest > 0 && self.markers && !cells.markers.is_empty() {
                if !detail.is_empty() {
                    spans.push(Span::styled(" · ", dim));
                }
                spans.extend(cells.markers.iter().cloned());
            }
        }
        let mut edge = Vec::new();
        if self.cost > 0 {
            edge.push(Span::styled(
                format!(" {}", right(&cells.cost, self.cost)),
                dim,
            ));
        }
        if self.time > 0 {
            edge.push(Span::styled(
                format!(" {}", right(&cells.time, self.time)),
                dim,
            ));
        }
        let room = self.width.saturating_sub(spans_width(&edge));
        let mut spans = truncate_spans(&spans, room);
        let used = spans_width(&spans);
        spans.push(Span::raw(" ".repeat(room.saturating_sub(used))));
        spans.extend(edge);
        Line::from(spans)
    }
}

/// A phase's `done/started`, or nothing before it has agents, so no phase reads `0/0`.
fn phase_count(phase: &PhaseRow) -> String {
    phase
        .count
        .map(|count| format!("{}/{}", count.done, count.total))
        .unwrap_or_default()
}

/// Every step at work in a phase, a nested workflow's included, so the workflow view and the
/// agent view title the phase alike. A step tried more than once is one, and commands don't
/// count, as in Claude Code.
pub(super) fn steps_in_phase(phase: &PhaseRow) -> usize {
    phase
        .rows
        .iter()
        .map(|row| match row {
            WorkflowRow::Agent(_) => 1,
            WorkflowRow::Workflow(nested) if nested.parent.is_some() => 0,
            WorkflowRow::Workflow(nested) => nested.agents_total as usize,
            WorkflowRow::Command(_) => 0,
        })
        .sum()
}

fn row_status(row: &WorkflowRow) -> RowStatus {
    match row {
        WorkflowRow::Agent(agent) => agent.status,
        WorkflowRow::Workflow(nested) => nested.status,
        WorkflowRow::Command(command) => command.status,
    }
}

fn row_label(row: &WorkflowRow) -> String {
    let label = match row {
        WorkflowRow::Agent(agent) => &agent.label,
        WorkflowRow::Workflow(nested) => &nested.name,
        WorkflowRow::Command(command) => &command.label,
    };
    sanitize_line(label).trim().to_owned()
}

/// What only Argonauts marks on an agent: review debt and the vendor's own subagents. A
/// handoff's `↷` goes before the model instead.
fn agent_markers(agent: &AgentRow, ctx: &ViewCtx<'_>) -> Vec<Span<'static>> {
    let theme = ctx.theme;
    let marks = Marks::for_theme(theme);
    let dim = theme.style(Role::Dim);
    let mut markers = Vec::new();
    let mut push = |text: String, role: Role| {
        if !markers.is_empty() {
            markers.push(Span::styled(" · ", dim));
        }
        markers.push(Span::styled(text, theme.style(role)));
    };
    if needs_review(ctx, &agent.session) {
        push(format!("{} needs review", marks.debt), Role::Warning);
    }
    // `ui.panel.vendor_internals = hidden` leaves out what the vendor runs inside an agent.
    if agent.subagents > 0 && vendor_internals(ctx) != VendorInternals::Hidden {
        let noun = if agent.subagents == 1 {
            "subagent"
        } else {
            "subagents"
        };
        push(format!("+{} {noun}", agent.subagents), Role::Dim);
    }
    markers
}

/// The page an agent published goes with its markers, named as on its row in the list
///: `⧉ test-report`.
fn with_pages(
    mut cells: Vec<Cells>,
    rows: &[&WorkflowRow],
    run: &RunId,
    ctx: &ViewCtx<'_>,
) -> Vec<Cells> {
    let dim = ctx.theme.style(Role::Dim);
    let mark = Marks::for_theme(ctx.theme).artifact;
    for (cells, row) in cells.iter_mut().zip(rows) {
        let WorkflowRow::Agent(agent) = row else {
            continue;
        };
        let page = ctx.state.artifacts.by_agent(run, &agent.session);
        let Some(name) = page.map(|page| sanitize_line(&page.name).trim().to_owned()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if !cells.markers.is_empty() {
            cells.markers.push(Span::styled(" · ", dim));
        }
        cells
            .markers
            .push(Span::styled(format!("{mark} {name}"), dim));
    }
    cells
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::views::{
        AgentView as AgentData, NestedWorkflowRow, PhaseCount, StatusKind,
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
            standing_in_for: None,
            answered: None,
            tries: Vec::new(),
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
            attempt: None,
            count: PhaseCount::of(done, rows.len() as u32),
            rows,
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
            owner: None,
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
                    owner: None,
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

    /// A review Claude Code's workflow asked Astra for through Argonauts says so
    /// beside Astra's name while Astra reads, and once answered says what Astra decided.
    #[test]
    fn a_step_asked_through_argonauts_says_so() {
        let mut store = agent("s-002", "review:store", RowStatus::Running);
        store.worker = "astra".parse().unwrap();
        store.tokens = 41_200;
        store.answered = Some("asked through Argonauts".into());
        let mut cli = agent("s-003", "review:cli", RowStatus::Done);
        cli.worker = "astra".parse().unwrap();
        cli.answered = Some("accepted".into());
        let mut data = workflow(1, "add-due-dates", Vec::new());
        data.phases = vec![phase(
            "Review",
            RowStatus::Running,
            vec![WorkflowRow::Agent(store), WorkflowRow::Agent(cli)],
        )];
        let mut view = WorkflowView::new(data);
        let text = draw_with(&mut view, 100, &theme(), &going());
        assert!(
            row_with(&text, "review:store").contains("astra · asked through Argonauts · 41.2k tok"),
            "{text}"
        );
        let cli = row_with(&text, "review:cli");
        assert!(cli.contains("astra · accepted"), "{text}");
        assert!(!cli.contains("tok"), "{text}");
    }

    /// The open view as Claude Code's: a rule six columns short, the name
    /// and the description with its totals inside it, numbered phases with counts only once
    /// started, `phase:target` rows with the model and tokens, and the time flush right.
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
            lines[2]
                .trim_end()
                .ends_with("6/7 steps · 4m31s · ↓ 2M tokens"),
            "{text}"
        );
        assert_eq!(lines[2].trim_end().chars().count(), 93, "{text}");
        assert!(lines[3].trim().is_empty(), "{text}");
        assert!(
            lines[4].starts_with(" ╭ Phases ────────────┬ Fix · 2 steps ─"),
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
            slugify.contains("│  ✔ fix:slugify Haiku 4.5 on Claude Code · 18.1k tok"),
            "{text}"
        );
        assert!(slugify.trim_end().ends_with("2m10s │"), "{text}");
        assert!(
            row_with(&text, "fix:median")
                .contains("◐ fix:median  GPT 5.6 Luna on Codex    · 31.2k tok"),
            "{text}"
        );
        assert!(!text.contains("0/0"), "{text}");
        assert!(!text.contains("task-"), "{text}");
        assert!(!text.contains('$'), "{text}");
        let footer = lines.last().unwrap_or(&"").trim_end();
        assert_eq!(
            footer,
            " ↑↓ select · x stop workflow · p pause · esc back · s save"
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

    /// Every row of a pane starts its model, puts its dot, and ends its time in the same
    /// cells, with long and CJK labels and a crew from three CLIs, at every width the
    /// snapshots use.
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
            let rows: Vec<&str> = ["fix:slugify", "fix:median", "修复:导出测试"]
                .iter()
                .map(|label| row_with(&text, label))
                .collect();
            let dots: Vec<Option<usize>> = rows.iter().map(|row| cell_of(row, " · ")).collect();
            assert!(dots.iter().all(|dot| *dot == dots[0]), "{width}: {text}");
            // The time ends one cell before the right border.
            let ends: Vec<Option<usize>> = rows
                .iter()
                .map(|row| {
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

    /// A row names the CLI its model runs on where every row of the pane has room for it. On
    /// a narrower pane the CLI's name is the first thing to go, before the tokens do.
    #[test]
    fn a_row_names_its_cli_where_there_is_room_and_the_model_alone_where_not() {
        let wide = draw_with(&mut WorkflowView::new(fixing()), 100, &theme(), &going());
        assert!(
            row_with(&wide, "fix:slugify").contains("Haiku 4.5 on Claude Code · 18.1k tok"),
            "{wide}"
        );
        assert!(
            row_with(&wide, "fix:median").contains("GPT 5.6 Luna on Codex    · 31.2k tok"),
            "{wide}"
        );
        let narrow = draw_with(&mut WorkflowView::new(fixing()), 80, &theme(), &going());
        assert!(
            row_with(&narrow, "fix:slugify").contains("Haiku 4.5    · 18.1k tok"),
            "{narrow}"
        );
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
        assert!(text.contains("┬ Review · 0 steps ─"), "{text}");
        assert!(
            row_with(&text, "Understand").contains("│ Not started yet"),
            "{text}"
        );
    }

    #[test]
    fn the_footer_names_only_the_keys_that_act() {
        let state = going();
        let mut view = WorkflowView::new(fixing());
        press_in(&mut view, KeyCode::Right, &state);
        // The first row is done, so there's nothing to stop.
        let footer = |view: &mut WorkflowView, state: &AppState| {
            let text = draw_with(view, 100, &theme(), state);
            text.lines().last().unwrap_or_default().trim().to_owned()
        };
        assert_eq!(
            footer(&mut view, &state),
            "↑↓ agent · enter open it · p pause · f filter · esc back · s save"
        );
        press_in(&mut view, KeyCode::Down, &state);
        assert_eq!(
            footer(&mut view, &state),
            "↑↓ agent · enter open it · x stop it · p pause · f filter · esc back · s save"
        );
        // Paused, p resumes.
        let mut paused = state.clone();
        if let Some(run) = paused.runs.get_mut(&run()) {
            run.parked = Some(ParkReason::Paused);
        }
        assert!(footer(&mut view, &paused).contains("p resume"));
        assert!(matches!(
            press_in(&mut view, KeyCode::Char('p'), &paused),
            ViewOutcome::Send(UiCommand::Resume { run: Some(_) })
        ));
        // Once the workflow ends, only reading is left.
        let mut done = fixing();
        for phase in &mut done.phases {
            phase.status = RowStatus::Done;
        }
        let mut view = WorkflowView::new(done);
        assert_eq!(footer(&mut view, &state), "↑↓ select · esc back · s save");
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
        press_in(&mut view, KeyCode::Char('f'), &state);
        let text = draw_with(&mut view, 100, &theme(), &state);
        assert!(text.contains("Fix · running · 2 steps"), "{text}");
        assert!(!text.contains("fix:slugify"), "{text}");
        assert!(text.contains("fix:median"), "{text}");
        for _ in 0..3 {
            press_in(&mut view, KeyCode::Char('f'), &state);
        }
        let text = draw_with(&mut view, 100, &theme(), &state);
        assert!(text.contains("Fix · 2 steps"), "{text}");
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
        data.status_lines = vec![StatusLine {
            kind: StatusKind::Note,
            text: "fix:slugify passed its tests, so review:slugify started".into(),
        }];
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
        assert!(text.contains("Build · 4 steps"), "{text}");
        assert!(text.contains("◐ Running"), "{text}");
        assert!(text.contains("Prompt · 1 line"), "{text}");

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
        // The model sits in one column, and its dot after the widest.
        let dots = ["build:a", "build:b", "build:d"].map(|label| {
            let row = row_with(&text, label);
            row[..row.find(" · ").unwrap_or_default()].chars().count()
        });
        assert!(dots.iter().all(|at| *at == dots[0]), "{text}");
        assert!(row_with(&text, "build:a").contains("Opus "), "{text}");
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 14));
        WorkflowView::new(kit()).render(buf.area, &mut buf, &ctx_with(&ascii, &state));
        assert!(crate::stability::check_with(&buf, 2).is_empty(), "{text}");
    }

    #[test]
    fn hidden_vendor_internals_leave_out_subagents() {
        let mut state = AppState::default();
        let text = draw_with(&mut WorkflowView::new(kit()), 120, &theme(), &state);
        assert!(text.contains("+2 subagents"), "{text}");
        let mut ui = argonauts_core::settings::ui::UiSettings::default();
        ui.panel.vendor_internals = VendorInternals::Hidden;
        state.ui = Some(ui);
        let text = draw_with(&mut WorkflowView::new(kit()), 120, &theme(), &state);
        assert!(!text.contains("subagent"), "{text}");
    }

    /// `ui.workflow.row_cost` adds a cost column before the time; without it no row shows
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
            row_with(&text, "fix:slugify").contains("$0.12 2m10s"),
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
        // A vendor's own workflow is asked for by its id.
        let mut vendor = workflow(1, "probe", Vec::new());
        vendor.id = argonauts_core::views::vendor_workflow_id("w4d273qmn");
        let view = WorkflowView::new(vendor);
        assert!(matches!(view.shown(), Shown::Workflow { root: false, .. }));
    }

    /// A workflow an agent's own CLI runs is drawn the same way, but only read: its CLI stops
    /// and saves it.
    #[test]
    fn a_vendors_own_workflow_only_reads() {
        let mut data = fixing();
        data.id = argonauts_core::views::vendor_workflow_id("w4d273qmn");
        let mut view = WorkflowView::new(data);
        let state = going();
        let text = draw_with(&mut view, 100, &theme(), &state);
        assert_eq!(
            text.lines().last().unwrap_or_default().trim(),
            "↑↓ select · esc back"
        );
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
            text.contains("Waiting for your answer in the chat"),
            "{text}"
        );

        // With the run's card known, it names the card.
        data.phases[2].status = RowStatus::Running;
        let mut state = AppState::default();
        let mut run_state = RunState::new(run());
        run_state.checkpoints.push(crate::state::OpenCheckpoint {
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

    fn asking(title: &str) -> argonauts_core::events::checkpoint::CheckpointOpened {
        use argonauts_core::events::checkpoint::{
            CheckpointKind, CheckpointOpened, CheckpointPacket, FreeText, QuestionPacket,
        };
        use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};

        CheckpointOpened {
            id: argonauts_core::names::CheckpointId::numbered(1),
            kind: CheckpointKind::Merge,
            title: title.into(),
            card: None,
            packet: CheckpointPacket::Question(QuestionPacket {
                from: "codex".parse().unwrap(),
                card: argonauts_core::CardId::new("task-001").unwrap(),
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

    /// In the goal phase the view shows the goal's alignment, and the run's own workflow takes
    /// its place once the lead picks it.
    #[test]
    fn the_runs_own_workflow_replaces_the_goal_alignment() {
        let mut alignment = workflow(2, "goal", Vec::new());
        alignment.breadcrumb = vec!["goal".into()];
        let mut view = WorkflowView::new(alignment);
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
        assert!(text.contains("1/3 steps · 2m05s"), "{text}");
        assert!(!text.contains('$'), "{text}");

        // A nested level keeps its own numbers.
        let mut nested = workflow(2, "review", Vec::new());
        nested.breadcrumb = vec!["council".into(), "review".into()];
        view.pending = Some(Pending::Workflow(nested.id.clone()));
        assert!(view.accept_workflow(&nested));
        let text = draw_with(&mut view, 120, &theme, &state);
        assert!(text.contains("1/3 steps · 1m00s"), "{text}");
    }
}
