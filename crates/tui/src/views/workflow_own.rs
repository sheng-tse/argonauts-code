//! The run's view when one session's CLI runs the whole run in its own workflow, as Claude
//! Code's own workflow runs a crew of Claude Code agents. One rounded box holds the run's name
//! and counts, the workflow's phases on the left, and every step it started on the right, each
//! with its model and what it has done so far. The keys go under the box.

use std::time::Duration;

use argonauts_core::views::{PhaseRow, RowStatus, WorkflowRow, WorkflowView as WorkflowData};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::format;
use crate::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::{Role, Theme};

use super::ViewCtx;
use super::panes::{self, Footer, Marks};
use super::states;

/// Cells the phases take at least, and a phase's name before its count.
const PHASES_MIN: usize = 16;
const PHASE_NAME_MIN: usize = 9;
/// Cells a step's label and its model take at least, so the columns stay put as steps come.
const LABEL_MIN: usize = 22;
const MODEL_MIN: usize = 28;
/// The header, the blank row under it, and the headings, above the lists.
const TOP_ROWS: u16 = 3;
/// Between the label, the model, and the counts when the columns are cut to fit.
const GAP: usize = 2;

/// What the frame shows of a run its CLI's workflow runs.
pub(super) struct Own<'a> {
    pub data: &'a WorkflowData,
    /// The step ↑↓ picked, by its phase and its row there. Nothing is picked at first.
    pub picked: Option<(usize, usize)>,
    /// The run's time so far.
    pub elapsed_ms: u64,
    /// How long running steps have gone on since the data came.
    pub ticking: u64,
}

/// Every step of the workflow in the order drawn: by phase, then as its phase lists them.
pub(super) fn steps(data: &WorkflowData) -> Vec<(usize, usize)> {
    data.phases
        .iter()
        .enumerate()
        .flat_map(|(at, phase)| (0..phase.rows.len()).map(move |row| (at, row)))
        .collect()
}

/// Rows the frame wants: the box around the header and the lists, and the keys under it.
pub(super) fn rows(data: &WorkflowData) -> u16 {
    let listed = data.phases.len().max(steps(data).len()).max(1);
    let listed = u16::try_from(listed).unwrap_or(u16::MAX);
    listed.saturating_add(2 + TOP_ROWS + 1)
}

/// `↑↓ select · enter opens an agent · x stop · p pause · esc back`, with stopping and pausing
/// only while the workflow goes.
pub(super) fn footer(going: bool, paused: bool) -> Footer {
    let mut hints = vec![("↑↓ select", 0), ("enter opens an agent", 6)];
    if going {
        hints.push(("x stop", 4));
        hints.push((if paused { "p resume" } else { "p pause" }, 3));
    }
    hints.push((panes::ESC_BACK, 0));
    Footer::hints(&hints)
}

pub(super) fn draw(buf: &mut Buffer, area: Rect, own: &Own<'_>, footer: Footer, ctx: &ViewCtx<'_>) {
    if area.width < 6 || area.height < 4 {
        return;
    }
    let theme = ctx.theme;
    let framed = Rect {
        height: area.height - 1,
        ..area
    };
    draw_border(buf, framed, theme);
    let inner = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width - 4,
        framed.height.saturating_sub(2),
    );
    let width = usize::from(inner.width);
    let phases = PhaseColumn::fit(&own.data.phases, width);
    let all = steps(own.data);
    let picked_line = own
        .picked
        .and_then(|picked| all.iter().position(|step| *step == picked));
    let listed = usize::from(inner.height).saturating_sub(usize::from(TOP_ROWS));
    let skip = picked_line.map_or(0, |at| (at + 1).saturating_sub(listed));

    let mut lines = vec![header(own, width, ctx), Line::default()];
    lines.push(headings(own, &phases, theme));
    let phase_lines: Vec<Vec<Span<'static>>> = own
        .data
        .phases
        .iter()
        .map(|phase| phases.line(phase, theme))
        .collect();
    let step_lines = StepColumns::lines(own, &all, width.saturating_sub(phases.width), ctx);
    for at in 0..listed {
        let mut spans = phase_lines
            .get(at)
            .cloned()
            .unwrap_or_else(|| vec![Span::raw(" ".repeat(phases.width))]);
        match step_lines.get(skip + at) {
            Some(step) => spans.extend(step.iter().cloned()),
            None if at >= phase_lines.len() => break,
            None => {}
        }
        lines.push(Line::from(spans));
    }
    panes::put_lines(buf, inner, &lines);
    let keys = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    panes::draw_footer(buf, keys, footer, "", theme);
}

/// The rounded border, in the faint colour.
fn draw_border(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let ascii = theme.glyphs.prompt.is_ascii();
    let [top_left, top_right, bottom_left, bottom_right, across, down] = if ascii {
        ["+", "+", "+", "+", "-", "|"]
    } else {
        ["╭", "╮", "╰", "╯", "─", "│"]
    };
    let faint = theme.style(Role::Faint);
    let inside = usize::from(area.width.saturating_sub(2));
    let width = area.width;
    let edge = |left: &str, right: &str| {
        Line::from(Span::styled(
            format!("{left}{}{right}", across.repeat(inside)),
            faint,
        ))
    };
    panes::put(buf, area.x, area.y, width, &edge(top_left, top_right));
    for y in area.y + 1..area.bottom().saturating_sub(1) {
        panes::put(buf, area.x, y, 1, &Line::from(Span::styled(down, faint)));
        panes::put(
            buf,
            area.right() - 1,
            y,
            1,
            &Line::from(Span::styled(down, faint)),
        );
    }
    if area.height > 1 {
        let bottom = area.bottom() - 1;
        panes::put(buf, area.x, bottom, width, &edge(bottom_left, bottom_right));
    }
}

/// The run's name in bold, the workflow it started from and whose own workflow runs it dim
/// after it, and the counts flush right two cells clear of them at least.
fn header(own: &Own<'_>, width: usize, ctx: &ViewCtx<'_>) -> Line<'static> {
    let data = own.data;
    let dim = ctx.theme.style(Role::Dim);
    let mut left = vec![Span::styled(
        sanitize_line(&data.name).trim().to_owned(),
        Style::new().add_modifier(Modifier::BOLD),
    )];
    for note in &data.notes {
        let note = sanitize_line(note).trim().to_owned();
        if !note.is_empty() {
            left.push(Span::styled(format!(" · {note}"), dim));
        }
    }
    if let Some(owner) = &data.owner {
        let cli = ctx.state.names.harness(owner);
        left.push(Span::styled(format!(" · {cli}'s own workflow"), dim));
    }
    let mut counts = if data.agents_total > 0 {
        format!(
            "{} of {} · {}",
            data.agents_done,
            panes::count(data.agents_total as usize, "step"),
            duration(own.elapsed_ms)
        )
    } else {
        duration(own.elapsed_ms)
    };
    let tokens = data.tokens();
    if tokens > 0 {
        counts.push_str(&format!(
            " · {} {} tokens",
            ctx.theme.glyphs.tokens,
            format::tokens(tokens)
        ));
    }
    let room = width.saturating_sub(str_width(&counts) + 2);
    if spans_width(&left) > room {
        left = truncate_spans(&left, room);
    }
    let gap = width.saturating_sub(spans_width(&left) + str_width(&counts));
    left.push(Span::raw(" ".repeat(gap)));
    left.push(Span::styled(counts, dim));
    Line::from(left)
}

/// `Phases`, then the picked step's phase (the first before one is picked), the workflow's
/// steps counted, and what that phase says of who does its work.
fn headings(own: &Own<'_>, phases: &PhaseColumn, theme: &Theme) -> Line<'static> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(pad("Phases", phases.width), bold)];
    let at = own.picked.map_or(0, |(phase, _)| phase);
    let Some(phase) = own.data.phases.get(at) else {
        return Line::from(spans);
    };
    let steps = panes::count(own.data.agents_total as usize, "step");
    let name = sanitize_line(&phase.name).trim().to_owned();
    spans.push(Span::styled(format!("{name} · {steps}"), bold));
    let detail = sanitize_line(&phase.detail).trim().to_owned();
    if !detail.is_empty() && !phase.rows.is_empty() {
        spans.push(Span::styled(format!("   {detail}"), theme.style(Role::Dim)));
    }
    Line::from(spans)
}

/// The mark for a phase or a step: `✓` done, `◐` running, `○` not started, `✗` failed.
fn mark(status: RowStatus, theme: &Theme) -> (&'static str, Role) {
    let state = states::of_row(status);
    let (glyph, role) = states::mark(state, theme);
    match status {
        RowStatus::Done => (Marks::for_theme(theme).done, role),
        _ => (glyph, role),
    }
}

fn waiting(status: RowStatus) -> bool {
    matches!(
        status,
        RowStatus::Waiting | RowStatus::Queued | RowStatus::Skipped
    )
}

/// The phases' column: each phase's mark, its name, and its steps done of all.
struct PhaseColumn {
    width: usize,
    name: usize,
}

impl PhaseColumn {
    fn fit(phases: &[PhaseRow], width: usize) -> Self {
        let longest = |cells: &dyn Fn(&PhaseRow) -> usize| phases.iter().map(cells).max();
        let name = longest(&|phase| str_width(sanitize_line(&phase.name).trim()))
            .unwrap_or(0)
            .saturating_add(GAP)
            .max(PHASE_NAME_MIN);
        let count = longest(&|phase| str_width(&count(phase))).unwrap_or(0);
        // The steps keep two thirds of the box however long the phases' names are.
        let most = (width / 3).max(PHASES_MIN);
        let wanted = (2 + name + count + GAP).max(PHASES_MIN);
        let width = wanted.min(most);
        let name = name.min(width.saturating_sub(2 + count + GAP));
        Self { width, name }
    }

    fn line(&self, phase: &PhaseRow, theme: &Theme) -> Vec<Span<'static>> {
        let (glyph, role) = mark(phase.status, theme);
        let name_style = if waiting(phase.status) {
            theme.style(Role::Dim)
        } else {
            Style::new()
        };
        let name = sanitize_line(&phase.name).trim().to_owned();
        let name = truncate_spans(&[Span::styled(name, name_style)], self.name);
        let mut spans = vec![Span::styled(format!("{glyph} "), theme.style(role))];
        let used = 2 + spans_width(&name);
        spans.extend(name);
        spans.push(Span::raw(" ".repeat((2 + self.name).saturating_sub(used))));
        spans.push(Span::raw(count(phase)));
        let drawn = spans_width(&spans);
        spans.push(Span::raw(" ".repeat(self.width.saturating_sub(drawn))));
        spans
    }
}

/// `1/1`, the phase's steps done of all, once it has any.
fn count(phase: &PhaseRow) -> String {
    phase
        .count
        .map(|count| format!("{}/{}", count.done, count.total))
        .unwrap_or_default()
}

/// The steps' columns: the mark, the label, the model on its CLI, then the tokens and the
/// time or the reviewer's verdict.
struct StepColumns;

struct StepCells {
    status: RowStatus,
    label: String,
    model: String,
    counts: String,
}

impl StepColumns {
    fn lines(
        own: &Own<'_>,
        all: &[(usize, usize)],
        room: usize,
        ctx: &ViewCtx<'_>,
    ) -> Vec<Vec<Span<'static>>> {
        let cells: Vec<StepCells> = all
            .iter()
            .filter_map(|&(phase, row)| own.data.phases.get(phase)?.rows.get(row))
            .map(|row| StepCells::of(row, own.ticking, ctx))
            .collect();
        let longest =
            |cells_of: &dyn Fn(&StepCells) -> usize| cells.iter().map(cells_of).max().unwrap_or(0);
        let (label, model, counts) = (
            longest(&|cells| str_width(&cells.label)),
            longest(&|cells| str_width(&cells.model)),
            longest(&|cells| str_width(&cells.counts)),
        );
        // Roomy columns where they fit; else each as wide as its longest, and the line is cut.
        let (mut label_width, mut model_width) =
            ((label + GAP).max(LABEL_MIN), (model + GAP).max(MODEL_MIN));
        if 2 + label_width + model_width + counts > room {
            (label_width, model_width) = (label + GAP, model + GAP);
        }
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let marks = Marks::for_theme(theme);
        cells
            .iter()
            .zip(all)
            .map(|(cells, step)| {
                let picked = own.picked == Some(*step);
                let (glyph, role) = if picked {
                    (marks.selected, Role::Card)
                } else {
                    mark(cells.status, theme)
                };
                let label_style = if picked {
                    theme.style(Role::Card)
                } else {
                    Style::new()
                };
                vec![
                    Span::styled(format!("{glyph} "), theme.style(role)),
                    Span::styled(pad(&cells.label, label_width), label_style),
                    Span::styled(pad(&cells.model, model_width), dim),
                    Span::styled(cells.counts.clone(), dim),
                ]
            })
            .collect()
    }
}

impl StepCells {
    fn of(row: &WorkflowRow, ticking: u64, ctx: &ViewCtx<'_>) -> Self {
        let names = &ctx.state.names;
        match row {
            WorkflowRow::Agent(agent) => {
                let mut counts = Vec::new();
                if agent.tokens > 0 {
                    counts.push(panes::tokens(agent.tokens));
                }
                // A reviewer the workflow asked says how it answered where the time would be,
                // once it has: while it reads the change, its time runs like any step's.
                let answered = agent
                    .answered
                    .as_deref()
                    .filter(|_| agent.status != RowStatus::Running)
                    .map(|words| sanitize_line(words.trim()).into_owned())
                    .filter(|words| !words.is_empty());
                counts.extend(answered.or_else(|| time(agent.status, agent.elapsed_ms, ticking)));
                Self {
                    status: agent.status,
                    label: sanitize_line(&agent.label).trim().to_owned(),
                    model: sanitize_line(&names.worker(&agent.worker)).into_owned(),
                    counts: counts.join(" · "),
                }
            }
            WorkflowRow::Workflow(nested) => Self {
                status: nested.status,
                label: sanitize_line(&nested.name).trim().to_owned(),
                model: nested.owner.as_deref().map_or_else(
                    || "workflow".to_owned(),
                    |owner| format!("{}'s own", names.harness(owner)),
                ),
                counts: format!("{}/{} steps", nested.agents_done, nested.agents_total),
            },
            WorkflowRow::Command(command) => Self {
                status: command.status,
                label: sanitize_line(&command.label).trim().to_owned(),
                model: sanitize_line(&command.command).into_owned(),
                counts: command
                    .result
                    .as_deref()
                    .map(|result| sanitize_line(result).into_owned())
                    .unwrap_or_default(),
            },
        }
    }
}

/// How long a step has run, `queued` while it waits for a slot, and nothing before it starts.
fn time(status: RowStatus, elapsed_ms: u64, ticking: u64) -> Option<String> {
    match status {
        RowStatus::Queued => Some("queued".to_owned()),
        RowStatus::Waiting | RowStatus::Skipped => None,
        RowStatus::Running => Some(duration(elapsed_ms + ticking)),
        _ if elapsed_ms == 0 => None,
        _ => Some(duration(elapsed_ms)),
    }
}

/// `1m 36s`, as Claude Code times its own workflow's agents.
fn duration(ms: u64) -> String {
    format::duration(Duration::from_millis(ms))
}

/// `text` and spaces after it to `width` cells, or as it is when it's wider.
fn pad(text: &str, width: usize) -> String {
    let used = str_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(used)))
}
