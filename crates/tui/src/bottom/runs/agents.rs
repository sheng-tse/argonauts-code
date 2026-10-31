//! The list under the bottom line, as Claude Code draws its own: your conversation
//! as `main`, then each agent by its label, what it's doing now in one line, and how long and
//! how many tokens so far. The filled dot is who your typing goes to: `main`, or with the list
//! focused, the agent selected.

use std::time::{Duration, Instant};

use argonauts_core::app::views::ContextUse;
use argonauts_core::look::logos::logo_for;
use argonauts_core::settings::ui::CliInternals;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::base::draw::layout::{cut, left};
use crate::base::draw::panes::Marks;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::format;
use crate::base::kit::logo::logo_before;
use crate::base::text::{sanitize_line, spans_width, str_width, truncate_line};
use crate::base::theme::{AgentColor, Role, Theme};

/// Only used when the glyph set turns the working circle. The default set holds it still.
pub const CIRCLE_PERIOD: Duration = Duration::from_millis(250);

/// The keys of the focused list, in Claude Code's words (2.1.292: `enter` to view, `x` to stop
/// it, and `ctrl+x ctrl+k` to stop all agents).
pub const LIST_KEYS: &str =
    "↑/↓  to select · enter to view · x to stop it · ctrl+x ctrl+k to stop all agents";

/// What the conversation's own row is called, as in Claude Code.
pub const MAIN: &str = "main";

/// The longest a name shows in the input's placeholder, as Claude Code cuts it.
const PLACEHOLDER_NAME: usize = 20;

/// Cells between a row's columns.
const GAP: usize = 2;

/// A name column wider than this is cut, so one long name can't crowd out what the agents do.
const MAX_NAME: usize = 28;

/// What an agent is doing shows while it has at least this much room.
const MIN_DOING: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum AgentState {
    Working,
    /// Waiting on other work, such as a task it depends on.
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
    /// The CLI, which also names the row in a [`PanelEvent`] when it has no key.
    pub name: String,
    pub model: Option<String>,
    /// What the row calls its agent. Without one it shows the ids.
    pub label: Option<AgentLabel>,
    /// What it's doing now, in one line.
    pub task: String,
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
    /// The session key a [`PanelEvent`] reports, when it is different from `name`.
    pub key: Option<String>,
    /// The run whose row it sits under in the workflow panel.
    pub run: Option<String>,
    pub markers: RowMarkers,
    pub kind: RowKind,
    /// Its place among a council's seats of the same model, from 1, when that model sits more
    /// than once: the row reads "Opus 5.5 · 2".
    pub seat_number: Option<u8>,
}

/// Whose row it is, which decides what the keys do on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum RowKind {
    /// An agent a run started. A message steers it directly.
    #[default]
    Agent,
    /// Your conversation with the main agent, which is where typing goes without a selection.
    Main,
    /// A subagent the main agent's own CLI started. A message reaches it through the main
    /// agent.
    Subagent,
}

/// An agent by name: `Opus 5.5 on Claude Code`, and `Claude Code` for a row too narrow for the
/// model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLabel {
    pub full: String,
    pub cli: String,
}

/// What a row adds after its stats: context against its cap, who it is the fallback
/// for, review debt, and the CLI's own subagents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowMarkers {
    /// How full its context is, against its window.
    pub context: Option<ContextUse>,
    /// The agent it replaced by name, for `↺ for Opus 5.5 on Claude Code`.
    pub fallback_for: Option<String>,
    /// Work a weaker fallback did that still waits for a review.
    pub needs_review: bool,
    pub subagents: u32,
    /// Its CLI is out of plan usage and finishing on a small allowance.
    pub wrapping_up: bool,
    /// The artifact it published last, by name, shown as Claude Code marks one: `⧉ name`.
    pub artifact: Option<String>,
}

impl AgentRow {
    pub fn new(name: impl Into<String>, task: impl Into<String>, state: AgentState) -> Self {
        Self {
            name: name.into(),
            model: None,
            label: None,
            task: task.into(),
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
            kind: RowKind::Agent,
            seat_number: None,
        }
    }

    /// The conversation's own row.
    pub fn main() -> Self {
        Self {
            kind: RowKind::Main,
            ..Self::new(MAIN, "", AgentState::Waiting)
        }
    }

    /// What the input offers while this row is selected: `Message @build:date-checks…`.
    pub fn placeholder(&self) -> Option<String> {
        if self.kind == RowKind::Main {
            return None;
        }
        let name = cut(&self.display_name(), PLACEHOLDER_NAME);
        Some(format!("Message @{name}…"))
    }

    /// Its session key, or the CLI.
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
        let name = sanitize_line(&name).trim().to_owned();
        match self.seat_number {
            Some(number) => format!("{name} · {number}"),
            None => name,
        }
    }

    /// A council seat whose model sits more than once, numbered among those seats.
    pub fn with_seat_number(mut self, number: u8) -> Self {
        self.seat_number = Some(number);
        self
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

    /// `x` on the row. The panel asks once before it lets the stop through. Your own
    /// conversation isn't stopped from the list: Esc in the input stops its turn.
    pub(crate) fn stop_event(&self) -> PanelEvent {
        if self.kind == RowKind::Main {
            return PanelEvent::Moved;
        }
        PanelEvent::Stop {
            agent: self.agent_ref(),
            name: self.display_name(),
        }
    }

    /// Enter on the row. Your own conversation is already on screen, so the list lets go.
    pub(crate) fn open_event(&self) -> PanelEvent {
        match self.kind {
            RowKind::Main => PanelEvent::Leave,
            RowKind::Agent | RowKind::Subagent => PanelEvent::Open(self.agent_ref()),
        }
    }

    fn is_running(&self) -> bool {
        self.started_at.is_some() && self.ended_at.is_none()
    }

    /// There's something of it to stop: an agent that hasn't ended. Your own conversation
    /// isn't one of the background agents.
    pub fn stops(&self) -> bool {
        self.kind != RowKind::Main && !matches!(self.state, AgentState::Done | AgentState::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPanel {
    pub rows: Vec<AgentRow>,
    /// Rows shown before the rest fold into `↓ N more`, which takes one row more.
    pub max_rows: usize,
    /// `Some` while the panel has focus.
    pub selected: Option<usize>,
    /// `ui.panel.subagents`; hidden leaves out the `+N subagents` marker.
    pub cli_internals: CliInternals,
}

/// An agent row's identity: its run, when the row knows it, and its session key, or the
/// CLI for a row without one.
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
    /// `p` once a stop was asked on a run's row pauses the run instead, with its run id.
    PauseRun(String),
    /// A first `x`: the bottom line asks for a second on the same row.
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
            cli_internals: CliInternals::Discovered,
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

    /// Focus lands on the first agent, past your own conversation's row, since the list is
    /// where you go to reach one of them.
    pub fn focus(&mut self) {
        let first_agent = self.rows.iter().position(|row| row.kind != RowKind::Main);
        self.selected = (!self.rows.is_empty()).then_some(first_agent.unwrap_or(0));
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
            KeyCode::Enter | KeyCode::Right => {
                let event = self.rows[selected].open_event();
                if event == PanelEvent::Leave {
                    self.blur();
                }
                event
            }
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
        // A selection past the end, after rows went away, marks the last row.
        let last = self.rows.len() - 1;
        let selected = self.selected.map(|i| i.min(last));
        let mut lines = Vec::new();
        if visible.start > 0 {
            lines.push(more_line(false, visible.start, ctx));
        }
        let rows: Vec<ListRow> = visible
            .clone()
            .map(|index| ListRow { index, indent: 0 })
            .collect();
        lines.extend(self.list_lines(&rows, selected, ctx));
        let below = self.rows.len() - visible.end;
        if below > 0 {
            lines.push(more_line(true, below, ctx));
        }
        lines
    }

    /// The rows in `rows`, in that order, in columns fitted across all of them: the name, what
    /// the agent is doing, and at the right edge how long and how many tokens. On a narrow
    /// terminal what it's doing goes first, then the name is cut.
    pub(crate) fn list_lines(
        &self,
        rows: &[ListRow],
        selected: Option<usize>,
        ctx: &RenderCtx<'_>,
    ) -> Vec<Line<'static>> {
        let width = usize::from(ctx.width);
        // Where logos draw as pictures, each agent's name has its model's logo before it.
        let logos: Vec<Vec<Span<'static>>> = rows
            .iter()
            .map(|row| {
                let agent = &self.rows[row.index];
                logo_before(&logo_for(&agent.name, agent.model.as_deref()), ctx.theme)
            })
            .collect();
        let name_wants = rows
            .iter()
            .zip(&logos)
            .map(|(row, logo)| {
                row.indent + spans_width(logo) + str_width(&self.rows[row.index].display_name())
            })
            .max()
            .unwrap_or(0)
            .min(MAX_NAME);
        // The stats say as much as leaves the names whole: the notes go before a name is cut.
        let cells = Notes::WIDEST_FIRST
            .into_iter()
            .map(|notes| {
                rows.iter()
                    .map(|row| self.cells(row.index, notes, ctx))
                    .collect::<Vec<Cells>>()
            })
            .find(|cells| {
                let stats = cells
                    .iter()
                    .map(|c| spans_width(&c.stats))
                    .max()
                    .unwrap_or(0);
                4 + name_wants + GAP + stats < width
            })
            .unwrap_or_else(|| {
                rows.iter()
                    .map(|row| self.cells(row.index, Notes::None, ctx))
                    .collect()
            });
        let widest = |cell: &dyn Fn(&Cells) -> usize| cells.iter().map(cell).max().unwrap_or(0);
        // The two cells before a row, the dot and its space, and the cell kept free at the end.
        let margin = |row: &ListRow| 2 + row.indent + 2;
        let stats_width = widest(&|cells| spans_width(&cells.stats));
        let stats_room = if stats_width > 0 {
            GAP + stats_width + 1
        } else {
            0
        };
        let doing_wants = widest(&|cells| str_width(&cells.doing));
        let room = width.saturating_sub(4 + stats_room);
        let doing_shows = doing_wants > 0 && room >= name_wants + GAP + MIN_DOING;
        let name_width = if doing_shows {
            name_wants
        } else {
            name_wants.min(room)
        };
        rows.iter()
            .zip(cells)
            .zip(logos)
            .map(|((row, cells), logo)| {
                let agent = &self.rows[row.index];
                let chosen = match selected {
                    Some(selected) => selected == row.index,
                    // With nothing selected, your typing goes to the main agent.
                    None => agent.kind == RowKind::Main,
                };
                let (dot, dot_style) = dot(agent, chosen, selected.is_some(), ctx);
                let name_style = if chosen && selected.is_some() {
                    ctx.theme.style(Role::Question)
                } else {
                    ctx.theme.agent_style(agent.color)
                };
                let mut spans = vec![
                    Span::raw(" ".repeat(2 + row.indent)),
                    Span::styled(dot, dot_style),
                    Span::raw(" "),
                ];
                let name_cells = name_width.saturating_sub(row.indent);
                let mut used = margin(row) + name_cells;
                let name_room = name_cells.saturating_sub(spans_width(&logo));
                spans.extend(logo);
                spans.push(Span::styled(left(&cells.name, name_room), name_style));
                if doing_shows {
                    let doing_width = width
                        .saturating_sub(used + GAP + stats_room)
                        .min(doing_wants + (name_wants - name_cells - row.indent));
                    spans.push(Span::raw(" ".repeat(GAP)));
                    spans.push(Span::styled(
                        left(&cells.doing, doing_width),
                        ctx.theme.style(Role::Text),
                    ));
                    used += GAP + doing_width;
                }
                if !cells.stats.is_empty() {
                    // The stats share a column that ends a cell short of the edge, as the mode
                    // line's parts do.
                    let start = width.saturating_sub(1 + stats_width);
                    spans.push(Span::raw(" ".repeat(start.saturating_sub(used).max(GAP))));
                    spans.extend(cells.stats);
                }
                truncate_line(&Line::from(spans), width)
            })
            .collect()
    }

    /// What row `index` draws in its columns.
    fn cells(&self, index: usize, notes: Notes, ctx: &RenderCtx<'_>) -> Cells {
        let row = &self.rows[index];
        Cells {
            name: row.display_name(),
            doing: sanitize_line(&row.task).trim().to_owned(),
            stats: self.stats(index, notes, ctx),
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
                sooner(next_tick(start, clock.now));
            }
        }
        next
    }

    /// Your conversation's row is always there, so it doesn't use up a row of the cap.
    fn visible_rows(&self) -> std::ops::Range<usize> {
        let main = usize::from(self.rows.iter().any(|row| row.kind == RowKind::Main));
        window(self.rows.len(), self.max_rows + main, self.selected)
    }

    /// `1h 7m · ↓ 490.9k tokens`, as Claude Code ends its rows, after what Argonauts Code notes of
    /// the agent: its to-do count, review debt, who it is the fallback for, and its context. A note
    /// such as `waiting` takes the place of the time and tokens.
    fn stats(&self, index: usize, notes: Notes, ctx: &RenderCtx<'_>) -> Vec<Span<'static>> {
        let row = &self.rows[index];
        let dim = ctx.theme.style(Role::Dim);
        let plain = |text: String| vec![Span::styled(text, dim)];
        let mut parts: Vec<Vec<Span<'static>>> = Vec::new();
        if let (Some((done, total)), Notes::All) = (row.steps, notes) {
            parts.push(plain(format!("{done}/{total}")));
        }
        let shown = match notes {
            Notes::All => row.markers.clone(),
            // Work that still waits for a review is the one note that asks something of you.
            Notes::Debt => RowMarkers {
                needs_review: row.markers.needs_review,
                ..RowMarkers::default()
            },
            Notes::None => RowMarkers::default(),
        };
        parts.extend(markers(&shown, self.cli_internals, ctx));
        match &row.note {
            Some(note) => parts.push(plain(sanitize_line(note).trim().to_owned())),
            None => {
                if let Some(start) = row.started_at {
                    let end = row.ended_at.unwrap_or(ctx.clock.now);
                    parts.push(plain(elapsed(end.saturating_duration_since(start))));
                }
                if row.tokens_down > 0 {
                    parts.push(plain(format!(
                        "{}{}{} tokens",
                        ctx.theme.glyphs.tokens,
                        crate::base::theme::glyphs::gap_after(ctx.theme.glyphs.tokens),
                        format::tokens(row.tokens_down)
                    )));
                }
            }
        }
        joined(parts, ctx.theme)
    }
}

/// The row's dot. The one your typing goes to is filled, in the chosen option's color while
/// the list has focus. Any other is hollow while it works, as Claude Code's are, and shows its
/// state once that's worth a look.
fn dot(row: &AgentRow, chosen: bool, focused: bool, ctx: &RenderCtx<'_>) -> (&'static str, Style) {
    let theme = ctx.theme;
    let glyphs = &theme.glyphs;
    let dim = theme.style(Role::Dim);
    if chosen {
        let role = if focused { Role::Question } else { Role::Text };
        return (glyphs.bullet, theme.style(role));
    }
    match row.state {
        AgentState::Working => {
            let frames = glyphs.agent_working;
            // A glyph set that turns its circle shows the turn, and the default holds still.
            let turning = !theme.reduced_motion && frames.len() > 1;
            let glyph = if turning {
                frames[ctx.clock.frame(CIRCLE_PERIOD, frames.len())]
            } else {
                glyphs.agent_waiting
            };
            (glyph, dim)
        }
        AgentState::Waiting => (glyphs.agent_waiting, dim),
        AgentState::Timer => (glyphs.agent_timer, theme.style(Role::Warn)),
        AgentState::NeedsYou => (glyphs.agent_needs_you, theme.style(Role::Warn)),
        AgentState::Done => (glyphs.agent_done, theme.style(Role::Success)),
        AgentState::Failed => (glyphs.agent_failed, theme.style(Role::Danger)),
    }
}

/// The rule over the input while `agent`'s row is selected: its name at the right end, in the
/// chosen option's color, where the rule otherwise names the model you talk to.
pub(crate) fn message_rule(agent: &AgentRow, ctx: &RenderCtx<'_>) -> Line<'static> {
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let rule = theme.glyphs.rule;
    let dim = crate::base::text::marks::decoration(theme.style(Role::Rule));
    // At least one rule cell on each side, with a space around the name.
    let name = cut(&agent.display_name(), width.saturating_sub(4));
    if name.is_empty() || str_width(&name) + 4 > width {
        return Line::from(Span::styled(rule.repeat(width), dim));
    }
    let left = width - str_width(&name) - 3;
    Line::from(vec![
        Span::styled(rule.repeat(left), dim),
        Span::raw(" "),
        Span::styled(name, theme.style(Role::Question)),
        Span::raw(" "),
        Span::styled(rule.to_owned(), dim),
    ])
}

/// A row of the list and how far in it sits: two cells under a run's row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ListRow {
    pub index: usize,
    pub indent: usize,
}

struct Cells {
    name: String,
    doing: String,
    stats: Vec<Span<'static>>,
}

/// How much Argonauts Code notes of an agent before its time and tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Notes {
    All,
    Debt,
    None,
}

impl Notes {
    const WIDEST_FIRST: [Notes; 3] = [Notes::All, Notes::Debt, Notes::None];
}

/// How long an agent has run, as Claude Code's list says it: `3s`, `29m`, `1h 7m`.
fn elapsed(time: Duration) -> String {
    let secs = time.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3_600, secs % 3_600 / 60),
    }
}

/// When a running row's time next reads differently: each second in its first minute, then
/// each minute.
fn next_tick(start: Instant, now: Instant) -> Instant {
    let ran = now.saturating_duration_since(start).as_secs();
    let step = if ran < 60 { 1 } else { 60 };
    start + Duration::from_secs((ran / step + 1) * step)
}

/// Notes joined with ` · `, since they're one cell of a row, not columns of their own.
fn joined(
    notes: impl IntoIterator<Item = Vec<Span<'static>>>,
    theme: &Theme,
) -> Vec<Span<'static>> {
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

/// `↓  N more` under a panel's window, or `↑  N more` over it.
pub(crate) fn more_line(below: bool, count: usize, ctx: &RenderCtx<'_>) -> Line<'static> {
    let marks = Marks::for_theme(ctx.theme);
    let arrow = if below { marks.down } else { marks.up };
    let line = Line::from(Span::styled(
        format!(
            "  {arrow}{}{count} more",
            crate::base::theme::glyphs::gap_after(arrow)
        ),
        ctx.theme.style(Role::Dim),
    ));
    truncate_line(&line, usize::from(ctx.width))
}

/// A panel takes at most `max_rows + 1` rows: all of them when that is enough, or a window with
/// `↓ N more` under it and, once scrolled, `↑ N more` above it. The window follows the selection.
pub(crate) fn window(
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

/// What a row notes after its stats, the debt and the fallback first, so a cut row keeps them.
fn markers(
    markers: &RowMarkers,
    internals: CliInternals,
    ctx: &RenderCtx<'_>,
) -> Vec<Vec<Span<'static>>> {
    let theme = ctx.theme;
    let marks = Marks::for_theme(theme);
    let dim = theme.style(Role::Dim);
    let mut notes = Vec::new();
    if markers.needs_review {
        notes.push(vec![Span::styled(
            format!("{} needs review", marks.debt),
            theme.style(Role::Warn),
        )]);
    }
    if let Some(worker) = &markers.fallback_for {
        notes.push(vec![Span::styled(
            format!("{} for {}", marks.fallback, sanitize_line(worker).trim()),
            dim,
        )]);
    }
    if let Some(context) = markers.context {
        // Past nine tenths of the way to where it rotates, the session is about to.
        let near = context.of_cap().is_some_and(|share| share >= 0.9);
        let style = if near { theme.style(Role::Warn) } else { dim };
        notes.push(vec![Span::styled(
            format!(
                "ctx {}",
                format::tokens_of_window(context.used, context.window)
            ),
            style,
        )]);
    }
    if markers.wrapping_up {
        notes.push(vec![Span::styled("wrapping up".to_owned(), dim)]);
    }
    if let Some(artifact) = &markers.artifact {
        notes.push(vec![Span::styled(
            format!("{} {}", marks.artifact, sanitize_line(artifact).trim()),
            dim,
        )]);
    }
    if markers.subagents > 0 && internals != CliInternals::Hidden {
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

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;

    use super::*;
    use crate::base::text::plain_text;
    use crate::base::theme::{Glyphs, Theme};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    #[test]
    fn a_seat_whose_model_sits_twice_reads_with_its_number() {
        let row = AgentRow::new("claude-code", "Backs all 6 points", AgentState::Done)
            .with_label("Opus 5.5", "Claude Code")
            .with_seat_number(2);
        assert_eq!(row.display_name(), "Opus 5.5 · 2");
        let row = AgentRow::new("codex", "Answering", AgentState::Working)
            .with_label("5.6 Luna", "Codex");
        assert_eq!(row.display_name(), "5.6 Luna");
    }

    fn agent(label: &str, doing: &str, state: AgentState) -> AgentRow {
        AgentRow::new("claude-code", doing, state).with_label(label, label)
    }

    /// A sample list, as it stood `now` after `t0`.
    fn panel(now: Instant) -> AgentPanel {
        let ago = |minutes: u64| now.checked_sub(Duration::from_secs(minutes * 60));
        let mut checks = agent(
            "build:date-checks",
            "Splitting input checks in parse_dates.py",
            AgentState::Working,
        );
        checks.started_at = ago(67);
        checks.tokens_down = 490_900;
        let mut readme = agent(
            "read:readme",
            "Reading README.md for due date mentions",
            AgentState::Working,
        );
        readme.started_at = ago(29);
        readme.tokens_down = 285_100;
        let mut due = agent(
            "test:due-date",
            "Testing parse_due_dates in test_due_parser.py",
            AgentState::Working,
        );
        due.started_at = ago(21);
        due.tokens_down = 521_600;
        AgentPanel {
            rows: vec![AgentRow::main(), checks, readme, due],
            ..AgentPanel::default()
        }
    }

    fn clock() -> (Instant, Clock) {
        let now = Instant::now() + Duration::from_secs(3 * 3_600);
        (now, Clock::new(now, now))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn rows_read_like_the_picture() {
        let theme = theme();
        let (now, clock) = clock();
        let ctx = RenderCtx::new(94, &theme, clock);
        let mut panel = panel(now);
        panel.selected = Some(1);
        let lines = panel.lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "  ○ main\n\
             \x20 ● build:date-checks  Splitting input checks in parse_dates.py        1h 7m · ↓  491k tokens\n\
             \x20 ○ read:readme        Reading README.md for due date mentions         29m · ↓  285k tokens\n\
             \x20 ○ test:due-date      Testing parse_due_dates in test_due_parser.py   21m · ↓  522k tokens"
        );
        // The selected agent's dot and name take the chosen option's color, lavender, what
        // each agent does reads as text, and the stats are dim.
        let chosen = theme.style(Role::Question);
        assert_eq!(lines[1].spans[1].style, chosen);
        assert_eq!(lines[1].spans[3].style, chosen);
        assert_eq!(lines[2].spans[1].style, theme.style(Role::Dim));
        assert_eq!(lines[2].spans[3].style, Style::new());
        assert_eq!(lines[2].spans[5].style, theme.style(Role::Text));
        let stats = lines[2].spans.last().unwrap();
        assert_eq!(stats.style, theme.style(Role::Dim));
    }

    /// Where the terminal draws logos, each agent's name has its model's logo before it, and
    /// the names' column widens to hold it. `main` names no CLI, so it has none.
    #[test]
    fn each_agent_has_its_models_logo_before_its_name() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        use crate::base::kit::logo::golden::{count, pictured, with_stand_ins};
        use crate::testing::pictures::golden::{Picture, Region, assert_matches, keep};

        let theme = pictured(&crate::testing::pictures::golden::theme());
        let (now, clock) = clock();
        let ctx = RenderCtx::new(94, &theme, clock);
        let mut panel = panel(now);
        panel.rows[2].name = "codex".to_owned();
        panel.selected = Some(1);
        let lines = panel.lines(&ctx);
        let mut buf = Buffer::empty(Rect::new(0, 0, 94, lines.len() as u16));
        for (y, line) in lines.iter().enumerate() {
            buf.set_line(0, y as u16, line, 94);
        }
        assert_eq!(count(&buf), 3);
        let shown = with_stand_ins(&buf);
        keep("agent_panel_logos", &shown);
        let picture = Picture::screen("bottom", "agent_panel_logos");
        assert_matches(&picture, &shown, &Region::whole(&picture, (0, 0)), &theme);
    }

    /// With nothing selected your typing goes to the main agent, so its dot is the filled one,
    /// as in Claude Code.
    #[test]
    fn without_a_selection_main_has_the_filled_dot() {
        let theme = theme();
        let (now, clock) = clock();
        let ctx = RenderCtx::new(96, &theme, clock);
        let text = plain_text(&panel(now).lines(&ctx));
        let dots: Vec<&str> = text.lines().map(|row| &row[2..5]).collect();
        assert_eq!(dots, ["●", "○", "○", "○"]);
    }

    #[test]
    fn how_long_reads_as_claude_code_writes_it() {
        let secs = |s| elapsed(Duration::from_secs(s));
        assert_eq!(secs(0), "0s");
        assert_eq!(secs(59), "59s");
        assert_eq!(secs(60), "1m");
        assert_eq!(secs(29 * 60 + 59), "29m");
        assert_eq!(secs(67 * 60), "1h 7m");
        assert_eq!(secs(2 * 3_600), "2h 0m");
    }

    /// What Argonauts Code notes of an agent comes before the time and tokens: its to-do count, and
    /// that it's wrapping up on a small allowance. A note such as `waiting` takes their place.
    #[test]
    fn notes_come_before_the_time_and_a_wait_takes_its_place() {
        let theme = theme();
        let (now, clock) = clock();
        let ctx = RenderCtx::new(110, &theme, clock);
        let mut panel = panel(now);
        panel.rows[1].steps = Some((2, 5));
        panel.rows[1].markers.wrapping_up = true;
        // A page it published inside the workflow is noted on its row, as Claude Code marks
        // an artifact.
        panel.rows[3].markers.artifact = Some("test-report".into());
        panel.rows[2].state = AgentState::Timer;
        panel.rows[2].note = Some("resumes at 11:20 AM".into());
        let lines = panel.lines(&ctx);
        let text = plain_text(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert!(
            rows[1].ends_with("2/5 · wrapping up · 1h 7m · ↓  491k tokens"),
            "{text}"
        );
        assert!(rows[2].starts_with("  ✢ read:readme"), "{text}");
        assert!(
            rows[3].ends_with("⧉ test-report · 21m · ↓  522k tokens"),
            "{text}"
        );
        assert!(
            rows[2].trim_end().ends_with("resumes at 11:20 AM"),
            "{text}"
        );
        assert_eq!(lines[2].spans[1].style, theme.style(Role::Warn));
        // The stats share a column, whatever each row's are.
        let column =
            |row: &str, part: &str| crate::base::text::str_width(&row[..row.find(part).unwrap()]);
        assert_eq!(column(rows[1], "2/5"), column(rows[2], "resumes"));
    }

    #[test]
    fn a_narrow_terminal_gives_up_what_an_agent_does_then_cuts_its_name() {
        let theme = theme();
        let (now, clock) = clock();
        let row = |width| {
            let ctx = RenderCtx::new(width, &theme, clock);
            plain_text(&panel(now).lines(&ctx)[1..2])
        };
        assert_eq!(
            row(62),
            "  ○ build:date-checks  Splitting inp…  1h 7m · ↓  491k tokens"
        );
        assert_eq!(
            row(58),
            "  ○ build:date-checks              1h 7m · ↓  491k tokens"
        );
        assert_eq!(row(38), "  ○ build:da…  1h 7m · ↓  491k tokens");
    }

    #[test]
    fn states_have_their_shapes_once_an_agent_stops_working() {
        let theme = theme();
        let (now, clock) = clock();
        let mut panel = panel(now);
        panel.rows[1].color = Some(AgentColor::Blue);
        panel.rows[2].state = AgentState::NeedsYou;
        panel.rows[3].state = AgentState::Failed;
        panel.rows.push(agent("checks:all", "", AgentState::Done));
        let ctx = RenderCtx::new(80, &theme, clock);
        let lines = panel.lines(&ctx);
        let dots: Vec<String> = lines
            .iter()
            .map(|line| line.spans[1].content.to_string())
            .collect();
        assert_eq!(dots, ["●", "○", "◆", "●", "●"]);
        assert_eq!(lines[2].spans[1].style, theme.style(Role::Warn));
        assert_eq!(lines[3].spans[1].style, theme.style(Role::Danger));
        assert_eq!(lines[4].spans[1].style, theme.style(Role::Success));
        // An agent keeps its color all run, as Claude Code colors its subagents.
        assert_eq!(lines[1].spans[3].style, theme.style(Role::AgentBlue));
    }

    #[test]
    fn extra_rows_fold_and_the_selection_stays_in_view() {
        let theme = theme();
        let (now, clock) = clock();
        let mut panel = panel(now);
        for i in 0..4 {
            panel
                .rows
                .push(agent(&format!("extra-{i}"), "x", AgentState::Waiting));
        }
        let ctx = RenderCtx::new(80, &theme, clock);
        // Four agents show under your conversation's row, which doesn't count against the cap.
        let lines = panel.lines(&ctx);
        assert_eq!(lines.len(), 6);
        assert_eq!(plain_text(&lines[..1]), "  ● main");
        assert_eq!(plain_text(&lines[5..]), "  ↓  3 more");

        panel.selected = Some(5);
        let lines = panel.lines(&ctx);
        assert_eq!(lines.len(), 6);
        assert_eq!(plain_text(&lines[..1]), "  ↑  2 more");
        assert!(plain_text(&lines[4..5]).starts_with("  ● extra-1"));
        assert_eq!(plain_text(&lines[5..]), "  ↓  2 more");

        panel.selected = Some(7);
        let text = plain_text(&panel.lines(&ctx));
        assert!(text.starts_with("  ↑  3 more\n"), "{text}");
        assert!(text.contains("  ● extra-3"), "{text}");
        assert!(
            !text.contains("main") && !text.contains("↓  1 more"),
            "{text}"
        );
        assert_eq!(text.lines().count(), 6, "{text}");

        panel.rows.truncate(6);
        panel.selected = None;
        assert_eq!(panel.lines(&ctx).len(), 6);
    }

    #[test]
    fn keys_move_open_and_ask_to_stop() {
        let (now, _) = clock();
        let mut panel = panel(now);
        for row in &mut panel.rows[1..] {
            row.run = Some("run-01".into());
        }
        panel.rows[1].key = Some("s-001".into());
        assert_eq!(
            panel.handle_key(key(KeyCode::Down)),
            PanelEvent::Unhandled,
            "no focus yet"
        );
        // Focus lands on the first agent, past your own conversation's row.
        panel.focus();
        assert_eq!(panel.selected, Some(1));
        let scan = AgentRef {
            run: Some("run-01".into()),
            session: "s-001".into(),
        };
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter)),
            PanelEvent::Open(scan.clone())
        );
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('x'))),
            PanelEvent::Stop {
                agent: scan,
                name: "build:date-checks".into()
            }
        );
        assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelEvent::Moved);
        assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelEvent::Moved);
        assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelEvent::PastEnd);
        assert_eq!(panel.selected, Some(3));
        // Letters go on to the input, where they start a message to the agent.
        assert_eq!(
            panel.handle_key(key(KeyCode::Char('h'))),
            PanelEvent::Unhandled
        );
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(panel.handle_key(ctrl_x), PanelEvent::Unhandled);

        // Your own conversation isn't stopped from the list, and Enter on it goes back to it.
        assert_eq!(panel.handle_key(key(KeyCode::Home)), PanelEvent::Moved);
        assert_eq!(panel.handle_key(key(KeyCode::Char('x'))), PanelEvent::Moved);
        assert_eq!(panel.handle_key(key(KeyCode::Enter)), PanelEvent::Leave);
        assert!(!panel.has_focus());

        panel.focus();
        assert_eq!(panel.handle_key(key(KeyCode::Up)), PanelEvent::Moved);
        assert_eq!(panel.handle_key(key(KeyCode::Up)), PanelEvent::Leave);
        assert!(!panel.has_focus());
        panel.focus();
        assert_eq!(panel.handle_key(key(KeyCode::Esc)), PanelEvent::Leave);
        assert!(!panel.has_focus());
    }

    #[test]
    fn the_input_offers_a_message_to_the_selected_agent() {
        let (now, _) = clock();
        let panel = panel(now);
        assert_eq!(panel.rows[0].placeholder(), None);
        assert_eq!(
            panel.rows[1].placeholder().as_deref(),
            Some("Message @build:date-checks…")
        );
        // Claude Code cuts a long name at twenty cells.
        let long = agent("review:the-whole-export-path", "", AgentState::Working);
        assert_eq!(
            long.placeholder().as_deref(),
            Some("Message @review:the-whole-ex……")
        );
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::new(now, now));
        let rule = plain_text(&[message_rule(&panel.rows[1], &ctx)]);
        assert_eq!(rule, format!("{} build:date-checks ─", "─".repeat(20)));
    }

    #[test]
    fn focus_lets_go_when_the_rows_are_gone() {
        let (now, _) = clock();
        let mut panel = panel(now);
        panel.focus();
        panel.rows.clear();
        assert!(!panel.has_focus());
        assert_eq!(panel.handle_key(key(KeyCode::Down)), PanelEvent::Unhandled);
        assert_eq!(panel.selected, None);
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::new(now, now));
        assert!(panel.lines(&ctx).is_empty());
    }

    #[test]
    fn next_change_follows_seconds_then_minutes() {
        let theme = theme();
        let t0 = Instant::now();
        let mut young = agent("build:flag", "", AgentState::Working);
        young.started_at = Some(t0);
        let mut old = agent("read:readme", "", AgentState::Working);
        old.started_at = Some(t0);
        let panel = |row: AgentRow| AgentPanel {
            rows: vec![row],
            ..AgentPanel::default()
        };
        let at = |ms| Clock::new(t0, t0 + Duration::from_millis(ms));
        assert_eq!(
            panel(young).next_change(&at(2_300), &theme),
            Some(t0 + Duration::from_secs(3))
        );
        assert_eq!(
            panel(old).next_change(&at(125_000), &theme),
            Some(t0 + Duration::from_secs(180))
        );
        let idle = AgentPanel {
            rows: vec![AgentRow::main()],
            ..AgentPanel::default()
        };
        assert_eq!(idle.next_change(&at(0), &theme), None);

        // A glyph set that turns the working circle asks for its frames, unless motion is off.
        let mut turning = theme.clone();
        turning.glyphs.agent_working = &["◐", "◓", "◑", "◒"];
        let mut row = agent("build:flag", "", AgentState::Working);
        row.started_at = Some(t0);
        assert_eq!(
            panel(row.clone()).next_change(&at(1_100), &turning),
            Some(t0 + Duration::from_millis(1_250))
        );
        turning.reduced_motion = true;
        assert_eq!(
            panel(row).next_change(&at(1_100), &turning),
            Some(t0 + Duration::from_secs(2))
        );
    }

    #[test]
    fn the_window_always_holds_the_selection() {
        for max_rows in 0..=3 {
            let limit = max_rows.max(1) + 1;
            for count in 1..=8 {
                for selected in 0..count {
                    let shown = window(count, max_rows, Some(selected));
                    let case = format!("max_rows {max_rows}, {count} rows, row {selected}");
                    assert!(shown.contains(&selected), "{case}: {shown:?}");
                    let markers = usize::from(shown.start > 0) + usize::from(shown.end < count);
                    // A lone row between two markers is the one way over a limit of two.
                    assert!(shown.len() + markers <= limit.max(3), "{case}: {shown:?}");
                    if limit >= 3 {
                        assert!(shown.len() + markers <= limit, "{case}: {shown:?}");
                    }
                }
            }
        }
        assert_eq!(window(3, 1, Some(1)), 1..2);
        assert_eq!(window(3, 0, Some(2)), 2..3);
        assert_eq!(window(2, 1, Some(1)), 0..2);
    }
}
