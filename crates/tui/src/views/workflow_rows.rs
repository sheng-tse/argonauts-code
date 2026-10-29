//! The rows of the workflow view, laid out as Claude Code's workflow detail view lays them out.
//! A phase is its cursor, its number or mark, its name, and `done/total`. An agent is its
//! pointer, its mark, its label, its CLI's logo with the model, then its tokens and its time at
//! the right edge. A step Argonauts Code runs itself, a check or the merge, is a row of the same
//! shape with the command or `git` where the model would be.
//!
//! A row's state is its mark alone, each state its own shape: what an agent gave, such as a
//! review's verdict and its findings, is for the agent's own view to say.

use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::agent_goal::AgentGoal;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::ui::CliInternals;
use argonauts_core::views::{
    AgentRow, PhaseRow, RowStatus, Tone, WorkflowRow, WorkflowView as WorkflowData,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::kit::logo::{ModelLabel, NoPicture};
use crate::kit::{Kit, State};
use crate::overview::shades::{ModelColors, shade};
use crate::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::{Role, Theme};

use super::ViewCtx;
use super::agent::{cli_internals, review_needed_by, review_needed_words};
use super::goal_panel;
use super::layout::{cut, cut_at_word, left};
use super::panes::{self, Marks};
use super::states;
use super::{workflow_outline, workflow_tries};

/// Phase names and row labels past these are cut, so one long name can't take the pane.
const MAX_PHASE_NAME: usize = 24;
/// The cells a phase's name takes before its count, as Claude Code's phases have them.
const NAME_COLUMN: usize = 8;
const MAX_LABEL: usize = argonauts_core::labels::MAX_CELLS;

/// The Phases pane is never narrower or wider than these. Claude Code's least is 14; two more
/// cells keep a list of short names, such as Build, Review, Final and Merge, clear of its counts.
const LEAST_PHASES: usize = 16;
const MOST_PHASES: usize = 34;

/// Blank cells between the label column and the model.
const LABEL_GAP: usize = 2;

/// The cells a time can take, which a narrow pane keeps free before it gives the label more.
const TIME_SLOT: usize = panes::ELAPSED_SLOT;

/// A row's mark and the role it draws in, each state its own shape so colour is never alone:
/// `✔` done, `×` failed, a dim `–` for one that never ran, the turning `⣾` at work, `≈` for a
/// fallback at work, `‖` paused, `◌` not started yet, `·` started and waiting its turn for a
/// slot, and `◆` waiting for you. A review that sent
/// the work back draws `«`, which [`Cells::of`] picks from its verdict.
pub(super) fn glyph(status: RowStatus, fallback: bool, theme: &Theme) -> (&'static str, Role) {
    let ascii = theme.glyphs.is_ascii();
    let pick = |unicode: &'static str, plain: &'static str| if ascii { plain } else { unicode };
    match status {
        RowStatus::Done => (pick("✔", "*"), Role::StateDone),
        RowStatus::Failed => (pick("×", "x"), Role::StateFailed),
        RowStatus::Skipped => (pick("–", "-"), Role::Dim),
        RowStatus::Running if fallback => (pick("≈", "~"), Role::Warn),
        RowStatus::Running => (pick(TURNING[0], ">"), Role::StateWorking),
        RowStatus::Paused => (pick("‖", "="), Role::Text),
        RowStatus::Waiting => (pick("◌", "o"), Role::StateWaiting),
        RowStatus::Queued => (pick("·", "_"), Role::StateWaiting),
        RowStatus::NeedsYou => states::mark(State::NeedsYou, theme),
    }
}

/// The frames of the mark at work, turning as Claude Code's braille spinners turn.
const TURNING: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// How long each frame of the turning mark shows.
pub(super) const TURN_PERIOD: Duration = Duration::from_millis(120);

/// The turning mark's frame `ms` after the view first showed, or its first frame where motion
/// is reduced or the glyphs are plain.
pub(super) fn turned(mark: &'static str, ms: u64, theme: &Theme) -> &'static str {
    if mark != TURNING[0] || theme.reduced_motion {
        return mark;
    }
    let period = TURN_PERIOD.as_millis() as u64;
    TURNING[(ms / period) as usize % TURNING.len()]
}

/// A done review's mark from its verdict's tone: `«` for one that sent the work back, `×` for
/// one that rejected it.
fn verdict_mark(tone: Tone, theme: &Theme) -> Option<(&'static str, Role)> {
    let ascii = theme.glyphs.is_ascii();
    match tone {
        Tone::Warn => Some((if ascii { "<" } else { "«" }, Role::Warn)),
        Tone::Bad => Some((if ascii { "x" } else { "×" }, Role::StateFailed)),
        _ => None,
    }
}

/// The role words in `tone` draw in.
pub(super) fn tone_role(tone: Tone) -> Role {
    match tone {
        Tone::Plain => Role::Dim,
        Tone::Text => Role::Text,
        Tone::Good => Role::Success,
        Tone::Warn => Role::Warn,
        Tone::Bad => Role::Danger,
    }
}

/// The rows a list `total` long shows in `viewport` rows with `selected` among them: all of
/// them when they fit, else a window that keeps the selected row near its middle, as Claude
/// Code's does.
pub(crate) fn window(selected: usize, total: usize, viewport: usize) -> (usize, usize) {
    if total <= viewport {
        return (0, total);
    }
    let from = selected.saturating_sub(viewport / 2).min(total - viewport);
    (from, from + viewport)
}

/// `↑  2–6 of 9 ↓` on the box's bottom edge while a list scrolls, each arrow only where rows
/// are hidden that way. Nothing when the list fits.
pub(crate) fn scroll_mark(from: usize, to: usize, total: usize, theme: &Theme) -> String {
    if total <= to.saturating_sub(from) {
        return String::new();
    }
    let marks = Marks::for_theme(theme);
    let dash = if theme.glyphs.is_ascii() { "-" } else { "–" };
    let gap = crate::theme::glyphs::gap_after(marks.up);
    let up = if from > 0 { marks.up } else { " " };
    let down = if to < total { marks.down } else { " " };
    format!("{up}{gap}{}{dash}{to} of {total} {down}", from + 1)
}

/// The Phases pane's columns: the cursor, the number or mark, the mark of a step you require
/// when the workflow has one, the name, and `done/total`.
pub(super) struct PhaseColumns {
    mark: usize,
    /// Cells for `★ ` before the name, or none when you require no step here.
    required: usize,
    name: usize,
    count: usize,
}

impl PhaseColumns {
    pub(super) fn fit(data: &WorkflowData, theme: &Theme) -> Self {
        let marks = Marks::for_theme(theme);
        let phases = &data.phases;
        let digits = phases.len().to_string().len();
        let required = if phases.iter().any(|phase| data.requires(phase)) {
            str_width(marks.required) + 1
        } else {
            0
        };
        Self {
            // A state's mark takes the number's place, and every one of them is one cell.
            mark: digits.max(1),
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

    /// The pane's width: its widest row, within the bounds Claude Code keeps it to.
    pub(super) fn width(&self) -> usize {
        let count = if self.count > 0 { 1 + self.count } else { 0 };
        let widest = 2 + self.mark + 1 + self.required + self.name + count;
        widest.clamp(LEAST_PHASES, MOST_PHASES)
    }

    /// Every phase numbered from 1, with its state's mark in the number's place once it has
    /// one: the turning `⣾` while its agents work, `‖` while they are held, `◆` while it waits
    /// for you, `✔` or `×` once it ended. Names, numbers and counts are in the text colour; the selected phase has `❯` while
    /// the phases have the keys. Across the whole box, as a narrow screen draws them, the counts
    /// stand at its far end. `turn_ms` is how long the view has shown, which turns the mark.
    pub(super) fn lines(
        &self,
        data: &WorkflowData,
        (selected, focused): (usize, bool),
        width: usize,
        whole_box: bool,
        turn_ms: u64,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let text = theme.style(Role::Text);
        // The counts stand right-aligned in one column after every phase's name, eight cells at
        // least, as Claude Code lays its phases out, so a count never moves when another phase
        // gets its first agent. A narrow pane cuts the names before the counts, and drops the
        // counts last.
        let count = if self.count > 0 { 1 + self.count } else { 0 };
        let fixed = 2 + self.mark + 1 + self.required + count;
        let counted = (self.name + 1).max(NAME_COLUMN);
        let name_width = width.saturating_sub(fixed);
        let column = if whole_box {
            name_width
        } else {
            counted.min(name_width)
        };
        let show_count = self.count > 0 && width > fixed;
        data.phases
            .iter()
            .enumerate()
            .map(|(i, phase)| {
                let shown = match phase.status {
                    RowStatus::Running if turns(phase) => Some(RowStatus::Running),
                    RowStatus::Running => Some(RowStatus::Paused),
                    RowStatus::Done | RowStatus::Failed | RowStatus::NeedsYou => Some(phase.status),
                    _ => None,
                };
                let (mark, mark_role) = match shown {
                    Some(status) => {
                        let (mark, role) = glyph(status, false, theme);
                        (turned(mark, turn_ms, theme).to_owned(), role)
                    }
                    None => ((i + 1).to_string(), Role::Text),
                };
                let cursor = if i == selected && focused {
                    Span::styled(format!("{} ", marks.selected), theme.style(Role::Question))
                } else {
                    Span::raw("  ")
                };
                let mut spans = vec![
                    cursor,
                    Span::styled(left(&mark, self.mark), theme.style(mark_role)),
                    Span::raw(" "),
                ];
                if self.required > 0 {
                    let star = if data.requires(phase) {
                        marks.required
                    } else {
                        ""
                    };
                    spans.push(Span::styled(
                        left(star, self.required),
                        theme.style(Role::Accent),
                    ));
                }
                let name = cut(sanitize_line(&phase.name).trim(), name_width);
                if show_count && phase.count.is_some() {
                    spans.push(Span::styled(left(&name, column), text));
                    spans.push(Span::raw(" "));
                    let count = format!("{:>width$}", phase_count(phase), width = self.count);
                    spans.push(Span::styled(count, text));
                } else {
                    spans.push(Span::styled(name, text));
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// Whether a phase's mark turns: one of its rows is at work, or it runs with none of them held,
/// as between a review's rounds. A phase whose work is all held, as when its CLI is down, is
/// paused.
pub(super) fn turns(phase: &PhaseRow) -> bool {
    let any = |status: RowStatus| phase.rows.iter().any(|row| row_status(row) == status);
    any(RowStatus::Running) || (phase.status == RowStatus::Running && !any(RowStatus::Paused))
}

/// A phase's `done/total`, or nothing before it has agents, so no phase reads `0/0`.
fn phase_count(phase: &PhaseRow) -> String {
    phase
        .count
        .map(|count| format!("{}/{}", count.done, count.total))
        .unwrap_or_default()
}

/// One row's cells, before they're laid out.
pub(super) struct Cells {
    glyph: &'static str,
    glyph_role: Role,
    label: String,
    /// Whether the label is dim: an agent the run went on without once it wasn't needed.
    dim_label: bool,
    /// The logo of whoever serves the model and the blank after it, drawn before it. `None`
    /// where the model's place holds something else, such as a command or a workflow inside.
    logo: Option<Vec<Span<'static>>>,
    /// The model, a command such as `npm test`, `git`, or `workflow`.
    model: String,
    model_style: Style,
    /// ` on Codex`, dim after the model, for a model inside a CLI's own workflow whose CLI
    /// isn't that workflow's own.
    model_cli: Option<String>,
    /// What the model's place gives way to in a row too narrow for it: a workflow inside
    /// keeps its count of agents and lets go of whose it is.
    short_model: Option<String>,
    /// A square per line of the agent's goal, before the words after the model. Empty
    /// without a goal.
    squares: Vec<Span<'static>>,
    /// What follows the model, each after a dim ` · `: what no mark can say, such as where a
    /// goal stands, a command's result or its subagents, each with how long it holds.
    stats: Vec<(Span<'static>, Holds)>,
    /// `18.4k tok` at the right, before the time. Empty when it has none.
    tokens: String,
    /// Empty for a row that hasn't started.
    time: String,
}

/// How long a stat holds in a row too long for its pane: what an agent runs beside its job
/// gives way first, and the words that say how it stands hold longest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Holds {
    /// Its spend.
    Least,
    /// Its subagents and the page it published.
    Longer,
    /// Where its goal stands, whose review it needs, and a command's result.
    Longest,
}

impl Cells {
    /// A workflow inside drawn open, `▾`.
    pub(super) fn opened(mut self, row: &WorkflowRow, theme: &Theme) -> Self {
        if let WorkflowRow::Workflow(nested) = row {
            (self.glyph, self.glyph_role) = workflow_outline::mark(nested, true, theme);
        }
        self
    }
}

/// What a row is drawn from beside itself: its run, whose workflow it is, and the screen.
pub(super) struct Around<'a> {
    pub run: &'a RunId,
    /// The CLI whose own workflow this is, such as `claude-code`, when it is one.
    pub own_cli: Option<&'a str>,
    /// Milliseconds since the data was first drawn, which a running row's time adds.
    pub ticking: u64,
    pub ctx: &'a ViewCtx<'a>,
}

impl Cells {
    pub(super) fn of(row: &WorkflowRow, goal: Option<&AgentGoal>, around: &Around<'_>) -> Self {
        let ctx = around.ctx;
        let theme = ctx.theme;
        let names = &ctx.state.names;
        let dim = theme.style(Role::Dim);
        let (status, elapsed_ms) = match row {
            WorkflowRow::Agent(agent) => (agent.status, agent.elapsed_ms),
            WorkflowRow::Workflow(nested) => (nested.status, nested.elapsed_ms),
            WorkflowRow::Command(command) => (command.status, command.elapsed_ms),
        };
        let not_needed = matches!(row, WorkflowRow::Agent(agent) if agent.not_needed);
        let left_out = matches!(row, WorkflowRow::Agent(agent) if agent.left_out);
        let (glyph, glyph_role) = match row {
            // A job left out or not needed never ran to an end of its own: a grey dash.
            WorkflowRow::Agent(_) if left_out || not_needed => {
                glyph(RowStatus::Skipped, false, theme)
            }
            WorkflowRow::Agent(agent) => {
                let sent_back = agent
                    .note
                    .as_ref()
                    .filter(|_| agent.status == RowStatus::Done)
                    .and_then(|note| verdict_mark(note.tone, theme));
                sent_back.unwrap_or_else(|| glyph(status, agent.fallback_for.is_some(), theme))
            }
            WorkflowRow::Workflow(nested) => workflow_outline::mark(nested, false, theme),
            WorkflowRow::Command(_) => glyph(status, false, theme),
        };
        let glyph = turned(glyph, around.ticking, theme);
        // A role that answered for an agent still passing its answer on shows its own time,
        // which no longer grows.
        let answer_in =
            matches!(row, WorkflowRow::Agent(agent) if agent.answered && agent.note.is_some());
        let time = match status {
            _ if left_out || not_needed => String::new(),
            RowStatus::Waiting | RowStatus::Queued => String::new(),
            RowStatus::Running if !answer_in => panes::elapsed(elapsed_ms + around.ticking),
            _ => panes::elapsed(elapsed_ms),
        };
        let model_cli = match row {
            WorkflowRow::Agent(agent) => around
                .own_cli
                .filter(|own| *own != agent.worker.agent)
                .map(|_| format!(" on {}", sanitize_line(&names.cli(&agent.worker.agent)))),
            _ => None,
        };
        let mut stats = Vec::new();
        let mut squares = Vec::new();
        let mut tokens = String::new();
        let mut logo = None;
        let (model, model_style) = match row {
            // Why it wasn't needed, dim, in its model's place, with no numbers.
            WorkflowRow::Agent(agent) if not_needed => {
                let why = agent.note.as_ref().map_or("", |note| note.text.trim());
                (sanitize_line(why).into_owned(), dim)
            }
            WorkflowRow::Agent(agent) => {
                // An agent on a goal says where the goal stands after its model.
                if let Some(goal) = goal {
                    squares = goal_panel::squares(goal, theme);
                    stats.push((
                        Span::styled(goal_panel::row_words(goal), dim),
                        Holds::Longest,
                    ));
                } else if agent.tokens > 0 && !left_out {
                    tokens = panes::tokens(agent.tokens);
                }
                // `ui.workflow.row_cost` is the only way a row shows spend.
                let priced = ctx.state.ui.as_ref().is_some_and(|ui| ui.workflow.row_cost);
                if let Some(spend) = panes::spend(&agent.spent).filter(|_| priced) {
                    stats.push((Span::styled(spend, dim), Holds::Least));
                }
                stats.extend(agent_words(agent, around.run, ctx));
                let worker = &agent.worker;
                let label = ModelLabel::of(names, worker, theme, NoPicture::Dot);
                logo = Some(label.logo);
                let named = label.name;
                // The model it was meant for, when it was left out, is dim.
                let style = if left_out {
                    dim
                } else {
                    theme.style(Role::Text)
                };
                (sanitize_line(&named).into_owned(), style)
            }
            WorkflowRow::Workflow(nested) => {
                (workflow_outline::whose(nested, &|cli| names.cli(cli)), dim)
            }
            WorkflowRow::Command(command) => {
                if let Some(result) = command.result.as_deref() {
                    let result = sanitize_line(result);
                    if !result.trim().is_empty() {
                        stats.push((
                            Span::styled(
                                result.trim().to_owned(),
                                theme.style(tone_role(command.tone)),
                            ),
                            Holds::Longest,
                        ));
                    }
                }
                (
                    sanitize_line(&command.command).trim().to_owned(),
                    theme.style(Role::Text),
                )
            }
        };
        let short_model = match row {
            WorkflowRow::Workflow(nested) => Some(workflow_outline::agents_done(nested)),
            _ => None,
        };
        Self {
            glyph,
            glyph_role,
            dim_label: not_needed,
            label: row_label(row),
            logo,
            model,
            model_style,
            model_cli,
            short_model,
            squares,
            stats,
            tokens,
            time,
        }
    }
}

/// What follows an agent's model that no mark says: the earlier tries of a step, whose review
/// it still needs, its subagents and the page it published. Its verdict, its state and whose
/// place it took are the mark's, and the peek's.
fn agent_words(agent: &AgentRow, run: &RunId, ctx: &ViewCtx<'_>) -> Vec<(Span<'static>, Holds)> {
    let theme = ctx.theme;
    let names = &ctx.state.names;
    let dim = theme.style(Role::Dim);
    let mut words = Vec::new();
    if agent.note.is_none()
        && agent.fallback_for.is_none()
        && let Some(tried) = workflow_tries::summary(agent, names)
    {
        words.push((Span::styled(tried, dim), Holds::Longer));
    }
    if let Some(by) = review_needed_by(ctx, &agent.session) {
        words.push((review_needed_words(&by, names, theme), Holds::Longest));
    }
    // `ui.panel.agent_internals = hidden` leaves out what the agent runs inside an agent.
    if agent.subagents > 0 && cli_internals(ctx) != CliInternals::Hidden {
        let subagents = format!("+{}", panes::count(agent.subagents as usize, "subagent"));
        words.push((Span::styled(subagents, dim), Holds::Longer));
    }
    let page = ctx.state.artifacts.by_agent(run, &agent.session);
    if let Some(name) = page.map(|page| sanitize_line(&page.name).trim().to_owned())
        && !name.is_empty()
    {
        let mark = Marks::for_theme(theme).artifact;
        words.push((Span::styled(format!("{mark} {name}"), dim), Holds::Longer));
    }
    words
}

pub(super) fn row_status(row: &WorkflowRow) -> RowStatus {
    match row {
        WorkflowRow::Agent(agent) => agent.status,
        WorkflowRow::Workflow(nested) => nested.status,
        WorkflowRow::Command(command) => command.status,
    }
}

pub(super) fn row_label(row: &WorkflowRow) -> String {
    let label = match row {
        WorkflowRow::Agent(agent) => &agent.label,
        WorkflowRow::Workflow(nested) => &nested.name,
        WorkflowRow::Command(command) => &command.label,
    };
    sanitize_line(label).trim().to_owned()
}

/// The label column: the widest label of any row in the shown phase, a row inside a workflow
/// inside with its level's cells, so the models stay in one column as you open or fold what the
/// phase holds. Each phase is laid out by its own labels, as Claude Code lays out its own.
pub(super) fn label_width(phase: &PhaseRow) -> usize {
    workflow_outline::label_column(std::slice::from_ref(phase)).min(MAX_LABEL)
}

/// The agents pane's columns: a pointer, the mark, the label padded to the label column, the
/// logo with the model, then the tokens and the time together at the right edge, each
/// right-aligned in its own column so they line up down the pane. A row too long for its pane
/// lets go of the words after its model, then its tokens, and only then cuts its model, at a
/// word.
pub(super) struct RowColumns {
    width: usize,
    slot: usize,
    label: usize,
}

/// Blank cells between the tokens and the time's slot, and at least as many before the tokens.
const TOKENS_GAP: usize = 2;

/// The fewest cells a row's last words take to be cut to, ` · ` and `…` among them.
const CUT_WORDS: usize = 16;

impl RowColumns {
    /// Columns for a pane `width` cells wide, whose labels are at most `label` cells.
    pub(super) fn fit(label: usize, width: usize, theme: &Theme) -> Self {
        let slot = Marks::for_theme(theme).slot();
        // What Claude Code leaves the label in a narrow pane: the model and the time keep
        // their room first.
        let room = width.saturating_sub(1 + slot + 2);
        let most = MAX_LABEL
            .min(room.saturating_sub(30).max(22))
            .min(room.min(room.saturating_sub(TIME_SLOT + 1).max(12)).max(4));
        Self {
            width,
            slot,
            label: label.min(most),
        }
    }

    /// The row, `indent` cells further in than one of the phase's own, its label column ending
    /// where theirs does.
    pub(super) fn line(
        &self,
        cells: &Cells,
        indent: usize,
        selected: bool,
        theme: &Theme,
    ) -> Line<'static> {
        let label = self.label.saturating_sub(indent);
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let text = theme.style(Role::Text);
        let label_style = if cells.dim_label { dim } else { text };
        let mut spans = vec![
            if selected {
                Span::styled(marks.selected.to_owned(), theme.style(Role::Question))
            } else {
                Span::raw(" ")
            },
            Span::raw(" ".repeat(indent)),
            Span::styled(
                panes::pad_glyph(cells.glyph, self.slot),
                theme.style(cells.glyph_role),
            ),
            Span::raw(" "),
            Span::styled(left(&cut(&cells.label, label), label), label_style),
            Span::raw(" ".repeat(LABEL_GAP)),
        ];
        let logo: Vec<Span<'static>> = cells.logo.clone().unwrap_or_default();
        // The model and the words after it, each after a dim ` · `; `cut` gives one stat
        // shorter words in place of its own.
        let middle = |model: &str, shown: &[bool], cut: Option<(usize, &str)>| {
            let mut middle = logo.clone();
            middle.push(Span::styled(model.to_owned(), cells.model_style));
            if let Some(on) = &cells.model_cli {
                middle.push(Span::styled(on.clone(), dim));
            }
            for (at, ((stat, _), shown)) in cells.stats.iter().zip(shown).enumerate() {
                if !shown {
                    continue;
                }
                // The squares stand before a goal's words, where the first ` · ` ends.
                middle.push(Span::styled(" · ", dim));
                if at == 0 && !cells.squares.is_empty() {
                    middle.extend(cells.squares.iter().cloned());
                    middle.push(Span::raw(" "));
                }
                match cut {
                    Some((cut_at, words)) if cut_at == at => {
                        middle.push(Span::styled(words.to_owned(), stat.style));
                    }
                    _ => middle.push(stat.clone()),
                }
            }
            middle
        };
        // The time ends at the pane's edge in a slot every row keeps, and the tokens end two
        // cells before it.
        let room = self.width.saturating_sub(spans_width(&spans) + TIME_SLOT);
        let tokens_room = if cells.tokens.is_empty() {
            0
        } else {
            str_width(&cells.tokens) + 2 * TOKENS_GAP
        };
        // Whole stats only, those that hold longest first, each kind from the left, so a stat
        // that holds less never takes the place of one that holds more.
        let fill = |room: usize| {
            let mut shown = vec![false; cells.stats.len()];
            let mut order: Vec<usize> = (0..cells.stats.len()).collect();
            order.sort_by_key(|&at| (std::cmp::Reverse(cells.stats[at].1), at));
            for at in order {
                shown[at] = true;
                if spans_width(&middle(&cells.model, &shown, None)) > room {
                    shown[at] = false;
                }
            }
            shown
        };
        let every_longest = |shown: &[bool]| {
            cells
                .stats
                .iter()
                .zip(shown)
                .all(|((_, holds), shown)| *shown || *holds != Holds::Longest)
        };
        // The tokens go first when what the row says doesn't fit beside them.
        let mut with_tokens = tokens_room > 0;
        let mut shown = fill(room.saturating_sub(tokens_room));
        let model_fits = |room: usize| str_width(&cells.model) + spans_width(&logo) <= room;
        if with_tokens && (!every_longest(&shown) || !model_fits(room.saturating_sub(tokens_room)))
        {
            with_tokens = false;
            shown = fill(room);
        }
        let room = if with_tokens {
            room.saturating_sub(tokens_room)
        } else {
            room
        };
        // A goal's words stay with its squares, and where no word that says how a row stands
        // fits whole, the first is kept to be cut at a word, so long as a few of its words can
        // show.
        let mut forced = (!cells.squares.is_empty() && !shown.is_empty()).then_some(0);
        if forced.is_none()
            && let Some(at) =
                (0..cells.stats.len()).find(|&at| !shown[at] && cells.stats[at].1 == Holds::Longest)
        {
            let used = spans_width(&middle(&cells.model, &shown, None));
            if room.saturating_sub(used) >= CUT_WORDS {
                forced = Some(at);
            }
        }
        if let Some(at) = forced {
            shown[at] = true;
        }
        let mut model = cells.model.clone();
        let mut kept = middle(&model, &shown, None);
        if let Some(short) = cells.short_model.as_deref()
            && spans_width(&kept) > room
        {
            model = short.to_owned();
            kept = middle(&model, &shown, None);
        }
        if let Some(at) = forced.filter(|_| spans_width(&kept) > room) {
            let words = cells.stats[at].0.content.as_ref();
            let others = spans_width(&kept) - str_width(words);
            let words = cut_stat(words, room.saturating_sub(others));
            kept = middle(&model, &shown, Some((at, &words)));
        }
        // What still doesn't fit is the model's, cut at its last whole word.
        if spans_width(&kept) > room {
            let others = spans_width(&kept) - str_width(&model);
            model = cut_at_word(&model, room.saturating_sub(others));
            kept = middle(&model, &shown, None);
        }
        spans.extend(truncate_spans(&kept, room));
        if with_tokens {
            let right = self.width.saturating_sub(TIME_SLOT + TOKENS_GAP);
            let gap = right.saturating_sub(spans_width(&spans) + str_width(&cells.tokens));
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(cells.tokens.clone(), text));
        }
        if !cells.time.is_empty() {
            let gap = self
                .width
                .saturating_sub(spans_width(&spans) + str_width(&cells.time));
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(cells.time.clone(), text));
        }
        Line::from(spans)
    }
}

/// A stat cut at its last whole word, never on the dot between two of its parts.
fn cut_stat(text: &str, width: usize) -> String {
    let words = cut_at_word(text, width);
    match words.strip_suffix('…').map(str::trim_end) {
        Some(kept) if kept.ends_with('·') => {
            format!("{}…", kept.trim_end_matches('·').trim_end())
        }
        _ => words,
    }
}

/// Your models by the tokens each has run here, as every tab ranks them.
fn ranked<'a>(ctx: &ViewCtx<'a>) -> &'a [argonauts_core::overview::ModelTokens] {
    ctx.state
        .usage
        .as_ref()
        .map_or(&[][..], |usage| usage.all_time_models.as_slice())
}

/// The models a workflow's rows name, a fallback's first model among them, each with its place
/// among its CLI's models: the place it has on every other tab, the one that ran the most tokens
/// here first, then the CLI's others in the order of their names. A model keeps its place, and
/// so its color, whichever phase is open.
#[derive(Debug, Clone, Default)]
pub(super) struct Models(Vec<((String, String), usize)>);

impl Models {
    pub(super) fn of(data: &WorkflowData, ctx: &ViewCtx<'_>) -> Self {
        let names = &ctx.state.names;
        let mut shown: Vec<(String, String)> = data
            .phases
            .iter()
            .flat_map(|phase| &phase.rows)
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some(agent),
                _ => None,
            })
            .flat_map(|agent| std::iter::once(&agent.worker).chain(agent.fallback_for.as_ref()))
            .map(|worker| names.shown_as(&worker.agent, worker.model.as_deref()))
            .collect();
        shown.sort();
        shown.dedup();
        let places = ModelColors::new(ranked(ctx), names).with_named(
            shown
                .iter()
                .map(|(cli, name)| (cli.as_str(), name.as_str())),
        );
        let models = shown
            .into_iter()
            .map(|(cli, name)| {
                let place = places.place_named(&cli, &name);
                ((cli, name), place)
            })
            .collect();
        Self(models)
    }

    /// The model's place among its CLI's models, 0 for the CLI's own color. One no row names,
    /// as in an agent's view, takes its place among your models.
    fn place(&self, worker: &ModelRef, ctx: &ViewCtx<'_>) -> usize {
        let names = &ctx.state.names;
        let (cli, name) = names.shown_as(&worker.agent, worker.model.as_deref());
        match self
            .0
            .iter()
            .find(|((known, model), _)| *known == cli && *model == name)
        {
            Some((_, place)) => *place,
            None => ModelColors::new(ranked(ctx), names).place_named(&cli, &name),
        }
    }

    /// The color a model's name draws in, as on every tab: its CLI's hue for the CLI's three
    /// models that ran the most, and the grey of the rest after them. Its name tells it from
    /// the CLI's others.
    pub(super) fn style(&self, worker: &ModelRef, ctx: &ViewCtx<'_>) -> Style {
        let role = Role::for_cli(&worker.agent);
        if role == Role::CatOther {
            return ctx.theme.style(Role::Dim);
        }
        shade(&Kit::new(ctx.theme), role, self.place(worker, ctx))
    }
}

/// Every step at work in a phase, a nested workflow's included, so the workflow view and the
/// agent view title the phase alike. A step tried more than once is one.
pub(super) fn steps_in_phase(phase: &PhaseRow) -> usize {
    phase
        .rows
        .iter()
        .map(|row| match row {
            WorkflowRow::Agent(agent) if agent.left_out || agent.not_needed => 0,
            WorkflowRow::Agent(_) | WorkflowRow::Command(_) => 1,
            WorkflowRow::Workflow(nested) => nested.agents_total as usize,
        })
        .sum()
}

/// `2 agents` for a phase of agents, and `3 steps` once Argonauts Code's own steps, a check or
/// the merge, are among its rows.
pub(super) fn phase_size(phase: &PhaseRow) -> String {
    let has_steps = phase
        .rows
        .iter()
        .any(|row| matches!(row, WorkflowRow::Command(_)));
    let noun = if has_steps { "step" } else { "agent" };
    // A job the workflow does without isn't counted, nor an agent it went on without once it
    // wasn't needed, and the title says so once for each.
    let agents = |kind: fn(&AgentRow) -> bool| {
        phase
            .rows
            .iter()
            .filter(|row| matches!(row, WorkflowRow::Agent(agent) if kind(agent)))
            .count()
    };
    let (left_out, skipped) = (agents(|a| a.left_out), agents(|a| a.not_needed));
    let counted = steps_in_phase(phase);
    let mut size = Vec::new();
    // A phase whose only agent wasn't needed has none to count.
    if counted > 0 || skipped == 0 {
        size.push(panes::count(counted, noun));
    }
    if left_out > 0 {
        size.push(format!("{left_out} left out"));
    }
    if skipped > 0 {
        size.push(format!("{skipped} skipped"));
    }
    size.join(" · ")
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::WorkflowId;
    use argonauts_core::views::{NestedWorkflowRow, PhaseRow, RowStatus, WorkflowRow};
    use argonauts_core::workflow::StandardPhase;

    use ratatui::text::Span;

    use super::{Cells, Holds, RowColumns, phase_size, scroll_mark};
    use crate::theme::{Glyphs, Role, Theme};

    /// A row's cells: its label, its model, its stats, each with how long it holds, and its
    /// tokens.
    fn cells(label: &str, model: &str, stats: &[(&str, Holds)], tokens: &str) -> Cells {
        Cells {
            glyph: "✔",
            glyph_role: Role::StateDone,
            label: label.to_owned(),
            dim_label: false,
            logo: None,
            model: model.to_owned(),
            model_style: Default::default(),
            model_cli: None,
            short_model: None,
            squares: Vec::new(),
            stats: stats
                .iter()
                .map(|(text, holds)| (Span::raw((*text).to_owned()), *holds))
                .collect(),
            tokens: tokens.to_owned(),
            time: "31m45s".to_owned(),
        }
    }

    fn drawn(cells: &Cells, width: usize) -> String {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let line = RowColumns::fit(18, width, &theme).line(cells, 0, false, &theme);
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// In the agents pane of an 80-column screen, 49 cells wide, a row keeps its mark, its label
    /// and its time. What it runs beside its job gives way first, then its tokens, and only then
    /// is its model cut, at a word.
    #[test]
    fn at_80_columns_a_row_lets_go_of_its_tokens_before_its_model() {
        let local = cells(
            "build:d-plain-mode",
            "Qwen3 Coder 30B (64k)",
            &[],
            "102.4k tok",
        );
        assert_eq!(
            drawn(&local, 49),
            " ✔ build:d-plain-mode  Qwen3 Coder 30B…    31m45s"
        );
        let row = drawn(&local, 63);
        assert!(
            row.contains("Qwen3 Coder 30B (64k) ") && !row.contains("tok"),
            "{row:?}"
        );
        let row = drawn(&local, 70);
        assert!(
            row.contains("Qwen3 Coder 30B (64k) ") && row.ends_with(" 102.4k tok  31m45s"),
            "{row:?}"
        );

        let counted = cells(
            "worker · task-001",
            "Opus 5.5",
            &[("+3 subagents", Holds::Longer)],
            "402k tok",
        );
        let row = drawn(&counted, 49);
        assert!(
            row.contains("Opus 5.5 ") && row.contains("402k tok"),
            "{row:?}"
        );
        assert!(!row.contains("subagents"), "{row:?}");
        let row = drawn(&counted, 64);
        assert!(row.contains("Opus 5.5 · +3 subagents "), "{row:?}");

        // What says how a row stands keeps its place over the tokens.
        let owed = cells(
            "build:due-flag",
            "Sonnet 5.5",
            &[("needs a review by Opus 5.5", Holds::Longest)],
            "44.0k tok",
        );
        let row = drawn(&owed, 70);
        assert!(
            row.contains("Sonnet 5.5 · needs a review by Opus 5.5 "),
            "{row:?}"
        );
        assert!(!row.contains("tok"), "{row:?}");
    }

    /// A pane's title counts the agents of a workflow inside, a CLI's own among them, as its
    /// phase does.
    #[test]
    fn a_pane_title_counts_the_agents_of_a_workflow_inside() {
        let cli_workflow = NestedWorkflowRow {
            id: WorkflowId::numbered(2),
            name: "add-due-flag".to_owned(),
            status: RowStatus::Running,
            agents_done: 1,
            agents_total: 3,
            workers: Vec::new(),
            elapsed_ms: 0,
            parent: Some(argonauts_core::names::SessionKey::numbered(1)),
            worker: Some("claude-code".to_owned()),
            phases: Vec::new(),
            rows: Vec::new(),
        };
        let phase = PhaseRow {
            name: "Build".to_owned(),
            detail: String::new(),
            standard: StandardPhase::Do,
            status: RowStatus::Running,
            try_number: None,
            count: None,
            rows: vec![WorkflowRow::Workflow(cli_workflow)],
            note: None,
        };
        assert_eq!(phase_size(&phase), "3 agents");
    }

    /// Claude Code's mark on a list that scrolls draws an arrow only on the side where rows
    /// are hidden, so the last rows of a list end the mark with a blank, and a list that
    /// fits has no mark.
    #[test]
    fn the_scroll_mark_points_only_where_rows_are_hidden() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        assert_eq!(scroll_mark(0, 5, 6, &theme), "   1–5 of 6 ↓");
        assert_eq!(scroll_mark(1, 5, 6, &theme), "↑  2–5 of 6 ↓");
        assert_eq!(scroll_mark(1, 6, 6, &theme), "↑  2–6 of 6  ");
        assert_eq!(scroll_mark(0, 6, 6, &theme), "");
    }
}
