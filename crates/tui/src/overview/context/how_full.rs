//! The main session of a CLI that says how full its context is and nothing of what fills it,
//! as OpenCode does, and Codex as the main agent: the title, Claude Code's grid by its own rule
//! in the CLI's colour beside the two rows Argonauts Code knows, where the window comes from,
//! and why nothing else is listed.

use argonauts_core::views::{ContextRow, WindowFigures};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::place::{Columns, at, ending, place};
use super::usage::{
    GridGlyphs, LABEL_CELLS, NAME_CELLS, SHARE_CELLS, TOKENS_CELLS, VALUE_CELLS, WHOLE_FROM, bold,
    level_role, tinted,
};
use super::words::share;
use crate::format;
use crate::kit::{Drawn, Kit, Level, Levels, ljust, rjust};
use crate::state::LastStep;
use crate::text::{sanitize_line, spans_width, str_width};
use crate::theme::Role;

/// What the tab knows of such a session.
pub(super) struct HowFull<'a> {
    pub row: &'a ContextRow,
    /// Its window in tokens, never zero.
    pub window: u64,
    /// Its CLI by name, such as `OpenCode`.
    pub cli: String,
    /// Its model by name, in the colour the model draws in.
    pub model: Option<(String, Style)>,
    /// The colour the session's cells draw in.
    pub own: Style,
    /// `counted 2m ago`: when its CLI last said how full it is.
    pub counted: Option<String>,
    /// Its last step, as its CLI said it.
    pub last: Option<LastStep>,
    /// Where its window comes from.
    pub window_from: Option<&'a WindowFigures>,
}

impl HowFull<'_> {
    fn used(&self) -> u64 {
        self.row.used
    }

    /// A count measured in the window, in the window's divisor.
    fn sized(&self, tokens: u64) -> String {
        format::tokens_beside(tokens, self.window)
    }

    /// How close the session is to the end of its window. Its CLI compacts by itself at a
    /// point it doesn't report, so the window's end is the one point known.
    fn level(&self) -> Level {
        Levels::CONTEXT.level(self.used() as f64 / self.window as f64 * 100.0)
    }
}

/// The session's blocks, top to bottom.
pub(super) fn blocks(kit: &Kit<'_>, columns: &Columns, told: &HowFull<'_>) -> Vec<Drawn> {
    vec![
        title(kit, columns, told),
        grid_and_legend(kit, columns, told),
        window_lines(kit, columns, told),
        not_listed(kit, columns, told),
    ]
}

/// `Context Usage  counted 2m ago` with `41k/64k tokens (64%)` flush right, and the model with
/// its id on the next row where the three don't fit one.
fn title(kit: &Kit<'_>, columns: &Columns, told: &HowFull<'_>) -> Drawn {
    let mut name = vec![bold(kit, "Context Usage")];
    if let Some(counted) = &told.counted {
        name.push(kit.dim(format!("  {counted}")));
    }
    let mut model = Vec::new();
    if let Some((shown, style)) = &told.model {
        model.push(Span::styled(
            sanitize_line(shown).trim().to_owned(),
            style.add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(id) = told.row.worker.model.as_deref() {
        let gap = if model.is_empty() { "" } else { "  " };
        model.push(kit.dim(format!("{gap}{}", sanitize_line(id).trim())));
    }
    let percent = (told.used() as f64 / told.window as f64 * 100.0).round();
    let percent = Span::styled(
        format!("{percent:.0}%"),
        match level_role(told.level()) {
            Some(role) => kit.fg(role).add_modifier(Modifier::BOLD),
            None => kit.bold(Role::Text),
        },
    );
    let totals = vec![
        bold(kit, told.sized(told.used())),
        kit.dim(format!("/{} tokens (", format::window(told.window))),
        percent,
        kit.dim(")"),
    ];
    let mut drawn = Drawn::new();
    let beside = columns
        .legend
        .filter(|legend| legend + spans_width(&model) + 2 + spans_width(&totals) <= columns.edge);
    match beside {
        Some(legend) => drawn.push(place([
            at(columns.indent(), name),
            at(legend, model),
            ending(columns.edge, totals),
        ])),
        None => {
            drawn.push(place([
                at(columns.indent(), name),
                ending(columns.edge, totals),
            ]));
            if !model.is_empty() {
                drawn.push(place([at(columns.indent(), model)]));
            }
        }
    }
    drawn
}

/// Claude Code's grid for a window this large: 20 x 10 cells from 1M tokens, else 10 x 10, and
/// 5 across on a screen narrower than 80 cells, as its /context draws it.
fn grid_size(window: u64, width: usize) -> (usize, usize) {
    let narrow = width < 80;
    let large = window >= 1_000_000;
    let across = if narrow {
        5
    } else if large {
        20
    } else {
        10
    };
    let down = if large {
        10
    } else if narrow {
        5
    } else {
        10
    };
    (across, down)
}

/// The grid's cells, first to last: how many are used, at least one for a session that holds
/// anything, and whether the last of them is mostly full.
fn used_cells(used: u64, window: u64, cells: usize) -> (usize, bool) {
    if used == 0 {
        return (0, false);
    }
    let share = used as f64 / window as f64;
    let filled = ((share * cells as f64).round() as usize).clamp(1, cells);
    let per_cell = window as f64 / cells as f64;
    let last = (used as f64 - (filled - 1) as f64 * per_cell) / per_cell;
    (filled, last >= WHOLE_FROM)
}

/// The grid with the CLI's words over the legend beside it, or the legend under the grid where
/// the panel is too narrow for both.
fn grid_and_legend(kit: &Kit<'_>, columns: &Columns, told: &HowFull<'_>) -> Drawn {
    let glyphs = GridGlyphs::for_kit(kit);
    let (across, down) = grid_size(told.window, columns.width);
    let (filled, last_whole) = used_cells(told.used(), told.window, across * down);
    let cell = |at: usize| -> Span<'static> {
        if at >= filled {
            Span::styled(glyphs.free, kit.fg(Role::Faint))
        } else if at + 1 == filled && !last_whole {
            Span::styled(glyphs.part, told.own)
        } else {
            Span::styled(glyphs.whole, told.own)
        }
    };
    let grid: Vec<Vec<Span<'static>>> = (0..down)
        .map(|row| {
            (0..across)
                .flat_map(|column| [cell(row * across + column), Span::raw(" ")])
                .collect()
        })
        .collect();
    let heading = kit.dim(format!(
        "{} says how full, not with what",
        sanitize_line(&told.cli).trim()
    ));
    let legend = legend(kit, told, glyphs);
    let indent = columns.indent();
    let mut drawn = Drawn::new();
    let Some(beside) = columns.legend else {
        for row in grid {
            drawn.push(place([at(indent, row)]));
        }
        drawn.push(Vec::new());
        drawn.push(place([at(indent, vec![heading])]));
        for row in legend {
            drawn.push(place([at(indent, row)]));
        }
        return drawn;
    };
    drawn.push(place([at(beside, vec![heading])]));
    let widest = grid.iter().map(|row| spans_width(row)).max().unwrap_or(0);
    let beside = beside.max(indent + widest + 1);
    let mut legend = legend.into_iter();
    for cells in grid {
        let mut row = vec![Span::raw(" ".repeat(indent))];
        row.extend(cells);
        if let Some(entry) = legend.next() {
            row.push(Span::raw(
                " ".repeat(beside.saturating_sub(spans_width(&row))),
            ));
            row.extend(entry);
        }
        drawn.push(row);
    }
    drawn
}

/// `In context` and `Free space`, in the approved legend's columns: free space in the colour
/// of how close the session is to the end of its window.
fn legend(kit: &Kit<'_>, told: &HowFull<'_>, glyphs: GridGlyphs) -> Vec<Vec<Span<'static>>> {
    let free = told.window.saturating_sub(told.used());
    let rows = [
        (glyphs.whole, told.own, "In context", told.used(), None),
        (
            glyphs.free,
            kit.fg(Role::Faint),
            "Free space",
            free,
            level_role(told.level()),
        ),
    ];
    rows.into_iter()
        .map(|(glyph, style, name, tokens, amount)| {
            vec![
                Span::styled(format!("{glyph} "), style),
                Span::raw(ljust(name, NAME_CELLS)),
                tinted(kit, rjust(&told.sized(tokens), TOKENS_CELLS), amount),
                kit.dim(rjust(&share(tokens, told.window), SHARE_CELLS)),
            ]
        })
        .collect()
}

/// The lines under the grid, each a label, a number and a note: the window and where it comes
/// from, who compacts, and the last request.
fn window_lines(kit: &Kit<'_>, columns: &Columns, told: &HowFull<'_>) -> Drawn {
    let indent = columns.indent();
    let cli = sanitize_line(&told.cli).trim().to_owned();
    let mut drawn = Drawn::new();
    let mut line = |label: &str, value: String, note: Vec<Span<'static>>| {
        let mut row = vec![
            Span::raw(" ".repeat(indent)),
            bold(kit, ljust(label, LABEL_CELLS)),
            Span::raw(ljust(&value, VALUE_CELLS)),
        ];
        if note.is_empty() {
            drawn.push(row);
            return;
        }
        let at_note =
            (indent + LABEL_CELLS + VALUE_CELLS).max(indent + LABEL_CELLS + str_width(&value) + 2);
        if at_note + spans_width(&note) <= columns.width {
            row.push(Span::raw(
                " ".repeat(at_note.saturating_sub(spans_width(&row))),
            ));
            row.extend(note);
            drawn.push(row);
        } else {
            drawn.push(row);
            drawn.push(place([at(indent + 2, note)]));
        }
    };

    let from: Vec<Span<'static>> = told
        .window_from
        .and_then(|figures| window_from(figures, told.row, &cli))
        .map(|words| vec![kit.dim(words)])
        .unwrap_or_default();
    line(
        "Context window:",
        format!("{} tokens", format::window(told.window)),
        from,
    );

    let left = told.window.saturating_sub(told.used());
    line(
        "Auto-compact:",
        format!("by {cli}"),
        vec![
            kit.dim("at a point it doesn't report \u{b7} "),
            kit.span(
                format!("{} left in the window", told.sized(left)),
                level_role(told.level()).unwrap_or(Role::Dim),
            ),
        ],
    );

    if let Some(last) = told.last {
        let server = told.window_from.and_then(|figures| figures.server.as_ref());
        let cache = match (last.cached, server) {
            (0, Some(server)) => format!(
                "{} reports no cache, so local input counts in full \u{b7} ",
                sanitize_line(&server.name).trim()
            ),
            (0, None) => String::new(),
            (cached, _) => format!("{} from cache \u{b7} ", told.sized(cached)),
        };
        line(
            "Last request:",
            format!("{} in", told.sized(told.used())),
            vec![kit.dim(format!("{cache}{} out", told.sized(last.output)))],
        );
    }
    drawn
}

/// `smallest of the model's 256k, the tag's 64k on Ollama, OpenCode's 64k`, from the figures
/// that hold the window. Nothing where only one is known, since then it is the window.
fn window_from(figures: &WindowFigures, row: &ContextRow, cli: &str) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(model) = figures.model {
        parts.push(format!("the model's {}", format::window(model)));
    }
    if let Some(server) = &figures.server {
        // A tag, as Ollama's `qwen3-coder:30b-64k`, holds the model to its own cap.
        let tagged = row
            .worker
            .model
            .as_deref()
            .is_some_and(|id| id.contains(':'));
        let whose = if tagged { "the tag's" } else { "the server's" };
        parts.push(format!(
            "{whose} {} on {}",
            format::window(server.tokens),
            sanitize_line(&server.name).trim()
        ));
    }
    if let Some(tokens) = figures.cli {
        parts.push(format!("{cli}'s {}", format::window(tokens)));
    }
    (parts.len() > 1).then(|| format!("smallest of {}", parts.join(", ")))
}

/// Why the lists Claude Code's /context has are missing.
fn not_listed(kit: &Kit<'_>, columns: &Columns, told: &HowFull<'_>) -> Drawn {
    let mut drawn = Drawn::new();
    drawn.push(place([at(
        columns.indent(),
        vec![kit.dim(format!(
            "No MCP tools, memory files, skills or messages to list: {} reports how many \
             tokens a step read, not what.",
            sanitize_line(&told.cli).trim()
        ))],
    )]));
    drawn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_is_claude_codes_for_the_window() {
        assert_eq!(grid_size(65_536, 118), (10, 10));
        assert_eq!(grid_size(1_000_000, 118), (20, 10));
        assert_eq!(grid_size(1_048_576, 118), (20, 10));
        assert_eq!(grid_size(65_536, 60), (5, 5));
        assert_eq!(grid_size(1_000_000, 60), (5, 10));
    }

    #[test]
    fn a_session_that_holds_anything_takes_a_cell() {
        assert_eq!(used_cells(0, 65_536, 100), (0, false));
        assert_eq!(used_cells(1, 65_536, 100), (1, false));
        assert_eq!(used_cells(41_984, 65_536, 100), (64, true));
        assert_eq!(used_cells(65_536, 65_536, 100), (100, true));
        assert_eq!(used_cells(70_000, 65_536, 100), (100, true));
    }
}
