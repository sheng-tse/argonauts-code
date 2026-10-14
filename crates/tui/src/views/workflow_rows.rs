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
use crate::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::{Brightness, Role, Theme};

use super::ViewCtx;
use super::agent::{cli_internals, needs_review};
use super::goal_panel;
use super::layout::{cut, left};
use super::panes::{self, Marks};
use super::states;
use super::workflow_tries;

/// Phase names and row labels past these are cut, so one long name can't take the pane.
const MAX_PHASE_NAME: usize = 24;
const MAX_LABEL: usize = 40;

/// The Phases pane is never narrower or wider than these, as in Claude Code.
const LEAST_PHASES: usize = 14;
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
    /// the phases have the keys. A phase with no agents yet is dim.
    pub(super) fn lines(
        &self,
        data: &WorkflowData,
        selected: usize,
        focused: bool,
        width: usize,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let accent = theme.style(Role::Question);
        // The count ends at the pane's edge, and the name has the cells before it. A narrow
        // pane cuts the names before the counts, and drops the counts last.
        let count = if self.count > 0 { 1 + self.count } else { 0 };
        let fixed = 2 + self.mark + 1 + self.required + count;
        let name_width = width.saturating_sub(fixed);
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
                spans.push(Span::styled(left(&name, name_width), words));
                if show_count {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(left(&phase_count(phase), self.count), words));
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
    /// A square per line of the agent's goal, before the stats. Empty without a goal.
    squares: Vec<Span<'static>>,
    /// What follows the model, each after a dim ` · `: the tokens, a state word as Claude
    /// Code has them, and the words Argonauts Code adds.
    stats: Vec<Span<'static>>,
    /// Empty for a row that hasn't started.
    time: String,
}

/// What a row is drawn from beside itself: its run, its workflow's models, and the screen.
pub(super) struct Around<'a> {
    pub run: &'a RunId,
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
            WorkflowRow::Agent(agent) => glyph(status, agent.fallback_for.is_some(), theme),
            WorkflowRow::Workflow(_) => (Marks::for_theme(theme).nested, Role::Question),
            WorkflowRow::Command(_) => glyph(status, false, theme),
        };
        let time = match status {
            RowStatus::Waiting | RowStatus::Queued => String::new(),
            RowStatus::Running => panes::elapsed(elapsed_ms + around.ticking),
            _ => panes::elapsed(elapsed_ms),
        };
        let mut stats = Vec::new();
        let mut squares = Vec::new();
        let (model, model_style) = match row {
            WorkflowRow::Agent(agent) => {
                // An agent on a goal says where the goal stands, where its tokens would be.
                match goal {
                    Some(goal) => {
                        squares = goal_panel::squares(goal, theme);
                        stats.push(Span::styled(goal_panel::row_words(goal), dim));
                    }
                    None if agent.tokens > 0 => {
                        stats.push(Span::styled(panes::tokens(agent.tokens), dim));
                    }
                    None => {}
                }
                // `ui.workflow.row_cost` is the only way a row shows spend.
                let priced = ctx.state.ui.as_ref().is_some_and(|ui| ui.workflow.row_cost);
                if let Some(spend) = panes::spend(&agent.spent).filter(|_| priced) {
                    stats.push(Span::styled(spend, dim));
                }
                stats.extend(agent_words(agent, around.run, ctx));
                (
                    sanitize_line(&names.short(&agent.worker)).into_owned(),
                    around.models.style(&agent.worker, ctx),
                )
            }
            WorkflowRow::Workflow(nested) => {
                let (whose, noun) = match nested.worker.as_deref() {
                    Some(worker) => (format!("{}'s own", names.cli(worker)), "agents"),
                    None => ("workflow".to_owned(), "steps"),
                };
                stats.push(Span::styled(
                    format!("{}/{} {noun}", nested.agents_done, nested.agents_total),
                    dim,
                ));
                (whose, dim)
            }
            WorkflowRow::Command(command) => {
                if let Some(result) = command.result.as_deref() {
                    let result = sanitize_line(result);
                    if !result.trim().is_empty() {
                        stats.push(Span::styled(
                            result.trim().to_owned(),
                            theme.style(tone_role(command.tone)),
                        ));
                    }
                }
                (
                    sanitize_line(&command.command).trim().to_owned(),
                    theme.style(Role::Text),
                )
            }
        };
        Self {
            glyph,
            glyph_role,
            at_work: status == RowStatus::Running && glyph_role == Role::Faint,
            // A workflow an agent's own CLI runs hangs under that agent's row.
            label: match row {
                WorkflowRow::Workflow(nested) if nested.parent.is_some() => {
                    format!("{} {}", theme.glyphs.result, row_label(row))
                }
                _ => row_label(row),
            },
            model,
            model_style,
            squares,
            stats,
            time,
        }
    }
}

/// What follows an agent's tokens: Claude Code's own state words, then what Argonauts Code
/// adds, a fallback's reason or the row's note first.
fn agent_words(agent: &AgentRow, run: &RunId, ctx: &ViewCtx<'_>) -> Vec<Span<'static>> {
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
        (Some(first), note) => {
            let mut text = format!("for {}", sanitize_line(&names.short(first)));
            if let Some((why, _)) = note {
                text.push_str(&format!(" · {why}"));
            }
            words.push(Span::styled(text, theme.style(Role::Warn)));
        }
        (None, Some((text, tone))) => {
            words.push(Span::styled(text, theme.style(tone_role(tone))));
        }
        (None, None) => {
            // A step tried more than once says so here, where nothing else does.
            if let Some(tried) = workflow_tries::summary(agent, names) {
                words.push(Span::styled(tried, dim));
            }
        }
    }
    if needs_review(ctx, &agent.session) {
        let mark = Marks::for_theme(theme).debt;
        words.push(Span::styled(
            format!("{mark} needs review"),
            theme.style(Role::Warn),
        ));
    }
    // `ui.panel.agent_internals = hidden` leaves out what the agent runs inside an agent.
    if agent.subagents > 0 && cli_internals(ctx) != CliInternals::Hidden {
        words.push(Span::styled(
            format!("+{}", panes::count(agent.subagents as usize, "subagent")),
            dim,
        ));
    }
    let page = ctx.state.artifacts.by_agent(run, &agent.session);
    if let Some(name) = page.map(|page| sanitize_line(&page.name).trim().to_owned())
        && !name.is_empty()
    {
        let mark = Marks::for_theme(theme).artifact;
        words.push(Span::styled(format!("{mark} {name}"), dim));
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

/// The label column: the widest label of any row in any phase, so the models stay in one
/// column as you move from phase to phase.
pub(super) fn label_width(data: &WorkflowData) -> usize {
    data.phases
        .iter()
        .flat_map(|phase| &phase.rows)
        .map(|row| str_width(&row_label(row)))
        .max()
        .unwrap_or(0)
        .min(MAX_LABEL)
}

/// The agents pane's columns, as Claude Code's rows: a pointer, the glyph, the label padded to
/// the label column, the model, ` · ` before each stat, and the time flush right. A row too
/// long for the pane loses its stats from the right, then its model.
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

    pub(super) fn line(&self, cells: &Cells, selected: bool, theme: &Theme) -> Line<'static> {
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
            Span::styled(
                panes::pad_glyph(cells.glyph, self.slot),
                theme.style(cells.glyph_role),
            ),
            Span::raw(" "),
            Span::styled(
                left(&cut(&cells.label, self.label), self.label),
                label_style,
            ),
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
        let mut rest = vec![Span::styled(cells.model.clone(), cells.model_style)];
        for (at, stat) in cells.stats.iter().enumerate() {
            // The squares stand before a goal's words, where the first ` · ` ends.
            rest.push(Span::styled(" · ", dim));
            if at == 0 && !cells.squares.is_empty() {
                rest.extend(cells.squares.iter().cloned());
                rest.push(Span::raw(" "));
            }
            rest.push(stat.clone());
        }
        spans.extend(truncate_spans(&rest, self.width.saturating_sub(fixed)));
        if !time.is_empty() {
            let used = spans_width(&spans);
            let gap = self.width.saturating_sub(used + str_width(&time));
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(time, dim));
        }
        Line::from(spans)
    }
}

/// The shades a CLI's models take on a dark screen after the CLI's own color.
const CLI_SHADES: [(Role, [u32; 3]); 3] = [
    (Role::CliClaude, [0xb4472a, 0xf3b27a, 0xf8dccb]),
    (Role::CliCodex, [0x2e5fc0, 0xa8cdff, 0x3fc4f0]),
    (Role::CliAgy, [0x1b8a9c, 0x93e8f3, 0x4fe0c0]),
];

/// The models a workflow's rows name, a fallback's first model among them, each CLI's in the
/// order of their names. A model keeps its place, and so its color, whichever phase is open.
#[derive(Debug, Clone, Default)]
pub(super) struct Models(Vec<(String, String)>);

impl Models {
    pub(super) fn of(data: &WorkflowData, ctx: &ViewCtx<'_>) -> Self {
        let names = &ctx.state.names;
        let mut models: Vec<(String, String)> = data
            .phases
            .iter()
            .flat_map(|phase| &phase.rows)
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some(agent),
                _ => None,
            })
            .flat_map(|agent| std::iter::once(&agent.worker).chain(agent.fallback_for.as_ref()))
            .map(|worker| (worker.agent.clone(), names.short(worker)))
            .collect();
        models.sort();
        models.dedup();
        Self(models)
    }

    /// The color a model's name draws in: its CLI's own for the first of the CLI's models the
    /// workflow names, and a shade of it for each one after, so two models of one CLI read
    /// apart.
    pub(super) fn style(&self, worker: &ModelRef, ctx: &ViewCtx<'_>) -> Style {
        let theme = ctx.theme;
        let role = Role::for_cli(&worker.agent);
        if role == Role::CatOther {
            return theme.style(Role::Dim);
        }
        let own = theme.style(role);
        let name = ctx.state.names.short(worker);
        let nth = self
            .0
            .iter()
            .filter(|(cli, _)| *cli == worker.agent)
            .position(|(_, model)| *model == name)
            .unwrap_or(0);
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
            WorkflowRow::Agent(_) | WorkflowRow::Command(_) => 1,
            WorkflowRow::Workflow(nested) if nested.parent.is_some() => 0,
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
    panes::count(steps_in_phase(phase), noun)
}

#[cfg(test)]
mod tests {
    use super::scroll_mark;
    use crate::theme::{Glyphs, Theme};

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
