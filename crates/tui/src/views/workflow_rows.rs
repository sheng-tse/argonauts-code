//! The rows of the workflow view, laid out as Claude Code's workflow detail view lays them out.
//! A phase is its cursor, its number or mark, its name, and `done/total`. An agent is its
//! pointer, its state glyph, its label, `model · stats`, and its time at the right edge. A step
//! Argonauts Code runs itself, a check or the merge, is a row of the same shape with the
//! command or `git` where the model would be.
//!
//! Argonauts Code adds one thing to a row: words after the stats, such as `approved`,
//! `sent it back · 2 findings`, or `for Opus 5.5 · overloaded` on a fallback.

use argonauts_core::RunId;
use argonauts_core::agent_goal::AgentGoal;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::ui::CliInternals;
use argonauts_core::views::{
    AgentRow, PhaseRow, RowStatus, Tone, WorkflowRow, WorkflowView as WorkflowData,
};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::kit::State;
use crate::overview::shades::ModelColors;
use crate::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::{Brightness, Role, Theme};

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

/// A row's state glyph and the role it draws in: `✔` done, `✘` failed, a grey `✘` skipped, a
/// grey `⏺` running, `◌` queued or not started. A fallback at work draws `↷`, and a paused
/// agent `◌`, both in the warning color.
pub(super) fn glyph(status: RowStatus, fallback: bool, theme: &Theme) -> (&'static str, Role) {
    let ascii = theme.glyphs.is_ascii();
    let pick = |unicode: &'static str, plain: &'static str| if ascii { plain } else { unicode };
    match status {
        RowStatus::Done => (pick("✔", "*"), Role::StateDone),
        RowStatus::Failed => (pick("✘", "x"), Role::StateFailed),
        RowStatus::Skipped => (pick("✘", "x"), Role::Faint),
        RowStatus::Running if fallback => (pick("↷", "~"), Role::Warn),
        RowStatus::Running => (pick("⏺", ">"), Role::Faint),
        RowStatus::Paused => (pick("◌", "o"), Role::Warn),
        RowStatus::Waiting | RowStatus::Queued => (pick("◌", "o"), Role::Faint),
        RowStatus::NeedsYou => states::mark(State::NeedsYou, theme),
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

/// `↑ 2–6 of 9 ↓` on the box's bottom edge while a list scrolls, each arrow only where rows
/// are hidden that way. Nothing when the list fits.
pub(crate) fn scroll_mark(from: usize, to: usize, total: usize, theme: &Theme) -> String {
    if total <= to.saturating_sub(from) {
        return String::new();
    }
    let marks = Marks::for_theme(theme);
    let dash = if theme.glyphs.is_ascii() { "-" } else { "–" };
    let up = if from > 0 { marks.up } else { " " };
    let down = if to < total { marks.down } else { " " };
    format!("{up} {}{dash}{to} of {total} {down}", from + 1)
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

    /// Every phase numbered from 1 in subtle grey, with `✔` or `✘` in the number's place once
    /// it ended. The selected phase's name and count are in the accent color, with `❯` while
    /// the phases have the keys. A phase with no agents yet is dim. Across the whole box, as a
    /// narrow screen draws them, the counts stand at its far end.
    pub(super) fn lines(
        &self,
        data: &WorkflowData,
        (selected, focused): (usize, bool),
        width: usize,
        whole_box: bool,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let accent = theme.style(Role::Question);
        // The counts line up in one column after the names that have them, eight cells at
        // least, as Claude Code lays its phases out; a longer name the pane makes room for
        // runs on past them. A narrow pane cuts the names before the counts, and drops the
        // counts last.
        let count = if self.count > 0 { 1 + self.count } else { 0 };
        let fixed = 2 + self.mark + 1 + self.required + count;
        let counted = data
            .phases
            .iter()
            .filter(|phase| phase.count.is_some())
            .map(|phase| str_width(sanitize_line(&phase.name).trim()) + 1)
            .max()
            .unwrap_or(0)
            .max(NAME_COLUMN);
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
                let picked = i == selected;
                let (mark, mark_role) = match phase.status {
                    RowStatus::Done | RowStatus::Failed | RowStatus::NeedsYou => {
                        let (mark, role) = glyph(phase.status, false, theme);
                        (mark.to_owned(), role)
                    }
                    _ => ((i + 1).to_string(), Role::Faint),
                };
                let words = if picked {
                    accent
                } else if phase.status == RowStatus::Waiting && phase.count.is_none() {
                    theme.style(Role::Dim)
                } else {
                    theme.style(Role::Text)
                };
                let cursor = if picked && focused {
                    Span::styled(format!("{} ", marks.selected), accent)
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
                    spans.push(Span::styled(left(&name, column), words));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(phase_count(phase), words));
                } else {
                    spans.push(Span::styled(name, words));
                }
                Line::from(spans)
            })
            .collect()
    }
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
    /// Whether the glyph is the grey one of a row at work, whose label is dim with it.
    at_work: bool,
    /// The model, a command such as `npm test`, `git`, or `workflow`.
    model: String,
    model_style: Style,
    /// ` on Codex`, dim after the model, for a model inside a CLI's own workflow whose CLI
    /// isn't that workflow's own.
    model_cli: Option<String>,
    /// What the model's place gives way to in a row too narrow for it: a workflow inside
    /// keeps its count of agents and lets go of whose it is.
    short_model: Option<String>,
    /// A square per line of the agent's goal, before the stats. Empty without a goal.
    squares: Vec<Span<'static>>,
    /// What follows the model, each after a dim ` · `: the tokens, a state word as Claude
    /// Code has them, and the words Argonauts Code adds, each with how long it holds.
    stats: Vec<(Span<'static>, Holds)>,
    /// Empty for a row that hasn't started.
    time: String,
}

/// How long a stat holds in a row too long for its pane: the counts give way first, then what
/// an agent runs beside its job, and the words that say how it stands hold longest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Holds {
    /// Its tokens and spend.
    Least,
    /// Its subagents and the page it published.
    Longer,
    /// Its state word or verdict, whom it stands in for, why it was left out, where its goal
    /// stands, and a command's result.
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

/// What a row is drawn from beside itself: its run, its workflow's models, and the screen.
pub(super) struct Around<'a> {
    pub run: &'a RunId,
    /// The CLI whose own workflow this is, such as `claude-code`, when it is one.
    pub own_cli: Option<&'a str>,
    pub models: &'a Models,
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
        let (glyph, glyph_role) = match row {
            // A job left out never ran: a grey cross.
            WorkflowRow::Agent(agent) if agent.left_out => {
                (glyph(RowStatus::Failed, false, theme).0, Role::Faint)
            }
            WorkflowRow::Agent(agent) => glyph(status, agent.fallback_for.is_some(), theme),
            WorkflowRow::Workflow(nested) => workflow_outline::mark(nested, false, theme),
            WorkflowRow::Command(_) => glyph(status, false, theme),
        };
        // A role that answered for an agent still passing its answer on shows its own time,
        // which no longer grows.
        let answer_in =
            matches!(row, WorkflowRow::Agent(agent) if agent.answered && agent.note.is_some());
        let left_out = matches!(row, WorkflowRow::Agent(agent) if agent.left_out);
        let time = match status {
            _ if left_out => String::new(),
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
        let (model, model_style) = match row {
            // The model it was meant for, dim, and why it was left out, with no numbers.
            WorkflowRow::Agent(agent) if agent.left_out => {
                let why = agent
                    .note
                    .as_ref()
                    .map(|note| sanitize_line(note.text.trim()).into_owned())
                    .filter(|why| !why.is_empty());
                let said = match why {
                    Some(why) => format!("left out · {why}"),
                    None => "left out".to_owned(),
                };
                stats.push((Span::styled(said, dim), Holds::Longest));
                (sanitize_line(&names.short(&agent.worker)).into_owned(), dim)
            }
            WorkflowRow::Agent(agent) => {
                // An agent on a goal says where the goal stands, where its tokens would be.
                match goal {
                    Some(goal) => {
                        squares = goal_panel::squares(goal, theme);
                        stats.push((
                            Span::styled(goal_panel::row_words(goal), dim),
                            Holds::Longest,
                        ));
                    }
                    None if agent.tokens > 0 => {
                        stats.push((Span::styled(panes::tokens(agent.tokens), dim), Holds::Least));
                    }
                    None => {}
                }
                // `ui.workflow.row_cost` is the only way a row shows spend.
                let priced = ctx.state.ui.as_ref().is_some_and(|ui| ui.workflow.row_cost);
                if let Some(spend) = panes::spend(&agent.spent).filter(|_| priced) {
                    stats.push((Span::styled(spend, dim), Holds::Least));
                }
                stats.extend(agent_words(agent, around.run, ctx));
                (
                    sanitize_line(&names.short(&agent.worker)).into_owned(),
                    around.models.style(&agent.worker, ctx),
                )
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
            at_work: status == RowStatus::Running && glyph_role == Role::Faint,
            label: row_label(row),
            model,
            model_style,
            model_cli,
            short_model,
            squares,
            stats,
            time,
        }
    }
}

/// What follows an agent's tokens: Claude Code's own state words, then what Argonauts Code
/// adds, a fallback's reason or the row's note first, each with how long it holds.
fn agent_words(agent: &AgentRow, run: &RunId, ctx: &ViewCtx<'_>) -> Vec<(Span<'static>, Holds)> {
    let theme = ctx.theme;
    let names = &ctx.state.names;
    let dim = theme.style(Role::Dim);
    let mut words = Vec::new();
    // Claude Code's word for the state, unless Argonauts Code says more of it below.
    let said = agent.note.is_some() || agent.fallback_for.is_some();
    match agent.status {
        RowStatus::Queued => words.push(Span::styled("queued", dim)),
        RowStatus::Skipped if !said => words.push(Span::styled("skipped", dim)),
        RowStatus::Failed if !said => words.push(Span::styled("failed", dim)),
        _ => {}
    }
    let note = agent
        .note
        .as_ref()
        .map(|note| (sanitize_line(note.text.trim()).into_owned(), note.tone))
        .filter(|(text, _)| !text.is_empty());
    match (&agent.fallback_for, note) {
        // While it works the fallback says why the model it takes over from couldn't; once it
        // answers, its verdict takes the reason's place.
        (Some(first), Some((verdict, tone))) if agent.status == RowStatus::Done => {
            let first = sanitize_line(&names.short(first)).into_owned();
            words.push(Span::styled(
                format!("for {first}"),
                theme.style(Role::Warn),
            ));
            words.push(Span::styled(verdict, theme.style(tone_role(tone))));
            let then = agent.note.iter().flat_map(|note| &note.then);
            for more in then.map(|more| sanitize_line(more.trim()).into_owned()) {
                if !more.is_empty() {
                    words.push(Span::styled(more, dim));
                }
            }
        }
        (Some(first), note) => {
            let mut text = format!("for {}", sanitize_line(&names.short(first)));
            if let Some((why, _)) = note {
                text.push_str(&format!(" · {why}"));
            }
            words.push(Span::styled(text, theme.style(Role::Warn)));
        }
        (None, Some((text, tone))) => {
            words.push(Span::styled(text, theme.style(tone_role(tone))));
            let then = agent.note.iter().flat_map(|note| &note.then);
            for more in then.map(|more| sanitize_line(more.trim()).into_owned()) {
                if !more.is_empty() {
                    words.push(Span::styled(more, dim));
                }
            }
        }
        (None, None) => {
            // A step tried more than once says so here, where nothing else does.
            if let Some(tried) = workflow_tries::summary(agent, names) {
                words.push(Span::styled(tried, dim));
            }
        }
    }
    if let Some(by) = review_needed_by(ctx, &agent.session) {
        words.push(review_needed_words(&by, names, theme));
    }
    // `ui.panel.agent_internals = hidden` leaves out what the agent runs inside an agent.
    let mut words: Vec<(Span<'static>, Holds)> = words
        .into_iter()
        .map(|word| (word, Holds::Longest))
        .collect();
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

/// The agents pane's columns, as Claude Code's rows: a pointer, the glyph, the label padded to
/// the label column, the model, ` · ` before each stat, and the time flush right. A row too
/// long for the pane loses its stats by how long each holds, then its model.
pub(super) struct RowColumns {
    width: usize,
    slot: usize,
    label: usize,
}

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
        let accent = theme.style(Role::Question);
        let dim = theme.style(Role::Dim);
        let label_style = match (selected, cells.at_work) {
            (true, _) => accent,
            (false, true) => dim,
            (false, false) => theme.style(Role::Text),
        };
        let mut spans = vec![
            if selected {
                Span::styled(marks.selected.to_owned(), accent)
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
        // The time ends at the pane's edge, and the stats keep one blank cell before it.
        let time = cells.time.clone();
        let fixed = spans_width(&spans)
            + if time.is_empty() {
                0
            } else {
                1 + str_width(&time)
            };
        // `cut` gives one stat shorter words in place of its own.
        let rest = |model: &str, shown: &[bool], cut: Option<(usize, &str)>| {
            let mut rest = vec![Span::styled(model.to_owned(), cells.model_style)];
            if let Some(on) = &cells.model_cli {
                rest.push(Span::styled(on.clone(), dim));
            }
            for (at, ((stat, _), shown)) in cells.stats.iter().zip(shown).enumerate() {
                if !shown {
                    continue;
                }
                // The squares stand before a goal's words, where the first ` · ` ends.
                rest.push(Span::styled(" · ", dim));
                if at == 0 && !cells.squares.is_empty() {
                    rest.extend(cells.squares.iter().cloned());
                    rest.push(Span::raw(" "));
                }
                match cut {
                    Some((cut_at, words)) if cut_at == at => {
                        rest.push(Span::styled(words.to_owned(), stat.style));
                    }
                    _ => rest.push(stat.clone()),
                }
            }
            rest
        };
        // A row too long for the pane shows whole stats only, those that hold longest first,
        // each kind from the left, so a stat that holds less never takes the place of one that
        // holds more. Then it lets go of what its model's place can do without, and only then
        // cuts what is left at a word. Where no word that says how the agent stands fits whole,
        // the first is kept to be cut, as a goal's squares keep its words.
        let room = self.width.saturating_sub(fixed);
        let mut shown = vec![false; cells.stats.len()];
        // A goal's words stay with its squares, to be cut after them.
        let mut forced = (!cells.squares.is_empty() && !shown.is_empty()).then_some(0);
        if let Some(at) = forced {
            shown[at] = true;
        }
        let mut order: Vec<usize> = (0..cells.stats.len()).collect();
        order.sort_by_key(|&at| (std::cmp::Reverse(cells.stats[at].1), at));
        for at in order {
            if shown[at] {
                continue;
            }
            shown[at] = true;
            if spans_width(&rest(&cells.model, &shown, None)) > room {
                shown[at] = false;
            }
        }
        if !shown.contains(&true) {
            forced = cells
                .stats
                .iter()
                .position(|(_, holds)| *holds == Holds::Longest);
            if let Some(at) = forced {
                shown[at] = true;
            }
        }
        let mut model = cells.model.as_str();
        let mut kept = rest(model, &shown, None);
        if let Some(short) = cells.short_model.as_deref()
            && spans_width(&kept) > room
        {
            model = short;
            kept = rest(model, &shown, None);
        }
        // What was kept to be cut ends at its last whole word.
        if let Some(at) = forced.filter(|_| spans_width(&kept) > room) {
            let text = cells.stats[at].0.content.as_ref();
            let others = spans_width(&kept) - str_width(text);
            let words = cut_stat(text, room.saturating_sub(others));
            kept = rest(model, &shown, Some((at, &words)));
        }
        spans.extend(truncate_spans(&kept, room));
        if !time.is_empty() {
            let used = spans_width(&spans);
            let gap = self.width.saturating_sub(used + str_width(&time));
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(time, dim));
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

/// The shades a CLI's models take on a dark screen after the CLI's own color.
const CLI_SHADES: [(Role, [u32; 3]); 3] = [
    (Role::CliClaude, [0xb4472a, 0xf3b27a, 0xf8dccb]),
    (Role::CliCodex, [0x2e5fc0, 0xa8cdff, 0x3fc4f0]),
    (Role::CliAgy, [0x1b8a9c, 0x93e8f3, 0x4fe0c0]),
];

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

    /// Each model that did a row's job, by name, with the style its row draws it in, for the
    /// lines that name them. The model a fallback was meant for isn't one.
    pub(super) fn named(&self, data: &WorkflowData, ctx: &ViewCtx<'_>) -> Vec<(String, Style)> {
        let mut named: Vec<(String, Style)> = Vec::new();
        let agents = data
            .phases
            .iter()
            .flat_map(|phase| &phase.rows)
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some(agent),
                _ => None,
            });
        for agent in agents {
            let name = ctx.state.names.short(&agent.worker);
            if !named.iter().any(|(known, _)| *known == name) {
                named.push((name, self.style(&agent.worker, ctx)));
            }
        }
        named
    }

    /// The color a model's name draws in: its CLI's own for the first of the CLI's models, and
    /// a shade of it for each one after, so two models of one CLI read apart.
    pub(super) fn style(&self, worker: &ModelRef, ctx: &ViewCtx<'_>) -> Style {
        let theme = ctx.theme;
        let role = Role::for_cli(&worker.agent);
        if role == Role::CatOther {
            return theme.style(Role::Dim);
        }
        let own = theme.style(role);
        let nth = self.place(worker, ctx);
        let shades = CLI_SHADES
            .iter()
            .find(|(cli, _)| *cli == role)
            .map(|(_, shades)| shades);
        match (nth, shades) {
            (0, _) | (_, None) => own,
            _ if theme.brightness != Brightness::Dark || !theme.has_color() => own,
            (nth, Some(shades)) => {
                let [_, r, g, b] = shades[(nth - 1) % shades.len()].to_be_bytes();
                own.fg(Color::Rgb(r, g, b))
            }
        }
    }
}

/// Every step at work in a phase, a nested workflow's included, so the workflow view and the
/// agent view title the phase alike. A step tried more than once is one.
pub(super) fn steps_in_phase(phase: &PhaseRow) -> usize {
    phase
        .rows
        .iter()
        .map(|row| match row {
            WorkflowRow::Agent(agent) if agent.left_out => 0,
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
    let mut size = panes::count(steps_in_phase(phase), noun);
    // A job the workflow does without isn't counted, and the title says so once.
    let left_out = phase
        .rows
        .iter()
        .filter(|row| matches!(row, WorkflowRow::Agent(agent) if agent.left_out))
        .count();
    if left_out > 0 {
        size.push_str(&format!(" · {left_out} left out"));
    }
    size
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::WorkflowId;
    use argonauts_core::views::{NestedWorkflowRow, PhaseRow, RowStatus, WorkflowRow};
    use argonauts_core::workflow::StandardPhase;

    use ratatui::text::Span;

    use super::{Cells, Holds, RowColumns, phase_size, scroll_mark};
    use crate::theme::{Glyphs, Role, Theme};

    /// A row's cells: its label, its model, and its stats, each with how long it holds.
    fn cells(glyph: &'static str, label: &str, model: &str, stats: &[(&str, Holds)]) -> Cells {
        Cells {
            glyph,
            glyph_role: Role::Dim,
            label: label.to_owned(),
            at_work: false,
            model: model.to_owned(),
            model_style: Default::default(),
            model_cli: None,
            short_model: None,
            squares: Vec::new(),
            stats: stats
                .iter()
                .map(|(text, holds)| (Span::raw((*text).to_owned()), *holds))
                .collect(),
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

    /// In the agents pane of an 80-column screen, 49 cells wide, a row too long for it gives up
    /// its counts before what it is there to say: a failed row keeps its state word, a moved row
    /// whom it stands in for, and what an agent runs beside its job goes before either. In a
    /// narrower pane a count that fits where nothing that holds longer does still shows. What
    /// nothing else can make room for is cut at a word.
    #[test]
    fn at_80_columns_a_row_keeps_what_it_says_over_its_counts() {
        let failed = cells(
            "✘",
            "build:d-plain-mode",
            "GPT 5.6",
            &[("95.2k tok", Holds::Least), ("failed", Holds::Longest)],
        );
        let row = drawn(&failed, 49);
        assert!(row.contains("GPT 5.6 · failed "), "{row:?}");
        assert!(!row.contains("tok"), "{row:?}");

        let moved = cells(
            "↷",
            "worker · task-002",
            "GPT 5.6",
            &[
                ("181k tok", Holds::Least),
                ("for Opus", Holds::Longest),
                ("+2 subagents", Holds::Longer),
            ],
        );
        let row = drawn(&moved, 49);
        assert!(row.contains("GPT 5.6 · for Opus "), "{row:?}");
        assert!(
            !row.contains("tok") && !row.contains("subagents"),
            "{row:?}"
        );
        // With room for one more, the subagents come before the tokens.
        let row = drawn(&moved, 64);
        assert!(
            row.contains("GPT 5.6 · for Opus · +2 subagents "),
            "{row:?}"
        );

        let counted = cells(
            "✔",
            "worker · task-001",
            "Opus",
            &[("402k tok", Holds::Least), ("+3 subagents", Holds::Longer)],
        );
        let row = drawn(&counted, 49);
        assert!(row.contains("Opus · +3 subagents "), "{row:?}");
        let row = drawn(&counted, 45);
        assert!(row.contains("Opus · 402k tok "), "{row:?}");

        let left_out = cells(
            "✘",
            "review:security",
            "Gemini 3.6",
            &[("left out · its CLI can't start here", Holds::Longest)],
        );
        let row = drawn(&left_out, 49);
        assert!(row.contains("Gemini 3.6 · left… "), "{row:?}");
        let row = drawn(&left_out, 56);
        assert!(row.contains("Gemini 3.6 · left out… "), "{row:?}");
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
        assert_eq!(scroll_mark(0, 5, 6, &theme), "  1–5 of 6 ↓");
        assert_eq!(scroll_mark(1, 5, 6, &theme), "↑ 2–5 of 6 ↓");
        assert_eq!(scroll_mark(1, 6, 6, &theme), "↑ 2–6 of 6  ");
        assert_eq!(scroll_mark(0, 6, 6, &theme), "");
    }
}
