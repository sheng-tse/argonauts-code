//! `/context`: every live session's context against its cap, its calls, and its spend.

use argonauts_core::Spend;
use argonauts_core::app::ViewRequest;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::views::{ContextRow, ContextView as ContextData};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::format;
use crate::text::{sanitize_line, str_width};
use crate::theme::{Role, Theme};

use super::layout::{left, right};
use super::list;
use super::panes::{self, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Labels past this are cut, so the numbers keep their room.
const MAX_LABEL: usize = 32;

/// Past this share of its cap, a session's context shows in the warning color: it rotates soon.
const NEAR_CAP: f64 = 0.9;

#[derive(Debug)]
pub struct ContextView {
    pub data: ContextData,
    selected: usize,
}

impl ContextView {
    pub fn new(data: ContextData) -> Self {
        Self { data, selected: 0 }
    }
}

/// One column: its heading, each row's cell, and whether numbers align right.
struct Column {
    heading: &'static str,
    cells: Vec<String>,
    numeric: bool,
    width: usize,
}

impl Column {
    fn new(heading: &'static str, cells: Vec<String>, numeric: bool) -> Self {
        let width = cells
            .iter()
            .map(|cell| str_width(cell))
            .chain([heading.len()])
            .max()
            .unwrap_or(0);
        Self {
            heading,
            cells,
            numeric,
            width,
        }
    }

    fn cell(&self, text: &str) -> String {
        if self.numeric {
            right(text, self.width)
        } else {
            left(text, self.width)
        }
    }
}

fn columns(rows: &[ContextRow], width: usize, names: &DisplayNames) -> Vec<Column> {
    let cells = |f: fn(&ContextRow) -> String| rows.iter().map(f).collect::<Vec<_>>();
    let mut label = Column::new(
        "Session",
        cells(|row| sanitize_line(&row.label).trim().to_owned()),
        false,
    );
    label.width = label.width.min(MAX_LABEL);
    let mut columns = vec![
        label,
        Column::new(
            "Worker",
            rows.iter()
                .map(|row| sanitize_line(&names.worker(&row.worker)).into_owned())
                .collect(),
            false,
        ),
        Column::new("Context", cells(used), true),
        Column::new(
            "Window",
            cells(|row| row.window.map_or_else(String::new, format::tokens)),
            true,
        ),
        Column::new("Calls", cells(|row| row.tool_calls.to_string()), true),
        Column::new(
            "Spend",
            cells(|row| panes::spend(&row.spent).unwrap_or_default()),
            true,
        ),
        Column::new("Rotated", cells(|row| row.rotations.to_string()), true),
    ];
    // Columns give way from the least telling, and then the label is cut.
    let total = |columns: &[Column]| {
        2 + columns.iter().map(|c| c.width).sum::<usize>() + 2 * columns.len().saturating_sub(1)
    };
    for heading in ["Rotated", "Window", "Spend", "Calls", "Worker"] {
        if total(&columns) <= width {
            break;
        }
        columns.retain(|column| column.heading != heading);
    }
    let over = total(&columns).saturating_sub(width);
    columns[0].width = columns[0].width.saturating_sub(over).max(1);
    columns
}

/// `120k/200k`, or the used tokens alone without a cap.
fn used(row: &ContextRow) -> String {
    match row.cap {
        Some(cap) => format!("{}/{}", format::tokens(row.used), format::tokens(cap)),
        None => format::tokens(row.used),
    }
}

fn near_cap(row: &ContextRow) -> bool {
    row.cap
        .is_some_and(|cap| cap > 0 && row.used as f64 >= cap as f64 * NEAR_CAP)
}

impl ContextView {
    fn lines(&self, width: usize, names: &DisplayNames, theme: &Theme) -> Vec<Line<'static>> {
        let rows = &self.data.rows;
        let dim = theme.style(Role::Dim);
        let marks = Marks::for_theme(theme);
        let columns = columns(rows, width, names);
        let heading: Vec<Span<'static>> = std::iter::once(Span::raw("  "))
            .chain(columns.iter().enumerate().flat_map(|(i, column)| {
                let gap = if i == 0 { "" } else { "  " };
                [
                    Span::raw(gap),
                    Span::styled(column.cell(column.heading), dim),
                ]
            }))
            .collect();
        let mut lines = vec![Line::from(heading)];
        for (i, row) in rows.iter().enumerate() {
            let selected = i == self.selected;
            let mut spans = vec![if selected {
                Span::styled(format!("{} ", marks.selected), theme.style(Role::Label))
            } else {
                Span::raw("  ")
            }];
            for (c, column) in columns.iter().enumerate() {
                if c > 0 {
                    spans.push(Span::raw("  "));
                }
                let style = match column.heading {
                    "Session" if selected => theme.style(Role::Label),
                    "Session" => theme.style(Role::Text),
                    "Context" if near_cap(row) => theme.style(Role::Warning),
                    "Context" => theme.style(Role::Text),
                    _ => dim,
                };
                spans.push(Span::styled(column.cell(&column.cells[i]), style));
            }
            lines.push(Line::from(spans));
        }
        if rows.is_empty() {
            lines.push(Line::from(Span::styled("  No live sessions", dim)));
        }
        lines
    }
}

impl View for ContextView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let rows = &self.data.rows;
        let used: u64 = rows.iter().map(|row| row.used).sum();
        let spent = rows
            .iter()
            .fold(Spend::default(), |total, row| total.plus(&row.spent));
        let mut summary = vec![format!("{} in context", format::tokens(used))];
        if spent.cost_usd > 0.0 {
            summary.push(format::spend(&spent));
        }
        let header = Header {
            title: Line::from(Span::styled("Context", theme.style(Role::Label))),
            description: "Each live session's context against the cap where it rotates".to_owned(),
            summary: summary.join(" · "),
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        let title = panes::count(rows.len(), "session");
        panes::draw_box(buf, &panes, ["", &title], Side::Right, theme);

        let lines = self.lines(usize::from(panes.right.width), &ctx.state.names, theme);
        // The heading stays; the rows scroll under it.
        let height = usize::from(panes.right.height);
        let skip = (self.selected + 2).saturating_sub(height);
        let mut shown = vec![lines[0].clone()];
        shown.extend(lines.iter().skip(1 + skip).cloned());
        panes::put_lines(buf, panes.right, &shown);

        let hints = Footer::hints(&[("↑↓ select", 0), ("⏎ open agent", 1), (panes::ESC_BACK, 0)]);
        panes::draw_footer(buf, panes.footer, hints, "", theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let count = self.data.rows.len();
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ViewOutcome::Consumed
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
                ViewOutcome::Consumed
            }
            KeyCode::Home => {
                self.selected = 0;
                ViewOutcome::Consumed
            }
            KeyCode::End => {
                self.selected = count.saturating_sub(1);
                ViewOutcome::Consumed
            }
            KeyCode::Enter | KeyCode::Right => {
                let Some(row) = self.data.rows.get(self.selected) else {
                    return ViewOutcome::Consumed;
                };
                match &row.run {
                    Some(run) => ViewOutcome::Request(ViewRequest::Agent {
                        run: run.clone(),
                        session: row.session.clone(),
                    }),
                    None => ViewOutcome::Notice("Chat with the lead has no agent view".to_owned()),
                }
            }
            KeyCode::Esc | KeyCode::Left => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }
}
