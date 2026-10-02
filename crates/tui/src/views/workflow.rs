//! The workflow view, which is the run view: phases on the left, the selected phase's agents on
//! the right, nested workflows as `◇` rows that open their own view under a breadcrumb. Enter on
//! an agent shows its agent view in the same frame. `l` switches to the Library tab, the
//! workflow files.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::settings::ui::VendorInternals;
use argonauts_core::views::{
    AgentRow, NestedWorkflowRow, PhaseRow, RowStatus, WorkflowRow, WorkflowView as WorkflowData,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;

use crate::format;
use crate::state::{AppState, RunState, RunTotals};
use crate::text::{sanitize_line, spans_width, str_width};
use crate::theme::{Role, Theme};

use super::agent::{AgentView, needs_review, vendor_internals};
use super::follow::{Follower, Shown};
use super::layout::{cut, left, right};
use super::library::{Library, LibraryKey, WorkflowTab, draw_tabs, tab_key};
use super::list::{self, Nav};
use super::panes::{self, ELAPSED_SLOT, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Phase names and row labels past this are cut, so one long name can't take the pane.
const MAX_PHASE_NAME: usize = 24;
const MAX_LABEL: usize = 40;

/// The markers column is cut rather than dropped while it has this much room.
const MIN_MARKERS: usize = 14;

/// A label gives up this many cells, down to [`MIN_LABEL`], before a whole column goes, so a
/// row one cell short, as the ASCII set's two-cell `<>` can make it, keeps its columns. A long
/// agent name, down to [`MIN_WORKER`], gives them first, since the label tells rows apart.
const LABEL_GIVE: usize = 4;
const MIN_LABEL: usize = 12;
const MIN_WORKER: usize = 16;

#[derive(Debug)]
pub struct WorkflowView {
    /// The run's workflow at the bottom, nested ones above it; Esc pops one level.
    levels: Vec<Level>,
    /// An agent opened from the top level, drawn in this frame.
    agent: Option<AgentView>,
    pending: Option<Pending>,
    /// An agent waiting for a second `x`.
    confirm_stop: Option<SessionKey>,
    follow: Option<Follower>,
    tab: WorkflowTab,
    library: Library,
}

#[derive(Debug)]
struct Level {
    data: WorkflowData,
    phase: usize,
    row: usize,
    side: Side,
    /// When this data was first drawn, so a running workflow's times keep counting.
    seen_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Agent(SessionKey),
    Workflow(WorkflowId),
}

impl Level {
    fn new(data: WorkflowData) -> Self {
        // Open on the phase that's going on, as Claude Code does, and once it's over, on the
        // last phase whose agents ran, rather than a closing phase with none.
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
            seen_at: None,
        }
    }

    fn phase(&self) -> Option<&PhaseRow> {
        self.data.phases.get(self.phase)
    }

    fn row(&self) -> Option<&WorkflowRow> {
        self.phase().and_then(|phase| phase.rows.get(self.row))
    }

    fn refresh(&mut self, data: WorkflowData) {
        self.data = data;
        self.phase = self.phase.min(self.data.phases.len().saturating_sub(1));
        let rows = self.phase().map_or(0, |phase| phase.rows.len());
        self.row = self.row.min(rows.saturating_sub(1));
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
            pending: None,
            confirm_stop: None,
            follow: None,
            tab: WorkflowTab::Run,
            library: Library::default(),
        }
    }

    pub fn tab(&self) -> WorkflowTab {
        self.tab
    }

    /// The Library tab asks for the workflow files each time it shows, since you may have
    /// edited one since; the last list stays up until the fresh one comes.
    fn open_library(&mut self) -> ViewOutcome {
        self.tab = WorkflowTab::Library;
        ViewOutcome::Request(Library::request())
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
                root: self.levels.len() == 1,
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

    fn header(&self, ctx: &ViewCtx<'_>) -> Header {
        let theme = ctx.theme;
        let level = self.top();
        let data = &level.data;
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let name = sanitize_line(&data.name).trim().to_owned();
        let mut crumbs: Vec<String> = data
            .breadcrumb
            .iter()
            .map(|crumb| sanitize_line(crumb).trim().to_owned())
            .collect();
        if crumbs.last() == Some(&name) {
            crumbs.pop();
        }
        let mut spans = Vec::new();
        for crumb in crumbs {
            spans.push(Span::styled(crumb, dim));
            spans.push(Span::styled(format!(" {} ", marks.crumb), dim));
        }
        spans.push(Span::styled(name, theme.style(Role::Label)));

        let (done, total, elapsed_ms, spent) = match self.run_totals(ctx.state) {
            Some(totals) => (
                totals.agents_done,
                totals.agents_total,
                totals.elapsed.map_or(0, millis),
                totals.spent,
            ),
            None => (
                data.agents_done,
                data.agents_total,
                data.elapsed_ms + level.ticking(ctx.now),
                data.spent,
            ),
        };
        // Timed as the panel row times the run, so the two read the same.
        let mut summary = vec![
            format!("{done}/{total} agents"),
            format::duration_compact(Duration::from_millis(elapsed_ms)),
        ];
        summary.extend(panes::spend(&spent));
        Header {
            title: Line::from(spans),
            description: data.description.clone(),
            summary: summary.join(" · "),
        }
    }

    /// The run's own counts, the ones its panel row shows, while the view is on the run's
    /// workflow rather than one nested in it.
    fn run_totals(&self, state: &AppState) -> Option<RunTotals> {
        if self.levels.len() > 1 {
            return None;
        }
        let run = state.runs.get(self.run())?;
        let totals = run.totals(OffsetDateTime::now_utc());
        totals.elapsed.is_some().then_some(totals)
    }

    /// Stopping an agent and pausing need the run going; a finished run's view only reads.
    fn footer(&self, vim: bool, run_going: bool) -> Footer {
        if let Some(session) = &self.confirm_stop {
            let label = self
                .top()
                .data
                .phases
                .iter()
                .flat_map(|phase| &phase.rows)
                .find_map(|row| match row {
                    WorkflowRow::Agent(agent) if agent.session == *session => {
                        Some(sanitize_line(&agent.label).trim().to_owned())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| session.to_string());
            let ask = format!("x again to stop {label}");
            return Footer::hints(&[(&ask, 0), ("esc to keep", 0)]);
        }
        let library = format!("{} library", tab_key(vim));
        let mut hints = vec![("↑↓ select", 0), ("tab switch pane", 1), ("→ open", 4)];
        if run_going {
            hints.extend([("x stop", 3), ("p pause", 2)]);
        }
        hints.extend([(library.as_str(), 1), (panes::ESC_BACK, 0)]);
        Footer::hints(&hints)
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
                let count = level.phase().map_or(0, |phase| phase.rows.len());
                level.row = step(level.row, count, nav);
            }
        }
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
            other => other,
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
            && view.breadcrumb.is_empty()
            && !self.levels[0].data.breadcrumb.is_empty()
            && view.id != self.levels[0].data.id
        {
            self.levels[0] = Level::new(view.clone());
            self.pending = None;
            self.confirm_stop = None;
            return true;
        }
        if self.pending == Some(Pending::Workflow(view.id.clone())) {
            self.pending = None;
            self.confirm_stop = None;
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

fn step(at: usize, count: usize, nav: Nav) -> usize {
    let last = count.saturating_sub(1);
    match nav {
        Nav::By(delta) => at.saturating_add_signed(delta).min(last),
        Nav::Top => 0,
        Nav::Bottom => last,
    }
}

impl View for WorkflowView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        self.top_mut().seen_at.get_or_insert(ctx.now);
        if let Some(follow) = &self.follow {
            follow.show(self.shown());
        }
        if self.tab == WorkflowTab::Library {
            self.library
                .render(area, buf, ctx.theme, list::vim(ctx.state));
            return;
        }
        let header = self.header(ctx);
        if let Some(agent) = self.agent.as_mut() {
            agent.render_frame(area, buf, ctx, &header);
            return;
        }
        let theme = ctx.theme;
        let level = self.top();
        let phases = PhaseColumns::fit(&level.data.phases, theme);
        let panes = panes::split(area, phases.width() as u16);
        panes::draw_header(buf, panes.header, &header, theme);
        draw_tabs(
            buf,
            panes.header,
            WorkflowTab::Run,
            header.title.width(),
            theme,
        );
        let right_title = level.phase().map_or_else(String::new, |phase| {
            format!(
                "{} · {}",
                sanitize_line(&phase.name).trim(),
                panes::count(agents_in_phase(phase), "agent")
            )
        });
        panes::draw_box(buf, &panes, ["Phases", &right_title], level.side, theme);

        let phase_lines = phases.lines(level, usize::from(panes.left.width), theme);
        let skip = (level.phase + 1).saturating_sub(usize::from(panes.left.height));
        panes::put_lines(buf, panes.left, &phase_lines[skip.min(phase_lines.len())..]);

        let rows = RowColumns::fit(&level.data, usize::from(panes.right.width), ctx);
        let lines: Vec<Line<'static>> = match level.phase() {
            Some(phase) if !phase.rows.is_empty() => phase
                .rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let selected = level.side == Side::Right && i == level.row;
                    rows.line(row, selected, level.ticking(ctx.now), ctx)
                })
                .collect(),
            Some(phase) => {
                let run = ctx.state.runs.get(&level.data.run);
                vec![dim_line(&empty_phase(phase, run), theme)]
            }
            None => vec![dim_line("No phases yet", theme)],
        };
        let skip = (level.row + 1).saturating_sub(usize::from(panes.right.height));
        panes::put_lines(buf, panes.right, &lines[skip.min(lines.len())..]);

        panes::draw_footer(
            buf,
            panes.footer,
            self.footer(
                list::vim(ctx.state),
                super::run_going(ctx.state, self.run()),
            ),
            "",
            theme,
        );
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if self.tab == WorkflowTab::Library {
            return match self.library.handle_key(key, ctx) {
                LibraryKey::Outcome(outcome) => outcome,
                LibraryKey::ToRun => {
                    self.tab = WorkflowTab::Run;
                    ViewOutcome::Consumed
                }
            };
        }
        if let Some(outcome) = self.agent_key(key, ctx) {
            return outcome;
        }
        // `h` goes up as ← does, and closes where ← has nowhere to go.
        let back = if self.top().side == Side::Left && self.levels.len() == 1 {
            KeyCode::Esc
        } else {
            KeyCode::Left
        };
        let vim = list::vim(ctx.state);
        let key = list::standard(key, vim, back);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if let Some(session) = self.confirm_stop.take() {
            match key.code {
                KeyCode::Char('x') if plain => {
                    return ViewOutcome::Send(UiCommand::StopAgent {
                        run: self.run().clone(),
                        session,
                    });
                }
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
            KeyCode::Tab | KeyCode::BackTab => {
                let level = self.top_mut();
                level.side = match level.side {
                    Side::Left => Side::Right,
                    Side::Right => Side::Left,
                };
                ViewOutcome::Consumed
            }
            // ← goes up: to the Phases pane, then out of a nested workflow.
            KeyCode::Left if self.top().side == Side::Left && self.levels.len() > 1 => {
                self.levels.pop();
                self.pending = None;
                ViewOutcome::Consumed
            }
            KeyCode::Left => {
                self.top_mut().side = Side::Left;
                ViewOutcome::Consumed
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
            KeyCode::Char('x') if plain => {
                let level = self.top();
                match level.row().filter(|_| level.side == Side::Right) {
                    Some(WorkflowRow::Agent(agent)) => {
                        self.confirm_stop = Some(agent.session.clone());
                        ViewOutcome::Consumed
                    }
                    _ => ViewOutcome::Notice("Select an agent to stop it".to_owned()),
                }
            }
            KeyCode::Char('p') if plain => super::pause(self.run(), ctx),
            KeyCode::Char(c) if plain && c == tab_key(vim) => self.open_library(),
            KeyCode::Esc if self.levels.len() > 1 => {
                self.levels.pop();
                self.pending = None;
                ViewOutcome::Consumed
            }
            KeyCode::Esc => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        match data {
            ViewData::Workflow { view } if view.run == *self.run() => self.accept_workflow(view),
            ViewData::Agent { view } if view.run == *self.run() => self.accept_agent(view),
            ViewData::WorkflowLibrary { .. } => self.library.accept(data),
            _ => false,
        }
    }

    fn next_change(&self, now: Instant) -> Option<Instant> {
        if self.tab == WorkflowTab::Library {
            return None;
        }
        if let Some(agent) = &self.agent {
            return agent.next_change(now);
        }
        let level = self.top();
        level.seen_at.filter(|_| level.running())?;
        let shown = level.data.elapsed_ms + level.ticking(now);
        Some(now + Duration::from_millis(1000 - shown % 1000))
    }
}

/// What a phase with no agents says: the card it waits on you for, that it closed with none, or
/// that its agents haven't started.
fn empty_phase(phase: &PhaseRow, run: Option<&RunState>) -> String {
    let asks = run.and_then(|run| {
        run.checkpoints
            .iter()
            .rev()
            .find(|open| open.opened.needs_you)
    });
    match (phase.status, asks) {
        (RowStatus::Done, _) => "No agents ran in this phase".to_owned(),
        (RowStatus::Running | RowStatus::NeedsYou, Some(open)) => format!(
            "Waiting for your answer: {}",
            sanitize_line(&open.opened.title).trim()
        ),
        (RowStatus::NeedsYou, None) => "Waiting for your answer in the chat".to_owned(),
        _ => "No agents in this phase yet".to_owned(),
    }
}

fn dim_line(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), theme.style(Role::Dim)))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The Phases pane's columns: the mark, the name, and `done/total`.
struct PhaseColumns {
    mark: usize,
    name: usize,
    count: usize,
}

impl PhaseColumns {
    fn fit(phases: &[PhaseRow], theme: &Theme) -> Self {
        let marks = Marks::for_theme(theme);
        let digits = phases.len().to_string().len();
        Self {
            mark: marks.slot().max(digits),
            name: phases
                .iter()
                .map(|phase| str_width(sanitize_line(&phase.name).trim()))
                .max()
                .unwrap_or(0)
                .min(MAX_PHASE_NAME),
            count: phases
                .iter()
                .map(|phase| format!("{}/{}", phase.done, phase.total).len())
                .max()
                .unwrap_or(0),
        }
    }

    fn width(&self) -> usize {
        2 + self.mark + 1 + self.name + 1 + self.count
    }

    fn lines(&self, level: &Level, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        // A narrow pane cuts the names before the counts, and drops the counts last.
        let fixed = 2 + self.mark + 1 + 1 + self.count;
        let name_width = self.name.min(width.saturating_sub(fixed));
        let show_count = width > fixed;
        level
            .data
            .phases
            .iter()
            .enumerate()
            .map(|(i, phase)| {
                let selected = i == level.phase;
                let (mark, mark_style) = match phase.status {
                    RowStatus::Done => (marks.done.to_owned(), theme.style(Role::Success)),
                    RowStatus::Running => ((i + 1).to_string(), theme.style(Role::Label)),
                    RowStatus::NeedsYou => (marks.needs_you.to_owned(), theme.style(Role::Card)),
                    RowStatus::Failed => (marks.failed.to_owned(), theme.style(Role::Error)),
                    RowStatus::Waiting => (marks.phase_waiting.to_owned(), dim),
                };
                let name_style = if selected {
                    theme.style(Role::Label).add_modifier(Modifier::BOLD)
                } else if phase.status == RowStatus::Waiting {
                    dim
                } else {
                    theme.style(Role::Text)
                };
                let lead = if selected {
                    let style = if level.side == Side::Left {
                        theme.style(Role::Label)
                    } else {
                        dim
                    };
                    Span::styled(format!("{} ", marks.selected), style)
                } else {
                    Span::raw("  ")
                };
                let mut spans = vec![
                    lead,
                    Span::styled(right(&mark, self.mark), mark_style),
                    Span::raw(" "),
                    Span::styled(
                        left(sanitize_line(&phase.name).trim(), name_width),
                        name_style,
                    ),
                ];
                if show_count {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        right(&format!("{}/{}", phase.done, phase.total), self.count),
                        dim,
                    ));
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// The right pane's columns, fitted across every phase's rows so they hold when the phase
/// changes: the state glyph, the label, then dim worker, tokens, spend, and markers, and the
/// duration right-aligned in its slot at the pane's edge.
struct RowColumns {
    width: usize,
    slot: usize,
    label: usize,
    worker: usize,
    tokens: usize,
    spend: usize,
    markers: usize,
}

/// The cells of one agent row, before padding.
struct AgentCells {
    worker: String,
    tokens: String,
    spend: String,
    markers: Vec<Span<'static>>,
}

impl RowColumns {
    fn fit(data: &WorkflowData, width: usize, ctx: &ViewCtx<'_>) -> Self {
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let rows = || data.phases.iter().flat_map(|phase| &phase.rows);
        let label = rows()
            .map(|row| str_width(&row_label(row)))
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL);
        let cells: Vec<AgentCells> = rows()
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some(agent_cells(agent, ctx)),
                WorkflowRow::Workflow(_) => None,
            })
            .collect();
        let widest = |cell: fn(&AgentCells) -> usize| cells.iter().map(cell).max().unwrap_or(0);
        let mut columns = Self {
            width,
            slot: marks.slot(),
            label,
            worker: widest(|c| str_width(&c.worker)),
            tokens: widest(|c| str_width(&c.tokens)),
            spend: widest(|c| str_width(&c.spend)),
            markers: widest(|c| spans_width(&c.markers)),
        };
        // The markers shrink to the room left, down to a few words. Past that, a few cells come
        // off the agent's name or else the label, then spend, tokens, the markers, and the
        // worker give way in turn, and then the label is cut.
        let full = columns.markers;
        let shrink_markers = |columns: &mut Self| {
            if columns.markers > 0 {
                columns.markers = full;
                let over = columns.used().saturating_sub(width);
                columns.markers = full.saturating_sub(over).max(MIN_MARKERS.min(full));
            }
        };
        let order: [fn(&mut Self) -> &mut usize; 4] = [
            |c| &mut c.spend,
            |c| &mut c.tokens,
            |c| &mut c.markers,
            |c| &mut c.worker,
        ];
        for column in order {
            shrink_markers(&mut columns);
            let over = columns.used().saturating_sub(width);
            if over == 0 {
                break;
            }
            if over <= LABEL_GIVE && columns.worker >= MIN_WORKER + over {
                columns.worker -= over;
                break;
            }
            if over <= LABEL_GIVE.min(columns.label.saturating_sub(MIN_LABEL)) {
                columns.label -= over;
                break;
            }
            *column(&mut columns) = 0;
        }
        let fixed = columns.used() - columns.label;
        columns.label = columns.label.min(width.saturating_sub(fixed));
        columns
    }

    /// The middle columns and their dots.
    fn middle(&self) -> usize {
        let columns = [self.worker, self.tokens, self.spend, self.markers];
        let shown = columns.iter().filter(|&&w| w > 0).count();
        columns.iter().sum::<usize>() + shown.saturating_sub(1) * 3
    }

    fn used(&self) -> usize {
        let middle = self.middle();
        let gap = if middle > 0 { 2 } else { 0 };
        2 + self.slot + 1 + self.label + gap + middle + 2 + ELAPSED_SLOT
    }

    fn line(
        &self,
        row: &WorkflowRow,
        selected: bool,
        ticking: u64,
        ctx: &ViewCtx<'_>,
    ) -> Line<'static> {
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let (glyph, glyph_role, status, elapsed_ms) = match row {
            WorkflowRow::Agent(agent) => {
                let (glyph, _, role) = marks.status(agent.status);
                (glyph, role, agent.status, agent.elapsed_ms)
            }
            WorkflowRow::Workflow(nested) => {
                (marks.nested, Role::Label, nested.status, nested.elapsed_ms)
            }
        };
        let lead = if selected {
            Span::styled(format!("{} ", marks.selected), theme.style(Role::Label))
        } else {
            Span::raw("  ")
        };
        let label_style = if selected {
            theme.style(Role::Label)
        } else if status == RowStatus::Waiting {
            dim
        } else {
            theme.style(Role::Text)
        };
        let mut spans = vec![
            lead,
            Span::styled(panes::pad_glyph(glyph, self.slot), theme.style(glyph_role)),
            Span::raw(" "),
            Span::styled(left(&row_label(row), self.label), label_style),
        ];
        // Everything between the label and the duration's slot.
        let middle_room = self
            .width
            .saturating_sub(spans_width(&spans) + 2 + ELAPSED_SLOT);
        match row {
            WorkflowRow::Agent(agent) => {
                spans.extend(self.agent_middle(agent_cells(agent, ctx), middle_room, theme));
            }
            WorkflowRow::Workflow(nested) if middle_room > 2 => {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    left(&nested_detail(nested, &ctx.state.names), middle_room - 2),
                    dim,
                ));
            }
            WorkflowRow::Workflow(_) => spans.push(Span::raw(" ".repeat(middle_room))),
        }
        let pad = self
            .width
            .saturating_sub(spans_width(&spans) + ELAPSED_SLOT);
        spans.push(Span::raw(" ".repeat(pad)));
        let elapsed = elapsed_ms
            + if status == RowStatus::Running {
                ticking
            } else {
                0
            };
        spans.push(Span::styled(
            right(&panes::elapsed(elapsed), ELAPSED_SLOT),
            dim,
        ));
        Line::from(spans)
    }

    /// The model, tokens, spend, and markers joined with ` · ` right after one another, so no
    /// dot floats after padding. A column that gave way leaves its cell out on every row, and
    /// the markers take what room the row has left.
    fn agent_middle(&self, cells: AgentCells, room: usize, theme: &Theme) -> Vec<Span<'static>> {
        let dim = theme.style(Role::Dim);
        if self.middle() == 0 || room < 2 {
            return Vec::new();
        }
        let text = |text: String| vec![Span::styled(text, dim)];
        let mut spans = vec![Span::raw("  ")];
        let mut first = true;
        let mut join = |spans: &mut Vec<Span<'static>>, cell: Vec<Span<'static>>| {
            if !first {
                spans.push(Span::styled(" · ", dim));
            }
            first = false;
            spans.extend(cell);
        };
        for (width, cell) in [
            (self.worker, cut(&cells.worker, self.worker)),
            (self.tokens, cells.tokens),
            (self.spend, cells.spend),
        ] {
            if width > 0 && !cell.is_empty() {
                join(&mut spans, text(cell));
            }
        }
        let dot = if spans.len() > 1 { 3 } else { 0 };
        let markers_room = room.saturating_sub(spans_width(&spans) + dot);
        let markers = crate::text::truncate_spans(&cells.markers, markers_room);
        if self.markers > 0 && spans_width(&markers) > 0 {
            join(&mut spans, markers);
        }
        spans
    }
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
        })
        .sum()
}

fn row_label(row: &WorkflowRow) -> String {
    let label = match row {
        WorkflowRow::Agent(agent) => &agent.label,
        WorkflowRow::Workflow(nested) => &nested.name,
    };
    sanitize_line(label).trim().to_owned()
}

fn agent_cells(agent: &AgentRow, ctx: &ViewCtx<'_>) -> AgentCells {
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
    let names = &ctx.state.names;
    if let Some(worker) = &agent.standing_in_for {
        let original = sanitize_line(&names.worker(worker)).into_owned();
        push(format!("{} for {original}", marks.stands_in), Role::Dim);
    }
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
    AgentCells {
        worker: sanitize_line(&names.worker(&agent.worker)).into_owned(),
        tokens: if agent.tokens > 0 {
            panes::tokens(agent.tokens)
        } else {
            String::new()
        },
        spend: panes::spend(&agent.spent).unwrap_or_default(),
        markers,
    }
}

/// `workflow · 2/4 agents · Opus 5.5 on Claude Code, GPT-6.1 Sol on Codex`.
fn nested_detail(nested: &NestedWorkflowRow, names: &DisplayNames) -> String {
    let mut detail = format!(
        "workflow · {}/{} agents",
        nested.agents_done, nested.agents_total
    );
    if !nested.workers.is_empty() {
        let workers: Vec<String> = nested
            .workers
            .iter()
            .map(|worker| sanitize_line(&names.worker(worker)).into_owned())
            .collect();
        detail.push_str(&format!(" · {}", workers.join(", ")));
    }
    detail
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::views::AgentView as AgentData;
    use argonauts_core::workflow::StandardPhase;

    use super::*;
    use crate::stability::{assert_frames, buffer_text};
    use crate::state::AppState;
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
            phases: vec![PhaseRow {
                name: "Build".into(),
                standard: StandardPhase::Do,
                status: RowStatus::Running,
                done: 1,
                total: rows.len() as u32,
                rows,
            }],
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

    fn press(view: &mut WorkflowView, code: KeyCode) -> ViewOutcome {
        let theme = theme();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
    }

    fn screen(view: &mut WorkflowView) -> String {
        let theme = theme();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let area = Rect::new(0, 0, 80, 16);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        assert_frames(&buf);
        buffer_text(&buf)
    }

    fn press_in(view: &mut WorkflowView, c: char, vim: bool) -> ViewOutcome {
        let theme = theme();
        let state = list::keys_state(vim);
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &ctx)
    }

    #[test]
    fn vim_keys_move_open_and_go_up_only_when_on() {
        let mut view = WorkflowView::new(parent());
        for c in ['j', 'k', 'g', 'G', 'h'] {
            assert!(
                matches!(press_in(&mut view, c, false), ViewOutcome::Ignored),
                "{c}"
            );
        }
        assert_eq!(view.top().side, Side::Left);

        // `l` goes into the agents pane as → does, then `j`, `G`, `g`, and `k` move in it.
        press_in(&mut view, 'l', true);
        assert_eq!((view.top().side, view.top().row), (Side::Right, 0));
        press_in(&mut view, 'j', true);
        assert_eq!(view.top().row, 1);
        press_in(&mut view, 'G', true);
        assert_eq!(view.top().row, 2);
        press_in(&mut view, 'g', true);
        assert_eq!(view.top().row, 0);
        press_in(&mut view, 'G', true);
        press_in(&mut view, 'k', true);
        assert_eq!(view.top().row, 1);
        // `h` goes up a pane as ← does, and closes from the top.
        press_in(&mut view, 'h', true);
        assert_eq!(view.top().side, Side::Left);
        assert!(matches!(press_in(&mut view, 'h', true), ViewOutcome::Close));
        // Home and End work without vim keys.
        press(&mut view, KeyCode::Right);
        press(&mut view, KeyCode::End);
        assert_eq!(view.top().row, 2);
        press(&mut view, KeyCode::Home);
        assert_eq!(view.top().row, 0);
    }

    #[test]
    fn the_library_tab_is_l_and_capital_l_with_vim_keys() {
        let mut view = WorkflowView::new(parent());
        assert!(matches!(
            press_in(&mut view, 'l', false),
            ViewOutcome::Request(ViewRequest::WorkflowLibrary)
        ));
        assert!(matches!(
            press_in(&mut view, 'l', false),
            ViewOutcome::Consumed
        ));
        assert_eq!(view.tab, WorkflowTab::Run);

        // With vim keys `l` opens as Enter does, so `L` switches tabs.
        press_in(&mut view, 'l', true);
        assert_eq!(view.tab, WorkflowTab::Run);
        assert!(matches!(
            press_in(&mut view, 'L', true),
            ViewOutcome::Request(ViewRequest::WorkflowLibrary)
        ));
        assert!(matches!(
            press_in(&mut view, 'L', true),
            ViewOutcome::Consumed
        ));
        assert_eq!(view.tab, WorkflowTab::Run);
    }

    #[test]
    fn esc_pops_nested_level() {
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
        assert!(text.starts_with("ui-kit › build:c"), "{text}");

        assert!(matches!(
            press(&mut view, KeyCode::Esc),
            ViewOutcome::Consumed
        ));
        assert_eq!(view.depth(), 1);
        assert_eq!(view.current().name, "ui-kit");
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

        press(&mut view, KeyCode::Tab);
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

        // Esc leaves the agent for the workflow's frame, and the next closes the view.
        assert!(matches!(
            press(&mut view, KeyCode::Esc),
            ViewOutcome::Consumed
        ));
        assert!(view.agent().is_none());
        assert!(matches!(press(&mut view, KeyCode::Esc), ViewOutcome::Close));
    }

    fn kit() -> WorkflowData {
        let mut rows = Vec::new();
        for (session, label, by, cost) in [
            ("s-002", "build:a-terminal-bottom", "claude-code/opus", 0.84),
            ("s-003", "build:b-history-cells", "codex/gpt-5.6", 1.12),
            ("s-005", "build:d-plain-mode", "agy/gemini-3.6", 0.0),
        ] {
            let mut row = agent(session, label, RowStatus::Done);
            row.worker = by.parse().unwrap();
            row.spent = Spend {
                cost_usd: cost,
                ..Spend::default()
            };
            rows.push(WorkflowRow::Agent(row));
        }
        if let WorkflowRow::Agent(row) = &mut rows[1] {
            row.subagents = 2;
        }
        rows.insert(2, parent().phases[0].rows[2].clone());
        workflow(1, "argonauts-ui-kit", rows)
    }

    fn draw_with(view: &mut WorkflowView, width: u16, theme: &Theme, state: &AppState) -> String {
        let ctx = ViewCtx {
            theme,
            state,
            now: Instant::now(),
        };
        let area = Rect::new(0, 0, width, 12);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        assert_frames(&buf);
        buffer_text(&buf)
    }

    fn row_with<'a>(text: &'a str, label: &str) -> &'a str {
        text.lines()
            .find(|line| line.contains(label))
            .unwrap_or_default()
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
        // One cell short at 100 columns cuts the label, not the tokens.
        let row = row_with(&text, "build:b");
        assert!(row.contains("381k tok · +2 subagents"), "{text}");
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 12));
        let ctx = ViewCtx {
            theme: &ascii,
            state: &state,
            now: Instant::now(),
        };
        WorkflowView::new(kit()).render(buf.area, &mut buf, &ctx);
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

    #[test]
    fn pause_acts_only_on_the_run_on_screen() {
        let mut view = WorkflowView::new(parent());
        let outcome = press(&mut view, KeyCode::Char('p'));
        assert!(
            matches!(&outcome, ViewOutcome::Notice(text) if text.starts_with("Only the run on screen")),
            "{outcome:?}"
        );
        let theme = theme();
        let mut state = AppState::default();
        state.attached = Some(run());
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let outcome = view.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE), &ctx);
        assert!(
            matches!(outcome, ViewOutcome::Send(UiCommand::Pause { now: false })),
            "{outcome:?}"
        );
    }

    #[test]
    fn left_on_the_phases_pane_pops_a_nested_level() {
        let mut view = WorkflowView::new(parent());
        view.pending = Some(Pending::Workflow(WorkflowId::numbered(2)));
        assert!(view.accept(&ViewData::Workflow {
            view: Box::new(workflow(2, "build:c", Vec::new())),
        }));
        assert_eq!(view.depth(), 2);
        press(&mut view, KeyCode::Right);
        press(&mut view, KeyCode::Left);
        assert_eq!(view.depth(), 2, "the first ← goes to the Phases pane");
        press(&mut view, KeyCode::Left);
        assert_eq!(view.depth(), 1);
        assert!(matches!(
            press(&mut view, KeyCode::Left),
            ViewOutcome::Consumed
        ));
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
    }

    fn phase(name: &str, status: RowStatus, rows: Vec<WorkflowRow>) -> PhaseRow {
        PhaseRow {
            name: name.into(),
            standard: StandardPhase::Do,
            status,
            done: rows.len() as u32,
            total: rows.len() as u32,
            rows,
        }
    }

    /// After the finish, the view opens on the last phase whose agents ran, not on a closing
    /// phase that had none.
    #[test]
    fn a_finished_workflow_opens_on_its_last_phase_with_agents() {
        let mut data = workflow(1, "peer-crew", Vec::new());
        let done = |session: &str| WorkflowRow::Agent(agent(session, "owner", RowStatus::Done));
        data.phases = vec![
            phase("Goal", RowStatus::Done, vec![done("s-001")]),
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

    /// A finished run's view only reads, so it offers no stop or pause.
    #[test]
    fn a_finished_runs_view_offers_no_stop_or_pause() {
        let theme = theme();
        let mut state = AppState::default();
        let mut finished = crate::state::RunState::new(run());
        finished.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        state.runs.insert(finished.id.clone(), finished);
        let mut view = WorkflowView::new(workflow(1, "peer-crew", Vec::new()));
        let text = draw_with(&mut view, 120, &theme, &state);
        let footer = text.lines().last().unwrap_or_default();
        assert!(!footer.contains("x stop"), "{footer}");
        assert!(!footer.contains("p pause"), "{footer}");
        assert!(footer.trim_end().ends_with("esc back"), "{footer}");
    }

    /// The header counts the run as its panel row does: every agent, the run's time, and the
    /// spend the mode line shows.
    #[test]
    fn the_header_shows_the_panel_rows_numbers() {
        let theme = theme();
        let wall = OffsetDateTime::now_utc();
        let mut state = AppState::default();
        let mut run = crate::state::RunState::new(run());
        run.started = Some(wall - time::Duration::seconds(125));
        run.ended = Some(wall);
        run.finished = Some(argonauts_core::events::run::RunOutcome::Merged);
        for n in 1..=5 {
            let mut session = crate::state::SessionState::new(
                SessionKey::numbered(n),
                "claude-code".parse().unwrap(),
                "claude-code",
                argonauts_core::names::RoleName::Owner,
                wall,
            );
            session.status = crate::state::SessionStatus::Done;
            session.cost_usd = Some(0.05);
            run.sessions.insert(session.key.clone(), session);
        }
        state.runs.insert(run.id.clone(), run);
        let mut view = WorkflowView::new(workflow(1, "peer-crew", Vec::new()));
        let text = draw_with(&mut view, 120, &theme, &state);
        assert!(text.contains("5/5 agents · 2m5s · $0.25"), "{text}");

        let rows = crate::bridge::panel::run_rows(&state, wall, Instant::now());
        assert_eq!((rows[0].agents_done, rows[0].agents_total), (5, 5));
        assert_eq!(rows[0].elapsed_ms, 125_000);
        assert_eq!(rows[0].spend.as_deref(), Some("$0.25"));

        // A nested level keeps its own numbers.
        let mut nested = workflow(2, "review", Vec::new());
        nested.breadcrumb = vec!["peer-crew".into(), "review".into()];
        view.pending = Some(Pending::Workflow(nested.id.clone()));
        assert!(view.accept_workflow(&nested));
        let text = draw_with(&mut view, 120, &theme, &state);
        assert!(text.contains("1/3 agents · 1m0s"), "{text}");
    }
}
