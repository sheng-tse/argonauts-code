//! The workflow view, as Claude Code's open workflow view: in the bottom region under the
//! conversation, the workflow's own name and description, its declared phases
//! numbered on the left, and the selected phase's agents on the right, each by its
//! `phase:target` label with its model, tokens, and time. Nested workflows are `◇` rows that
//! open their own view under a breadcrumb, and Enter on an agent shows its agent view in the
//! same frame.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
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
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::state::AppState;
use crate::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::{Role, Theme};

use super::agent::{AgentView, needs_review, vendor_internals};
use super::dock::{self, Heading};
use super::follow::{Follower, Shown};
use super::goal::GoalView;
use super::layout::{cut, left, right};
use super::list::{self, Nav};
use super::panes::{self, Footer, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Phase names and row labels past this are cut, so one long name can't take the pane.
const MAX_PHASE_NAME: usize = 24;
const MAX_LABEL: usize = 40;

/// A narrow pane cuts the model to no less than this before cutting the label, and the label
/// to no less than [`CUT_LABEL`] before the model goes.
const CUT_MODEL: usize = 12;
const CUT_LABEL: usize = 12;

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
        Heading {
            names,
            description: data.description.clone(),
            totals: format!(
                "{}/{} agents · {}",
                data.agents_done,
                data.agents_total,
                panes::elapsed(elapsed_ms)
            ),
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
                let running = level.row().is_some_and(|row| {
                    matches!(row, WorkflowRow::Agent(agent) if agent.status == RowStatus::Running)
                });
                let mut hints = vec![("↑↓ select", 0)];
                if running {
                    hints.push(("x stop agent", 4));
                }
                hints.extend([
                    (pause, 3),
                    ("f filter", 2),
                    (panes::ESC_BACK, 0),
                    ("s save", 1),
                ]);
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
        self.agent = Some(agent);
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
        let level = self.top();
        let theme = ctx.theme;
        let phases = PhaseColumns::fit(&level.data.phases, theme);
        let frame = dock::split(area, phases.width() as u16, status.len() as u16);
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
            let agents = panes::count(agents_in_phase(phase), "agent");
            match (level.filter.word(), phase.attempt) {
                (Some(filter), _) => format!("{name} · {filter} · {agents}"),
                (None, Some(attempt)) => format!("{name} · attempt {attempt} · {agents}"),
                (None, None) => format!("{name} · {agents}"),
            }
        });
        let height = usize::from(panes.right.height);
        let lines: Vec<Line<'static>> = match level.phase() {
            Some(_) if !shown.is_empty() => {
                let cells: Vec<Cells> = shown
                    .iter()
                    .map(|row| Cells::of(row, level.ticking(ctx.now), ctx))
                    .collect();
                let rows = RowColumns::fit(&cells, usize::from(panes.right.width), ctx);
                cells
                    .iter()
                    .enumerate()
                    .map(|(i, cells)| {
                        rows.line(cells, level.side == Side::Right && i == level.row, theme)
                    })
                    .collect()
            }
            Some(_) if level.filter != Filter::All => {
                vec![dim_line("No agents match this filter", theme)]
            }
            Some(phase) => vec![dim_line(&empty_phase(phase, ctx.state, self.run()), theme)],
            None => vec![dim_line("No phases yet", theme)],
        };
        let skip = (level.row + 1).saturating_sub(height);
        let mark = dock::position(skip, height, lines.len(), theme);
        dock::draw_box(buf, &frame, ["Phases", &right_title], &mark, theme);
        panes::put_lines(buf, panes.right, &lines[skip.min(lines.len())..]);
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
                self.move_selection(nav);
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
                if paused(ctx.state, self.run()) {
                    ViewOutcome::Send(UiCommand::Resume {
                        run: Some(self.run().clone()),
                    })
                } else {
                    super::pause(self.run(), ctx)
                }
            }
            KeyCode::Char('f') if plain => {
                let level = self.top_mut();
                level.filter = level.filter.next();
                level.row = 0;
                ViewOutcome::Consumed
            }
            KeyCode::Char('s') if plain => self.save(),
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
        _ => "Not started yet".to_owned(),
    }
}

fn dim_line(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), theme.style(Role::Dim)))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The Phases pane's columns: the cursor, the number or mark, the name, and `done/started`.
struct PhaseColumns {
    mark: usize,
    name: usize,
    count: usize,
}

impl PhaseColumns {
    fn fit(phases: &[PhaseRow], theme: &Theme) -> Self {
        let marks = Marks::for_theme(theme);
        let digits = phases.len().to_string().len();
        let mark = [marks.finished, marks.needs_you, marks.failed]
            .iter()
            .map(|glyph| str_width(glyph))
            .max()
            .unwrap_or(1);
        Self {
            mark: mark.max(digits),
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
        2 + self.mark + 1 + self.name + count
    }

    /// Every phase numbered from 1 in subtle grey, a finished one a green `✔` in its place, and
    /// the selected one in the accent color, whole, with `❯` while the phases have the keys.
    /// A phase still to come is dim, and one that has started reads in the text color.
    fn lines(&self, level: &Level, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let accent = theme.style(Role::Card);
        let subtle = theme.style(Role::Faint);
        // A narrow pane cuts the names before the counts, and drops the counts last.
        let fixed = 2 + self.mark + 1 + 1 + self.count;
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
                    RowStatus::Done => (marks.finished.to_owned(), theme.style(Role::Success)),
                    RowStatus::NeedsYou => (marks.needs_you.to_owned(), theme.style(Role::Card)),
                    RowStatus::Failed => (marks.failed.to_owned(), theme.style(Role::Error)),
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
                    Span::styled(
                        left(sanitize_line(&phase.name).trim(), name_width),
                        pick(name_style),
                    ),
                ];
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
    /// Tokens, a command's result, or a nested workflow's agents.
    detail: String,
    markers: Vec<Span<'static>>,
    cost: String,
    /// Left out when it's unknown, and `queued` for an agent waiting for a slot.
    time: String,
}

impl Cells {
    fn of(row: &WorkflowRow, ticking: u64, ctx: &ViewCtx<'_>) -> Self {
        let marks = Marks::for_theme(ctx.theme);
        let names = &ctx.state.names;
        let (glyph, glyph_role, status, elapsed_ms) = match row {
            WorkflowRow::Agent(agent) => {
                let (glyph, role) = row_glyph(&marks, agent.status);
                (glyph, role, agent.status, agent.elapsed_ms)
            }
            WorkflowRow::Workflow(nested) => {
                (marks.nested, Role::Card, nested.status, nested.elapsed_ms)
            }
            WorkflowRow::Command(command) => {
                let (glyph, role) = row_glyph(&marks, command.status);
                (glyph, role, command.status, command.elapsed_ms)
            }
        };
        let time = match status {
            RowStatus::Queued => "queued".to_owned(),
            RowStatus::Waiting => String::new(),
            RowStatus::Running => panes::elapsed(elapsed_ms + ticking),
            _ => panes::elapsed(elapsed_ms),
        };
        let (model, detail, markers, cost) = match row {
            WorkflowRow::Agent(agent) => {
                let mut model = sanitize_line(&names.short(&agent.worker)).into_owned();
                if agent.standing_in_for.is_some() {
                    model = format!("{} {model}", marks.stands_in);
                }
                let tokens = if agent.tokens > 0 {
                    panes::tokens(agent.tokens)
                } else {
                    String::new()
                };
                let cost = if row_cost(ctx) {
                    panes::spend(&agent.spent).unwrap_or_default()
                } else {
                    String::new()
                };
                (model, tokens, agent_markers(agent, ctx), cost)
            }
            WorkflowRow::Workflow(nested) => (
                "workflow".to_owned(),
                format!("{}/{} agents", nested.agents_done, nested.agents_total),
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
            label: row_label(row),
            model,
            detail,
            markers,
            cost,
            time,
        }
    }

    /// What follows the model's ` · `: the detail, then the markers after another ` · `.
    fn rest_width(&self, markers: bool) -> usize {
        let detail = str_width(&self.detail);
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

/// A row's glyph, as Claude Code's: grey `●` running, green `✔` done, and our own `○` queued,
/// `✗` failed, and `◆` waiting for you.
fn row_glyph(marks: &Marks, status: RowStatus) -> (&'static str, Role) {
    match status {
        RowStatus::Running => (marks.running, Role::Faint),
        RowStatus::Done => (marks.finished, Role::Success),
        RowStatus::Failed => (marks.failed, Role::Error),
        RowStatus::NeedsYou => (marks.needs_you, Role::Card),
        RowStatus::Queued | RowStatus::Waiting | RowStatus::Skipped => (marks.waiting, Role::Dim),
    }
}

fn row_cost(ctx: &ViewCtx<'_>) -> bool {
    ctx.state.ui.as_ref().is_some_and(|ui| ui.workflow.row_cost)
}

/// The right pane's columns, as Claude Code's rows: a cursor slot, the glyph, the label padded
/// to the longest in the phase, one space, the model padded to the widest, ` · `, the tokens,
/// and the time flush right. Room runs out from the right: markers, then tokens, then the
/// model go, and then the label is cut. The time always stays.
struct RowColumns {
    width: usize,
    slot: usize,
    label: usize,
    model: usize,
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
            rest: true,
            markers: true,
            cost,
            time,
        };
        let with_markers = widest(&|c| c.rest_width(true));
        let without = widest(&|c| c.rest_width(false));
        if need(label, model, with_markers) <= room {
            return fitted;
        }
        fitted.markers = false;
        if need(label, model, without) <= room {
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
            let rest = if self.rest {
                cells.rest_width(self.markers)
            } else {
                0
            };
            let model = if rest > 0 {
                left(&cells.model, self.model)
            } else {
                cut(&cells.model, self.model)
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(model, model_style));
            if rest > 0 {
                spans.push(Span::styled(" · ", dim));
                if !cells.detail.is_empty() {
                    spans.push(Span::styled(cells.detail.clone(), dim));
                }
                if self.markers && !cells.markers.is_empty() {
                    if !cells.detail.is_empty() {
                        spans.push(Span::styled(" · ", dim));
                    }
                    spans.extend(cells.markers.iter().cloned());
                }
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

/// Every agent at work in a phase, a nested workflow's included, so the workflow view and the
/// agent view title the phase alike.
pub(super) fn agents_in_phase(phase: &PhaseRow) -> usize {
    phase
        .rows
        .iter()
        .map(|row| match row {
            WorkflowRow::Agent(_) => 1,
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
            slugify.contains("│  ✔ fix:slugify Haiku 4.5    · 18.1k tok"),
            "{text}"
        );
        assert!(slugify.trim_end().ends_with("2m10s │"), "{text}");
        assert!(
            row_with(&text, "fix:median").contains("● fix:median  GPT 5.6 Luna · 31.2k tok"),
            "{text}"
        );
        assert!(!text.contains("0/0"), "{text}");
        assert!(!text.contains("card-"), "{text}");
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
            "↑↓ select · p pause · f filter · esc back · s save"
        );
        press_in(&mut view, KeyCode::Down, &state);
        assert_eq!(
            footer(&mut view, &state),
            "↑↓ select · x stop agent · p pause · f filter · esc back · s save"
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
        assert!(text.contains("Fix · running · 2 agents"), "{text}");
        assert!(!text.contains("fix:slugify"), "{text}");
        assert!(text.contains("fix:median"), "{text}");
        for _ in 0..3 {
            press_in(&mut view, KeyCode::Char('f'), &state);
        }
        let text = draw_with(&mut view, 100, &theme(), &state);
        assert!(text.contains("Fix · 2 agents"), "{text}");
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
        assert!(text.contains("● Running"), "{text}");
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
        let mut data = workflow(1, "peer-crew", Vec::new());
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
                card: argonauts_core::CardId::new("card-001").unwrap(),
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
        let own = workflow(1, "peer-crew", Vec::new());
        assert!(view.accept_workflow(&own));
        assert_eq!(view.depth(), 1);
        assert_eq!(view.current().name, "peer-crew");
        // A nested workflow the view didn't ask for changes nothing.
        let mut nested = workflow(3, "review", Vec::new());
        nested.breadcrumb = vec!["peer-crew".into(), "review".into()];
        assert!(!view.accept_workflow(&nested));
        assert_eq!(view.current().name, "peer-crew");
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
        let mut view = WorkflowView::new(workflow(1, "peer-crew", Vec::new()));
        let text = draw_with(&mut view, 120, &theme, &state);
        assert!(text.contains("1/3 agents · 2m05s"), "{text}");
        assert!(!text.contains('$'), "{text}");

        // A nested level keeps its own numbers.
        let mut nested = workflow(2, "review", Vec::new());
        nested.breadcrumb = vec!["peer-crew".into(), "review".into()];
        view.pending = Some(Pending::Workflow(nested.id.clone()));
        assert!(view.accept_workflow(&nested));
        let text = draw_with(&mut view, 120, &theme, &state);
        assert!(text.contains("1/3 agents · 1m00s"), "{text}");
    }
}
