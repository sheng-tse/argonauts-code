use std::time::{Duration, Instant};

use argonauts_core::settings::ui::VendorInternals;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::format;
use crate::render::{Clock, RenderCtx};
use crate::text::{sanitize_line, spans_width, str_width, truncate_line};
use crate::theme::{AgentColor, Role, Theme};
use crate::views::columns::{self, Cell, Column, GAP, Give};
use crate::views::layout::left;
use crate::views::panes::Marks;

use super::bar::{MIN_BAR_CELLS, bar};

/// Only used when the glyph set turns the working circle. The default set holds it still.
pub const CIRCLE_PERIOD: Duration = Duration::from_millis(250);

/// The agent panel on its own has a shorter bar than a run's, since its rows carry the task.
const BAR_CELLS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum AgentState {
    Working,
    /// Waiting on other work, such as a card it depends on.
    #[default]
    Waiting,
    /// Waiting on a timer, such as a usage reset.
    Timer,
    NeedsYou,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    /// The harness, which also names the row in a [`PanelEvent`] when it has no key.
    pub name: String,
    pub model: Option<String>,
    /// What the row calls its agent. Without one it shows the ids.
    pub label: Option<AgentLabel>,
    pub task: String,
    /// What it goes by under a run's row, its label such as `build:flag`, in place of the task,
    /// so two agents on the same harness and model read apart.
    pub card: Option<String>,
    pub state: AgentState,
    /// Done and total.
    pub steps: Option<(u32, u32)>,
    pub started_at: Option<Instant>,
    /// Freezes the elapsed time once the agent stops.
    pub ended_at: Option<Instant>,
    pub tokens_down: u64,
    /// Spent so far, such as `$0.12`, once anything was priced.
    pub spend: Option<String>,
    /// Replaces the stats when set, such as `waiting`.
    pub note: Option<String>,
    /// The name's color for the whole run. Without one the name is in normal text.
    pub color: Option<AgentColor>,
    /// The session key a [`PanelEvent`] reports, when it differs from `name`.
    pub key: Option<String>,
    /// The run whose row it sits under in the workflow panel.
    pub run: Option<String>,
    pub markers: RowMarkers,
}

/// An agent by name: `Opus 5.5 on Claude Code`, and `Claude Code` for a row too narrow for the
/// model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLabel {
    pub full: String,
    pub cli: String,
}

/// What a row adds after its stats: context against its cap, who it stands in for after a
/// handoff, review debt, and the vendor's own subagents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowMarkers {
    /// Used and cap, in tokens.
    pub context: Option<(u64, u64)>,
    /// The agent it replaced by name, for `↺ for Opus 5.5 on Claude Code`.
    pub stands_in_for: Option<String>,
    /// Work a weaker replacement did that still waits for a review.
    pub needs_review: bool,
    pub subagents: u32,
}

impl AgentRow {
    pub fn new(name: impl Into<String>, task: impl Into<String>, state: AgentState) -> Self {
        Self {
            name: name.into(),
            model: None,
            label: None,
            task: task.into(),
            card: None,
            state,
            steps: None,
            started_at: None,
            ended_at: None,
            tokens_down: 0,
            spend: None,
            note: None,
            color: None,
            key: None,
            run: None,
            markers: RowMarkers::default(),
        }
    }

    /// Its session key, or the harness.
    pub fn id(&self) -> &str {
        self.key.as_deref().unwrap_or(&self.name)
    }

    /// What a [`PanelEvent`] names the row by. Session keys repeat across runs, so the run
    /// goes with the key.
    pub fn agent_ref(&self) -> AgentRef {
        AgentRef {
            run: self.run.clone(),
            session: self.id().to_owned(),
        }
    }

    /// `Opus 5.5 on Claude Code`, as the row and the stop confirm name it.
    pub fn display_name(&self) -> String {
        let name = match (&self.label, self.model.as_deref().map(str::trim)) {
            (Some(label), _) => label.full.clone(),
            (None, Some(model)) if !model.is_empty() => format!("{}/{model}", self.name.trim()),
            (None, _) => self.name.clone(),
        };
        sanitize_line(&name).trim().to_owned()
    }

    /// The CLI alone, for a row too narrow for the model.
    fn cli_name(&self) -> String {
        let cli = self.label.as_ref().map_or(&self.name, |label| &label.cli);
        sanitize_line(cli).trim().to_owned()
    }

    pub fn with_label(mut self, full: impl Into<String>, cli: impl Into<String>) -> Self {
        self.label = Some(AgentLabel {
            full: full.into(),
            cli: cli.into(),
        });
        self
    }

    pub fn with_color(mut self, color: AgentColor) -> Self {
        self.color = Some(color);
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// `x` on the row. The panel asks once before it lets the stop through.
    pub(super) fn stop_event(&self) -> PanelEvent {
        PanelEvent::Stop {
            agent: self.agent_ref(),
            name: self.display_name(),
        }
    }

    fn is_running(&self) -> bool {
        self.started_at.is_some() && self.ended_at.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPanel {
    pub rows: Vec<AgentRow>,
    /// Rows shown before the rest fold into `↓ N more`, which takes one row more.
    pub max_rows: usize,
    /// `Some` while the panel has focus.
    pub selected: Option<usize>,
    /// `ui.panel.vendor_internals`; hidden leaves out the `+N subagents` marker.
    pub vendor_internals: VendorInternals,
}

/// An agent row's identity: its run, when the row knows it, and its session key, or the
/// harness for a row without one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentRef {
    pub run: Option<String>,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEvent {
    Open(AgentRef),
    /// A second `x` on an agent's row stops it.
    Stop {
        agent: AgentRef,
        name: String,
    },
    /// A second `x` on a run's row stops the run, with its run id.
    StopRun(String),
    /// A first `x`: the mode line asks for a second on the same row.
    StopAsked,
    /// Enter or → on a run's row, with its run id.
    OpenWorkflow(String),
    /// ↓ on the last row, with nothing more to scroll into view: the workflow view.
    PastEnd,
    /// Focus goes back to the prompt.
    Leave,
    Moved,
    Unhandled,
}

impl Default for AgentPanel {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            max_rows: 4,
            selected: None,
            vendor_internals: VendorInternals::Discovered,
        }
    }
}

impl AgentPanel {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row_mut(&mut self, name: &str) -> Option<&mut AgentRow> {
        self.rows.iter_mut().find(|row| row.name == name)
    }

    pub fn has_focus(&self) -> bool {
        self.selected.is_some() && !self.rows.is_empty()
    }

    pub fn focus(&mut self) {
        self.selected = (!self.rows.is_empty()).then_some(0);
    }

    pub fn blur(&mut self) {
        self.selected = None;
    }

    pub fn any_working(&self) -> bool {
        self.rows.iter().any(|row| row.state == AgentState::Working)
    }

    /// Once the rows are gone the panel lets go, and the key goes on to the prompt.
    pub fn handle_key(&mut self, key: KeyEvent) -> PanelEvent {
        let Some(selected) = self.selected else {
            return PanelEvent::Unhandled;
        };
        if self.rows.is_empty() {
            self.blur();
            return PanelEvent::Unhandled;
        }
        let selected = selected.min(self.rows.len() - 1);
        self.selected = Some(selected);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Up if selected == 0 => {
                self.blur();
                PanelEvent::Leave
            }
            KeyCode::Up => {
                self.selected = Some(selected - 1);
                PanelEvent::Moved
            }
            KeyCode::Down if selected + 1 < self.rows.len() => {
                self.selected = Some(selected + 1);
                PanelEvent::Moved
            }
            KeyCode::Down => PanelEvent::PastEnd,
            KeyCode::Home => {
                self.selected = Some(0);
                PanelEvent::Moved
            }
            KeyCode::End => {
                self.selected = Some(self.rows.len() - 1);
                PanelEvent::Moved
            }
            KeyCode::Enter | KeyCode::Right => PanelEvent::Open(self.rows[selected].agent_ref()),
            KeyCode::Char('x') if plain => self.rows[selected].stop_event(),
            KeyCode::Esc => {
                self.blur();
                PanelEvent::Leave
            }
            _ => PanelEvent::Unhandled,
        }
    }

    pub fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        if self.rows.is_empty() {
            return Vec::new();
        }
        let visible = self.visible_rows();
        let indices: Vec<usize> = visible.clone().collect();
        // A selection past the end, after rows went away, marks the last row.
        let last = self.rows.len() - 1;
        let selected = self.selected.map(|i| i.min(last));
        let mut lines = Vec::new();
        if visible.start > 0 {
            lines.push(more_line(false, visible.start, ctx));
        }
        lines.extend(self.row_lines(&indices, selected, ctx));
        let below = self.rows.len() - visible.end;
        if below > 0 {
            lines.push(more_line(true, below, ctx));
        }
        lines
    }

    /// The rows at `indices`, in that order, with columns fitted across all of them.
    /// `selected` is a row index.
    fn row_lines(
        &self,
        indices: &[usize],
        selected: Option<usize>,
        ctx: &RenderCtx<'_>,
    ) -> Vec<Line<'static>> {
        let cells: Vec<RowCells> = indices.iter().map(|&i| self.cells(i, false, ctx)).collect();
        let widest = |cell: fn(&RowCells) -> usize| cells.iter().map(cell).max().unwrap_or(0);
        let stats: Vec<&Stats> = cells.iter().map(|row| &row.stats).collect();
        let mut layout = RowLayout::fit(
            usize::from(ctx.width),
            [
                widest(|row| str_width(&row.name)),
                widest(|row| str_width(&row.harness)),
            ],
            widest(|row| str_width(&row.task)),
            StatColumns::wanted(&stats),
            ctx.theme.style(Role::BarFill),
        );
        layout.fit_stats(&stats, usize::from(ctx.width));
        indices
            .iter()
            .zip(&cells)
            .map(|(&i, row)| self.row_line(i, row, &layout, selected == Some(i), ctx))
            .collect()
    }

    /// What row `index` draws in its columns. Under a run's row the task column holds its
    /// label instead, since the run's row already says what the work is.
    pub(super) fn cells(&self, index: usize, under_run: bool, ctx: &RenderCtx<'_>) -> RowCells {
        let row = &self.rows[index];
        RowCells {
            name: row.display_name(),
            harness: row.cli_name(),
            task: if under_run {
                sanitize_line(row.card.as_deref().unwrap_or_default())
                    .trim()
                    .to_owned()
            } else {
                sanitize_line(&row.task).into_owned()
            },
            stats: self.stats(index, ctx),
        }
    }

    pub fn next_change(&self, clock: &Clock, theme: &Theme) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut sooner = |at: Instant| next = Some(next.map_or(at, |n| n.min(at)));
        let turning = !theme.reduced_motion && theme.glyphs.agent_working.len() > 1;
        for row in &self.rows[self.visible_rows()] {
            if row.state == AgentState::Working && turning {
                sooner(clock.next_frame_at(CIRCLE_PERIOD));
            }
            if let (Some(start), true) = (row.started_at, row.is_running()) {
                let whole = clock.now.saturating_duration_since(start).as_secs();
                sooner(start + Duration::from_secs(whole + 1));
            }
        }
        next
    }

    fn visible_rows(&self) -> std::ops::Range<usize> {
        window(self.rows.len(), self.max_rows, self.selected)
    }

    pub(super) fn row_line(
        &self,
        index: usize,
        cells: &RowCells,
        layout: &RowLayout,
        selected: bool,
        ctx: &RenderCtx<'_>,
    ) -> Line<'static> {
        let theme = ctx.theme;
        let glyphs = &theme.glyphs;
        let dim = theme.style(Role::Dim);
        let row = &self.rows[index];

        let (circle, circle_style) = match row.state {
            AgentState::Working => {
                let frames = glyphs.agent_working;
                let frame = if theme.reduced_motion || frames.is_empty() {
                    0
                } else {
                    ctx.clock.frame(CIRCLE_PERIOD, frames.len())
                };
                (
                    frames.get(frame).copied().unwrap_or(glyphs.agent_waiting),
                    theme.style(Role::Accent),
                )
            }
            AgentState::Waiting => (glyphs.agent_waiting, dim),
            AgentState::Timer => (glyphs.agent_timer, theme.style(Role::Warning)),
            AgentState::NeedsYou => (glyphs.agent_needs_you, theme.style(Role::Warning)),
            AgentState::Done => (glyphs.agent_done, theme.style(Role::Success)),
            AgentState::Failed => (glyphs.agent_failed, theme.style(Role::Error)),
        };

        let mut spans = if selected {
            vec![Span::styled(
                format!("{} ", glyphs.prompt),
                theme.style(Role::Card),
            )]
        } else {
            vec![Span::raw("  ")]
        };
        if layout.indent > 0 {
            spans.push(Span::raw(" ".repeat(layout.indent)));
        }
        spans.push(Span::styled(circle, circle_style));
        spans.push(Span::raw(" "));
        // The circle and the bar carry the state, so the name keeps the agent's color all
        // run, as Claude Code colors its subagents.
        let name = if layout.harness_only {
            &cells.harness
        } else {
            &cells.name
        };
        spans.push(Span::styled(
            left(name, layout.name_width),
            theme.agent_style(row.color),
        ));
        if layout.task_width > 0 {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(left(&cells.task, layout.task_width), dim));
        }
        if layout.bar_cells > 0 {
            spans.push(Span::raw("  "));
            spans.extend(bar(row.steps, layout.bar_cells, layout.fill, theme));
        }
        spans.push(Span::raw(" ".repeat(GAP)));
        layout
            .stats
            .line(spans, &cells.stats, usize::from(ctx.width), dim)
    }

    fn stats(&self, index: usize, ctx: &RenderCtx<'_>) -> Stats {
        let row = &self.rows[index];
        let notes = markers(&row.markers, self.vendor_internals, ctx);
        if let Some(note) = &row.note {
            let note = Span::styled(
                sanitize_line(note).trim().to_owned(),
                ctx.theme.style(Role::Dim),
            );
            return Stats {
                notes_only: true,
                notes: joined(vec![vec![note]].into_iter().chain(notes), ctx.theme),
                ..Stats::default()
            };
        }
        Stats {
            notes_only: false,
            steps: row
                .steps
                .map_or(String::new(), |(done, total)| format!("{done}/{total}")),
            // The space after the arrow is Claude Code's. Fonts like Iosevka draw the arrow two
            // cells wide, so it covers the space instead of the first digit. A run's row writes
            // its tokens the same way, so the rows under it do too.
            tokens: if row.tokens_down > 0 {
                format!(
                    "{} {} tokens",
                    ctx.theme.glyphs.tokens,
                    format::tokens(row.tokens_down)
                )
            } else {
                String::new()
            },
            spend: row.spend.as_deref().map_or(String::new(), |spend| {
                sanitize_line(spend).trim().to_owned()
            }),
            notes: joined(notes, ctx.theme),
            time: row.started_at.map_or(String::new(), |start| {
                let end = row.ended_at.unwrap_or(ctx.clock.now);
                format::duration_compact(end.saturating_duration_since(start))
            }),
        }
    }
}

/// Notes joined with ` · `, since they're one cell of a row, not columns of their own.
fn joined(notes: impl IntoIterator<Item = Cell>, theme: &Theme) -> Cell {
    let dot = Span::styled(" · ", theme.style(Role::Dim));
    let mut spans = Vec::new();
    for note in notes {
        if !spans.is_empty() {
            spans.push(dot.clone());
        }
        spans.extend(note);
    }
    spans
}

/// `↓ N more` under a panel's window, or `↑ N more` over it.
pub(super) fn more_line(below: bool, count: usize, ctx: &RenderCtx<'_>) -> Line<'static> {
    let marks = Marks::for_theme(ctx.theme);
    let arrow = if below { marks.down } else { marks.up };
    let line = Line::from(Span::styled(
        format!("  {arrow} {count} more"),
        ctx.theme.style(Role::Dim),
    ));
    truncate_line(&line, usize::from(ctx.width))
}

/// A panel takes at most `max_rows + 1` rows: all of them when that is enough, or a window with
/// `↓ N more` under it and, once scrolled, `↑ N more` above it. The window follows the selection.
pub(super) fn window(
    count: usize,
    max_rows: usize,
    selected: Option<usize>,
) -> std::ops::Range<usize> {
    let lines = max_rows.max(1) + 1;
    if count <= lines {
        return 0..count;
    }
    let selected = selected.unwrap_or(0).min(count - 1);
    // With room for one row and one marker, the selected row shows alone. In the middle it
    // gets a marker on each side, one line over, since hiding it would hide the `❯`.
    if lines < 3 {
        return selected..selected + 1;
    }
    // One marker, under the first rows.
    if selected + 1 < lines {
        return 0..lines - 1;
    }
    // Markers on both sides, unless the window nearly reaches the end.
    let shown = lines.saturating_sub(2).max(1);
    let start = (selected + 1).saturating_sub(shown).max(1);
    if count - (start + shown) > 1 {
        return start..start + shown;
    }
    // One marker, over the last rows.
    count - (lines - 1)..count
}

/// What a row notes after its stats, the debt and the handoff first, so a cut row keeps them.
fn markers(markers: &RowMarkers, internals: VendorInternals, ctx: &RenderCtx<'_>) -> Vec<Cell> {
    let theme = ctx.theme;
    let marks = Marks::for_theme(theme);
    let dim = theme.style(Role::Dim);
    let mut notes = Vec::new();
    if markers.needs_review {
        notes.push(vec![Span::styled(
            format!("{} needs review", marks.debt),
            theme.style(Role::Warning),
        )]);
    }
    if let Some(worker) = &markers.stands_in_for {
        notes.push(vec![Span::styled(
            format!("{} for {}", marks.stands_in, sanitize_line(worker).trim()),
            dim,
        )]);
    }
    if let Some((used, cap)) = markers.context {
        // Past nine tenths of the cap the session is about to rotate.
        let near = cap > 0 && used.saturating_mul(10) >= cap.saturating_mul(9);
        let style = if near {
            theme.style(Role::Warning)
        } else {
            dim
        };
        notes.push(vec![Span::styled(
            format!("ctx {}/{}", format::tokens(used), format::tokens(cap)),
            style,
        )]);
    }
    if markers.subagents > 0 && internals != VendorInternals::Hidden {
        let noun = if markers.subagents == 1 {
            "subagent"
        } else {
            "subagents"
        };
        notes.push(vec![Span::styled(
            format!("+{} {noun}", markers.subagents),
            dim,
        )]);
    }
    notes
}

/// A row's stats, before padding: steps, tokens, and spend, then its notes, and the time.
#[derive(Debug, Clone, Default)]
pub(super) struct Stats {
    /// Only the notes show, in the stats' place, as `waiting for card-002` does.
    pub notes_only: bool,
    pub steps: String,
    pub tokens: String,
    pub spend: String,
    pub notes: Cell,
    pub time: String,
}

/// The stats' columns, fitted across every row the panel draws: steps, tokens, and spend
/// right-aligned, the notes in what room is left, and the time right-aligned at the edge, as
/// Claude Code ends its agent rows.
#[derive(Debug, Clone, Default)]
pub(super) struct StatColumns {
    columns: [Column; 4],
    /// The widest time, which ends one cell short of the edge, as the mode line's parts do.
    time: usize,
    /// The cells the stats were fitted into, which a note spans.
    room: usize,
}

const STEPS: usize = 0;
const TOKENS: usize = 1;
const SPEND: usize = 2;
const NOTES: usize = 3;

/// Notes are cut rather than dropped while they have this much room.
const MIN_NOTES: usize = 14;

impl StatColumns {
    fn full(stats: &[&Stats]) -> Self {
        let widest = |cell: fn(&Stats) -> usize| stats.iter().map(|s| cell(s)).max().unwrap_or(0);
        let counted = |cell: fn(&Stats) -> usize| {
            stats
                .iter()
                .filter(|s| !s.notes_only)
                .map(|s| cell(s))
                .max()
                .unwrap_or(0)
        };
        Self {
            columns: [
                Column::right(counted(|s| str_width(&s.steps))),
                Column::right(counted(|s| str_width(&s.tokens))),
                Column::right(counted(|s| str_width(&s.spend))),
                Column::left(counted(|s| spans_width(&s.notes))),
            ],
            time: widest(|s| str_width(&s.time)),
            room: 0,
        }
    }

    /// The cells the stats take whole, after the gap before them.
    pub fn wanted(stats: &[&Stats]) -> usize {
        let full = Self::full(stats);
        let noted = stats
            .iter()
            .filter(|s| s.notes_only)
            .map(|s| spans_width(&s.notes))
            .max()
            .unwrap_or(0);
        columns::used(&full.columns).max(noted) + full.edge()
    }

    /// The time with its gap, and the cell after it.
    fn edge(&self) -> usize {
        if self.time > 0 {
            GAP + self.time + 1
        } else {
            0
        }
    }

    /// Fits the stats into `room` cells: the notes shrink to a few words, then spend, tokens,
    /// and steps give way, and then the notes are cut further. The time stays to the end.
    fn fit(stats: &[&Stats], room: usize) -> Self {
        let mut fitted = Self::full(stats);
        fitted.room = room;
        let notes = fitted.columns[NOTES].width;
        fitted.columns[NOTES].width = notes.min(MIN_NOTES);
        let room = room.saturating_sub(fitted.edge());
        let gives = [
            Give::Drop(SPEND),
            Give::Drop(TOKENS),
            Give::Drop(STEPS),
            Give::Cut {
                column: NOTES,
                min: 0,
            },
        ];
        columns::fit(&mut fitted.columns, room, &gives);
        // The notes take what the others leave.
        if fitted.columns[NOTES].width > 0 {
            let left = room.saturating_sub(columns::used(&fitted.columns));
            fitted.columns[NOTES].width = notes.min(fitted.columns[NOTES].width + left);
        }
        fitted
    }

    /// `lead`, then the row's stats in their columns, the time ending a cell short of `width`.
    /// A note spans the stats' columns.
    fn line(
        &self,
        lead: Vec<Span<'static>>,
        stats: &Stats,
        width: usize,
        dim: Style,
    ) -> Line<'static> {
        let plain = |text: &str| vec![Span::styled(text.to_owned(), dim)];
        let edge = (!stats.time.is_empty()).then(|| plain(&stats.time));
        let right_end = width.saturating_sub(1);
        let line = if stats.notes_only {
            let edge_room = if edge.is_some() { self.edge() } else { 0 };
            let span = Column::left(self.room.saturating_sub(edge_room));
            columns::line(
                lead,
                &[span],
                std::slice::from_ref(&stats.notes),
                edge,
                right_end,
            )
        } else {
            let cells = [
                plain(&stats.steps),
                plain(&stats.tokens),
                plain(&stats.spend),
                stats.notes.clone(),
            ];
            columns::line(lead, &self.columns, &cells, edge, right_end)
        };
        truncate_line(&line, width)
    }
}

pub(super) struct RowCells {
    /// `Opus 5.5 on Claude Code`.
    pub name: String,
    /// The CLI alone, for rows too narrow for the model.
    pub harness: String,
    pub task: String,
    pub stats: Stats,
}

/// Where a row's columns sit. A width of zero leaves the column out, and a name wider than its
/// column is cut with `…`.
pub(super) struct RowLayout {
    /// Cells between the selection mark and the circle, for rows under a run's row.
    pub indent: usize,
    pub name_width: usize,
    /// Names drop their model, for rows too narrow for it.
    pub harness_only: bool,
    pub task_width: usize,
    pub bar_cells: usize,
    /// The bar's fill color, as a background.
    pub fill: Style,
    /// Set by [`RowLayout::fit_stats`] once the rest is laid out.
    pub stats: StatColumns,
}

impl RowLayout {
    /// Fits the stats into what the row leaves them, the same for every row.
    pub fn fit_stats(&mut self, stats: &[&Stats], width: usize) {
        let task = if self.task_width > 0 {
            GAP + self.task_width
        } else {
            0
        };
        let bar = if self.bar_cells > 0 {
            GAP + self.bar_cells
        } else {
            0
        };
        // The mark, the indent, the circle and its space, the name, the task, the bar, and the
        // gap before the stats.
        let lead = 2 + self.indent + 2 + self.name_width + task + bar + GAP;
        self.stats = StatColumns::fit(stats, width.saturating_sub(lead));
    }

    /// The agent panel's own layout. So the stats stay whole, the bar shrinks, then goes, then
    /// names drop their model, and then the task column goes. `names` holds the widest full
    /// name and the widest harness.
    fn fit(
        width: usize,
        names: [usize; 2],
        task_width: usize,
        stats_width: usize,
        fill: Style,
    ) -> RowLayout {
        // The mark, the circle and its space, the name, and the stats after their gap.
        let row = |name_width: usize, task_width: usize| {
            let base = 2 + 2 + name_width + 2 + stats_width;
            if task_width > 0 {
                base + 2 + task_width
            } else {
                base
            }
        };
        let [full, harness] = names;
        // The bar takes its own gap too.
        let bar_cells = width
            .saturating_sub(row(full, task_width) + 2)
            .min(BAR_CELLS);
        if bar_cells >= MIN_BAR_CELLS {
            return RowLayout {
                indent: 0,
                name_width: full,
                harness_only: false,
                task_width,
                bar_cells,
                fill,
                stats: StatColumns::default(),
            };
        }
        let with_model = row(full, task_width) <= width || full == harness;
        let name_width = if with_model { full } else { harness };
        Self {
            indent: 0,
            name_width,
            harness_only: !with_model,
            task_width: if row(name_width, task_width) <= width {
                task_width
            } else {
                0
            },
            bar_cells: 0,
            fill,
            stats: StatColumns::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;

    use super::*;
    use crate::text::plain_text;
    use crate::theme::{ColorLevel, Glyphs, Theme};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn panel(t0: Instant) -> AgentPanel {
        let mut claude =
            AgentRow::new("claude-code", "card-002", AgentState::Working).with_model("opus");
        claude.steps = Some((2, 5));
        claude.started_at = Some(t0);
        claude.tokens_down = 18_100;
        let mut codex =
            AgentRow::new("codex", "card-003", AgentState::Working).with_model("gpt-5.6");
        codex.steps = Some((1, 5));
        codex.started_at = Some(t0 + Duration::from_secs(32));
        codex.tokens_down = 12_900;
        let mut agy = AgentRow::new("agy", "review", AgentState::Waiting).with_model("gemini-3.6");
        agy.note = Some("waiting for card-002".into());
        AgentPanel {
            rows: vec![claude, codex, agy],
            ..AgentPanel::default()
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn rows_read_like_the_reference() {
        let theme = theme();
        let t0 = Instant::now();
        let ctx = RenderCtx::new(100, &theme, Clock::new(t0, t0 + Duration::from_secs(252)));
        let lines = panel(t0).lines(&ctx);
        let bar = " ".repeat(20);
        // The stats line up in columns, and the times end a cell short of the edge.
        let pad = " ".repeat(21);
        assert_eq!(
            plain_text(&lines),
            format!(
                "  ◐ claude-code/opus  card-002  {bar}  2/5  ↓ 18.1k tokens{pad}4m12s\n\
                 \x20 ◐ codex/gpt-5.6     card-003  {bar}  1/5  ↓ 12.9k tokens{pad}3m40s\n\
                 \x20 ○ agy/gemini-3.6    review    {bar}  waiting for card-002"
            )
        );
        // Eight of twenty cells filled for 2/5.
        let bar: Vec<_> = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.bg.is_some())
            .collect();
        assert_eq!(bar[0].content.len(), 8);
        assert_eq!(bar[0].style, theme.style(Role::BarFill));
        assert_eq!(bar[1].content.len(), 12);
        assert_eq!(bar[1].style, theme.style(Role::BarTrack));
    }

    #[test]
    fn the_bar_is_twenty_cells_and_no_color_draws_blocks() {
        let t0 = Instant::now();
        let theme = theme();
        let wide = RenderCtx::new(200, &theme, Clock::new(t0, t0));
        let lines = panel(t0).lines(&wide);
        let bar_cells: usize = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.bg.is_some())
            .map(|s| s.content.len())
            .sum();
        assert_eq!(bar_cells, 20);

        let mut plain = Theme::dark().with_level(ColorLevel::None);
        plain.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &plain, Clock::new(t0, t0));
        let text = plain_text(&panel(t0).lines(&ctx));
        assert!(text.contains("████████░░░░░░░░░░░░  2/5"), "{text}");
    }

    #[test]
    fn narrow_terminals_give_up_the_bar_the_model_then_the_task_before_the_stats() {
        let t0 = Instant::now();
        let theme = theme();
        let first_row = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::new(t0, t0 + Duration::from_secs(252)));
            plain_text(&panel(t0).lines(&ctx)[..1])
        };
        // With 27 cells of stats, a row needs 60 without its bar and 66 with the shortest one.
        assert_eq!(
            first_row(66),
            "  ◐ claude-code/opus  card-002        2/5  ↓ 18.1k tokens   4m12s"
        );
        assert_eq!(
            first_row(65),
            "  ◐ claude-code/opus  card-002  2/5  ↓ 18.1k tokens        4m12s"
        );
        assert_eq!(
            first_row(59),
            "  ◐ claude-code  card-002  2/5  ↓ 18.1k tokens       4m12s"
        );
        assert_eq!(
            first_row(50),
            "  ◐ claude-code  2/5  ↓ 18.1k tokens        4m12s"
        );
        // Then the tokens and the steps give way, and the time stays at the edge.
        assert_eq!(first_row(30), "  ◐ claude-code  2/5    4m12s");
        assert_eq!(first_row(24), "  ◐ claude-code   4m12s");
    }

    #[test]
    fn circles_turn_and_states_have_their_shapes() {
        let theme = theme();
        let t0 = Instant::now();
        let mut panel = panel(t0);
        panel.rows[0].color = Some(AgentColor::Blue);
        panel.rows[1].state = AgentState::NeedsYou;
        panel.rows[2].state = AgentState::Failed;
        panel
            .rows
            .push(AgentRow::new("gemini", "tests", AgentState::Done));
        let at = |ms| {
            let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0 + Duration::from_millis(ms)));
            panel
                .lines(&ctx)
                .iter()
                .map(|line| line.spans[1].content.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(at(0), ["◐", "◆", "●", "●"]);
        assert_eq!(at(260), ["◐", "◆", "●", "●"]);
        let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0));
        let lines = panel.lines(&ctx);
        assert_eq!(lines[1].spans[1].style, theme.style(Role::Warning));
        assert_eq!(lines[2].spans[1].style, theme.style(Role::Error));
        assert_eq!(lines[3].spans[1].style, theme.style(Role::Success));
        let name_styles: Vec<_> = lines.iter().map(|line| line.spans[3].style).collect();
        let text = Style::new();
        assert_eq!(
            name_styles,
            [theme.style(Role::AgentBlue), text, text, text]
        );
    }

    #[test]
    fn extra_rows_fold_and_the_selection_stays_in_view() {
        let theme = theme();
        let t0 = Instant::now();
        let mut panel = panel(t0);
        for i in 0..4 {
            panel.rows.push(AgentRow::new(
                format!("extra-{i}"),
                "x",
                AgentState::Waiting,
            ));
        }
        let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0));
        let lines = panel.lines(&ctx);
        assert_eq!(lines.len(), 5);
        assert_eq!(plain_text(&lines[4..]), "  ↓ 3 more");

        panel.selected = Some(4);
        let lines = panel.lines(&ctx);
        assert_eq!(lines.len(), 5);
        assert_eq!(plain_text(&lines[..1]), "  ↑ 2 more");
        assert!(plain_text(&lines[3..4]).starts_with("❯ ○ extra-1"));
        assert_eq!(plain_text(&lines[4..]), "  ↓ 2 more");

        panel.selected = Some(5);
        let text = plain_text(&panel.lines(&ctx));
        assert!(text.starts_with("  ↑ 3 more\n"), "{text}");
        assert!(text.contains("❯ ○ extra-2"), "{text}");
        assert!(
            !text.contains("claude-code") && !text.contains("↓"),
            "{text}"
        );

        panel.rows.truncate(5);
        panel.selected = None;
        assert_eq!(panel.lines(&ctx).len(), 5);
    }

    #[test]
    fn names_drop_their_model_only_when_rows_are_narrow() {
        let theme = theme();
        let t0 = Instant::now();
        let at = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::new(t0, t0));
            plain_text(&panel(t0).lines(&ctx))
        };
        assert!(
            at(60).starts_with("  ◐ claude-code/opus  card-002  2/5"),
            "{}",
            at(60)
        );
        assert!(
            at(54).starts_with("  ◐ claude-code  card-002  2/5"),
            "{}",
            at(54)
        );
        assert!(
            at(54).contains("  ○ agy          review    waiting"),
            "{}",
            at(54)
        );
    }

    #[test]
    fn a_timer_wait_reads_apart_from_waiting_on_work() {
        let theme = theme();
        let t0 = Instant::now();
        let mut panel = panel(t0);
        panel.rows[1].state = AgentState::Timer;
        panel.rows[1].note = Some("resumes at 11:20 AM".into());
        let ctx = RenderCtx::new(100, &theme, Clock::new(t0, t0));
        let lines = panel.lines(&ctx);
        assert!(plain_text(&lines[1..2]).starts_with("  ✢ codex/gpt-5.6"));
        assert!(plain_text(&lines[1..2]).ends_with("resumes at 11:20 AM"));
        assert_eq!(lines[1].spans[1].style, theme.style(Role::Warning));
        assert!(plain_text(&lines[2..3]).starts_with("  ○ agy"));
    }

    #[test]
    fn keys_move_open_and_ask_to_stop() {
        let t0 = Instant::now();
        let mut panel = panel(t0);
        panel.rows[1].key = Some("s-001".into());
        panel.rows[1].run = Some("run-02".into());
        let codex = AgentRef {
            run: Some("run-02".into()),
            session: "s-001".into(),
        };
        assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelEvent::Unhandled);
        panel.focus();
        assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelEvent::Moved);
        assert_eq!(panel.selected, Some(1));
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter)),
            PanelEvent::Open(codex.clone())
        );
        assert_eq!(
            panel.handle_key(key(KeyCode::Right)),
            PanelEvent::Open(codex.clone())
        );

        // `x` names the agent and leaves the row as it was. Asking for a second `x` is up to the
        // bottom region.
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0));
        let before = plain_text(&panel.lines(&ctx));
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('x'))),
            PanelEvent::Stop {
                agent: codex,
                name: "codex/gpt-5.6".into(),
            }
        );
        assert_eq!(plain_text(&panel.lines(&ctx)), before);
        assert!(panel.has_focus());

        assert_eq!(panel.handle_key(key(KeyCode::Up)), PanelEvent::Moved);
        assert_eq!(panel.handle_key(key(KeyCode::Up)), PanelEvent::Leave);
        assert!(!panel.has_focus());
        panel.focus();
        assert_eq!(panel.handle_key(key(KeyCode::Esc)), PanelEvent::Leave);
        panel.focus();
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('q'))),
            PanelEvent::Unhandled
        );
    }

    #[test]
    fn focus_lets_go_when_the_rows_are_gone() {
        let t0 = Instant::now();
        let mut panel = panel(t0);
        panel.focus();
        panel.selected = Some(2);
        panel.rows.truncate(1);
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0));
        assert!(plain_text(&panel.lines(&ctx)).starts_with("❯ ◐ claude-code"));

        panel.rows.clear();
        assert!(!panel.has_focus());
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('a'))),
            PanelEvent::Unhandled
        );
        assert_eq!(panel.selected, None);
    }

    #[test]
    fn next_change_follows_seconds_and_turning_circles() {
        let t0 = Instant::now();
        let panel = panel(t0);
        let clock = Clock::new(t0, t0 + Duration::from_millis(1_100));
        let mut theme = theme();
        assert_eq!(
            panel.next_change(&clock, &theme),
            Some(t0 + Duration::from_secs(2))
        );
        theme.glyphs.agent_working = &["◐", "◓", "◑", "◒"];
        assert_eq!(
            panel.next_change(&clock, &theme),
            Some(t0 + Duration::from_millis(1_250))
        );
        theme.reduced_motion = true;
        assert_eq!(
            panel.next_change(&clock, &theme),
            Some(t0 + Duration::from_secs(2))
        );
        assert_eq!(AgentPanel::default().next_change(&clock, &theme), None);
    }

    #[test]
    fn the_window_always_holds_the_selection() {
        for max_rows in 0..=3 {
            let budget = max_rows.max(1) + 1;
            for count in 1..=8 {
                for selected in 0..count {
                    let shown = window(count, max_rows, Some(selected));
                    let case = format!("max_rows {max_rows}, {count} rows, row {selected}");
                    assert!(shown.contains(&selected), "{case}: {shown:?}");
                    let markers = usize::from(shown.start > 0) + usize::from(shown.end < count);
                    // A lone row between two markers is the one way over a budget of two.
                    assert!(shown.len() + markers <= budget.max(3), "{case}: {shown:?}");
                    if budget >= 3 {
                        assert!(shown.len() + markers <= budget, "{case}: {shown:?}");
                    }
                }
            }
        }
        assert_eq!(window(3, 1, Some(1)), 1..2);
        assert_eq!(window(3, 0, Some(2)), 2..3);
        assert_eq!(window(2, 1, Some(1)), 0..2);
    }
}
