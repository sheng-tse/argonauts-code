//! The block Claude Code's `/context` prints for one session: its grid on the left, and two
//! blanks after it the model, its id, how much of the window is used, then what fills it, with
//! the free space and the room held back for compacting.

use argonauts_core::events::context::{ContextCell, ContextKind, ContextUsage};
use argonauts_core::logos::Logo;
use argonauts_core::window_size;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::words::{self, cc_style};
use crate::format::{self, Figures};
use crate::kit::context::claude_code_free;
use crate::kit::logo::logo_before;
use crate::kit::{Drawn, Kit};
use crate::text::str_width;
use crate::theme::Role;

/// Where the block's rows start, under Claude Code's `⎿`.
pub(super) const INDENT: usize = 5;
/// Blanks between the grid's last cell and the column beside it, wherever the grid ends, as
/// Claude Code 2.1.311 draws a 20-wide grid.
const BESIDE_GAP: usize = 2;
/// The grid's cells a row, and its rows.
const ACROSS: usize = 20;
const DOWN: usize = 10;
/// A cell this full or more draws whole, as Claude Code's grid does.
const WHOLE_FROM: f64 = 0.7;

/// One row of what fills the session, as the column beside the grid lists it.
pub(super) struct Part {
    pub name: String,
    pub tokens: u64,
    pub style: Style,
}

/// What the block shows of a session.
pub(super) struct Block<'a> {
    /// The model's name, and how its CLI calls it.
    pub model: Option<String>,
    pub model_id: Option<&'a str>,
    /// Whoever serves the model, whose logo goes before its name where one draws.
    pub logo: Option<Logo>,
    pub used: u64,
    pub window: u64,
    /// `used` of the window in whole percent, where the CLI said it.
    pub percent: Option<u64>,
    /// The line over the parts: `Estimated usage by category`.
    pub heading: String,
    pub parts: Vec<Part>,
    pub free: u64,
    pub buffer: u64,
    /// The grid as the CLI drew it, cell by cell, where it did.
    pub grid: Option<&'a [Vec<ContextCell>]>,
    pub usage: Option<&'a ContextUsage>,
    /// Lines under the parts, after a blank one, that run on below the grid.
    pub notes: Vec<String>,
    /// The parts are our count, each marked `~`.
    pub parts_ours: bool,
    /// How full it is, and so what is free, is our count too.
    pub used_ours: bool,
    /// Dim words after how full it is, such as ` · from your last session`.
    pub used_tail: String,
}

/// The block's rows, starting at the grid's first.
pub(super) fn rows(kit: &Kit<'_>, block: &Block<'_>, width: usize) -> Drawn {
    let window = block.window.max(1);
    let percent = match block.percent {
        Some(percent) => Figures::Table.share(percent, 100),
        None => Figures::Table.share(block.used, window),
    };
    let mut right: Vec<Vec<Span<'static>>> = Vec::new();
    let mut model = block
        .logo
        .as_ref()
        .map(|logo| logo_before(logo, kit.theme))
        .unwrap_or_default();
    model.push(Span::raw(block.model.clone().unwrap_or_default()));
    right.push(model);
    right.push(vec![kit.dim(block.model_id.unwrap_or_default().to_owned())]);
    let (ours, used_ours) = (mark(block.parts_ours), mark(block.used_ours));
    let count = |tokens: u64| beside(tokens, block.window);
    let window_words = if binary(block.window) {
        format::window(block.window)
    } else {
        format::context_tokens(block.window)
    };
    right.push(vec![kit.dim(format!(
        "{used_ours}{}/{window_words} tokens ({percent}){}",
        count(block.used),
        block.used_tail
    ))]);
    right.push(Vec::new());
    right.push(vec![Span::styled(
        block.heading.clone(),
        kit.fg(Role::Dim).add_modifier(Modifier::ITALIC),
    )]);
    let glyphs = Glyphs::for_kit(kit);
    for part in &block.parts {
        right.push(vec![
            Span::styled(format!("{} ", glyphs.whole), part.style),
            Span::raw(format!("{}: ", part.name)),
            kit.dim(format!(
                "{ours}{} tokens ({})",
                count(part.tokens),
                words::share(part.tokens, window)
            )),
        ]);
    }
    right.push(vec![
        Span::styled(format!("{} ", glyphs.free), claude_code_free(kit, false)),
        Span::raw("Free space: "),
        kit.dim(format!(
            "{used_ours}{} ({})",
            count(block.free),
            words::share(block.free, window)
        )),
    ]);
    if block.buffer > 0 {
        right.push(vec![
            Span::styled(format!("{} ", glyphs.held), claude_code_free(kit, true)),
            Span::raw("Autocompact buffer: "),
            kit.dim(format!(
                "{used_ours}{} tokens ({})",
                count(block.buffer),
                words::share(block.buffer, window)
            )),
        ]);
    }
    if !block.notes.is_empty() {
        right.push(Vec::new());
        right.extend(block.notes.iter().map(|note| vec![kit.dim(note.clone())]));
    }

    let grid = match block.grid.filter(|grid| !grid.is_empty()) {
        Some(cells) => drawn_grid(kit, cells, block.usage),
        None => worked_out_grid(kit, block),
    };
    let grid_cells = grid
        .iter()
        .map(|row| {
            row.iter()
                .map(|span| str_width(&span.content))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    let beside_at = INDENT + grid_cells + BESIDE_GAP;
    let mut drawn = Drawn::new();
    // Too narrow for the column beside the grid: it goes under it, whole.
    let widest = right
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| str_width(&span.content))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    if width < beside_at + widest {
        for row in grid {
            drawn.push(indented(INDENT, row));
        }
        for line in right {
            drawn.push(indented(INDENT, line));
        }
        return drawn;
    }
    for at in 0..grid.len().max(right.len()) {
        let mut line = Vec::new();
        let mut cells = INDENT;
        line.push(Span::raw(" ".repeat(INDENT)));
        if let Some(row) = grid.get(at) {
            cells += row
                .iter()
                .map(|span| str_width(&span.content))
                .sum::<usize>();
            line.extend(row.iter().cloned());
        }
        if let Some(beside) = right.get(at).filter(|beside| !beside.is_empty()) {
            line.push(Span::raw(" ".repeat(beside_at.saturating_sub(cells))));
            line.extend(beside.iter().cloned());
        }
        drawn.push(line);
    }
    drawn
}

/// `~` before a count that is ours.
pub(super) fn mark(ours: bool) -> &'static str {
    if ours { "~" } else { "" }
}

/// Whether the window counts in 1,024s, as 65,536 does.
fn binary(window: u64) -> bool {
    window_size::window_divisor(window) == 1_024
}

/// A count as Claude Code writes it, except beside a window counted in 1,024s, where it
/// counts as the card does (9.3k of 64k), so the two never disagree.
pub(super) fn beside(tokens: u64, window: u64) -> String {
    if binary(window) {
        format::tokens_beside(tokens, window)
    } else {
        format::context_tokens(tokens)
    }
}

fn indented(by: usize, mut line: Vec<Span<'static>>) -> Vec<Span<'static>> {
    if !line.is_empty() {
        line.insert(0, Span::raw(" ".repeat(by)));
    }
    line
}

/// The grid as the CLI counted it: a cell mostly full draws whole, one partly full as part,
/// free space faint, and the room held back for compacting in its own glyph.
fn drawn_grid(
    kit: &Kit<'_>,
    cells: &[Vec<ContextCell>],
    usage: Option<&ContextUsage>,
) -> Vec<Vec<Span<'static>>> {
    let glyphs = Glyphs::for_kit(kit);
    let kind = |cell: &ContextCell| {
        usage
            .and_then(|usage| usage.category(&cell.category))
            .map_or(ContextKind::Used, |category| category.kind)
    };
    cells
        .iter()
        .map(|row| {
            let mut spans = Vec::with_capacity(row.len() * 2);
            for cell in row {
                let (glyph, style) = match kind(cell) {
                    ContextKind::Free => (glyphs.free, claude_code_free(kit, false)),
                    ContextKind::Buffer => (glyphs.held, claude_code_free(kit, true)),
                    _ if cell.fullness >= WHOLE_FROM => (glyphs.whole, cc_style(kit, &cell.color)),
                    _ => (glyphs.part, cc_style(kit, &cell.color)),
                };
                spans.push(Span::styled(glyph, style));
                spans.push(Span::raw(" "));
            }
            spans
        })
        .collect()
}

/// The grid by Claude Code's rule where its CLI drew none: each part takes its share of the
/// 200 cells and at least one, in order, each cell whole when the part fills 70% of it or more,
/// the room held back comes last, and the rest is free.
fn worked_out_grid(kit: &Kit<'_>, block: &Block<'_>) -> Vec<Vec<Span<'static>>> {
    let glyphs = Glyphs::for_kit(kit);
    let total = ACROSS * DOWN;
    let cell = block.window.max(1) as f64 / total as f64;
    let mut cells: Vec<(&'static str, Style)> = Vec::with_capacity(total);
    for part in &block.parts {
        let exact = part.tokens as f64 / cell;
        let n = (exact.round() as usize).max(1);
        cells.extend((0..n).map(|at| {
            let fullness = (exact - at as f64).clamp(0.0, 1.0);
            let glyph = if fullness >= WHOLE_FROM {
                glyphs.whole
            } else {
                glyphs.part
            };
            (glyph, part.style)
        }));
    }
    let held = (block.buffer as f64 / cell).round() as usize;
    cells.truncate(total.saturating_sub(held));
    let free = total.saturating_sub(cells.len() + held);
    cells.extend(std::iter::repeat_n(
        (glyphs.free, claude_code_free(kit, false)),
        free,
    ));
    cells.extend(std::iter::repeat_n(
        (glyphs.held, claude_code_free(kit, true)),
        held,
    ));
    cells
        .chunks(ACROSS)
        .map(|row| {
            row.iter()
                .flat_map(|(glyph, style)| [Span::styled(*glyph, *style), Span::raw(" ")])
                .collect()
        })
        .collect()
}

/// The grid's glyphs: a cell mostly full, one partly full, free space, and the room held
/// back for compacting.
#[derive(Debug, Clone, Copy)]
struct Glyphs {
    whole: &'static str,
    part: &'static str,
    free: &'static str,
    held: &'static str,
}

impl Glyphs {
    fn for_kit(kit: &Kit<'_>) -> Self {
        if kit.theme.glyphs.rule.is_ascii() {
            Self {
                whole: "#",
                part: "+",
                free: ".",
                held: "x",
            }
        } else {
            Self {
                whole: "\u{26C1}",
                part: "\u{26C0}",
                free: "\u{26F6}",
                held: "\u{26DD}",
            }
        }
    }
}

/// The parts of a count Claude Code lists by category: what takes room in the window, in its
/// order, then what is free and what is held back.
pub(super) fn of_usage(kit: &Kit<'_>, usage: &ContextUsage) -> (Vec<Part>, u64, u64) {
    let parts = usage
        .of_kind(ContextKind::Used)
        .filter(|category| category.tokens > 0)
        .map(|category| Part {
            name: category.name.clone(),
            tokens: category.tokens,
            style: cc_style(kit, &category.color),
        })
        .collect();
    let buffer = if usage.auto_compact {
        usage.tokens_of(ContextKind::Buffer)
    } else {
        0
    };
    let free = match usage.of_kind(ContextKind::Free).next() {
        Some(free) => free.tokens,
        None => usage
            .window
            .saturating_sub(usage.total_tokens)
            .saturating_sub(buffer),
    };
    (parts, free, buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::plain_text;
    use crate::theme::Theme;

    fn shown(block: &Block<'_>, width: usize) -> Vec<String> {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let lines = rows(&kit, block, width).lines;
        plain_text(&lines)
            .lines()
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn block(parts: &[(&str, u64)], buffer: u64) -> Block<'static> {
        let used = parts.iter().map(|(_, tokens)| tokens).sum::<u64>();
        Block {
            model: Some("Opus 5.5".to_owned()),
            model_id: Some("claude-opus-5-5"),
            logo: None,
            used,
            window: 1_000_000,
            percent: None,
            heading: "Estimated usage by category".to_owned(),
            parts: parts
                .iter()
                .map(|(name, tokens)| Part {
                    name: (*name).to_owned(),
                    tokens: *tokens,
                    style: Style::default(),
                })
                .collect(),
            free: 1_000_000 - used - buffer,
            buffer,
            grid: None,
            usage: None,
            notes: Vec::new(),
            parts_ours: false,
            used_ours: false,
            used_tail: String::new(),
        }
    }

    /// Each part takes its share of the grid and at least a cell, and the room held back for
    /// compacting fills the last cells.
    #[test]
    fn the_grid_gives_each_part_its_share_and_holds_the_buffer_last() {
        let rows = shown(
            &block(&[("System prompt", 1_000), ("Messages", 151_400)], 33_000),
            118,
        );
        let cells: String = rows
            .iter()
            .take(DOWN)
            .map(|row| {
                row.chars()
                    .skip(INDENT)
                    .take(ACROSS * 2)
                    .collect::<String>()
            })
            .collect::<String>()
            .chars()
            .filter(|ch| *ch != ' ')
            .collect();
        assert_eq!(cells.chars().count(), 200);
        // The prompt's 1k fills a fifth of its one cell, which draws as part full.
        assert_eq!(cells.chars().filter(|ch| *ch == '\u{26C0}').count(), 1);
        assert_eq!(cells.chars().filter(|ch| *ch == '\u{26C1}').count(), 30);
        assert_eq!(cells.chars().filter(|ch| *ch == '\u{26DD}').count(), 7);
        assert!(cells.ends_with(&"\u{26DD}".repeat(7)));
    }

    #[test]
    fn the_column_beside_the_grid_names_the_model_and_its_parts() {
        let rows = shown(&block(&[("Skills", 6_000)], 33_000), 118);
        assert!(rows[0].ends_with(" Opus 5.5"), "{rows:#?}");
        assert!(rows[2].ends_with(" 6k/1m tokens (1%)"), "{rows:#?}");
        assert!(
            rows[5].ends_with("\u{26C1} Skills: 6k tokens (0.6%)"),
            "{rows:#?}"
        );
        assert!(rows[6].ends_with("Free space: 961k (96.1%)"), "{rows:#?}");
        assert!(
            rows[7].ends_with("Autocompact buffer: 33k tokens (3.3%)"),
            "{rows:#?}"
        );
    }

    /// A line of the column too wide to fit beside the grid puts the whole column under it, so
    /// no line of it is cut at 80 columns.
    #[test]
    fn a_column_too_wide_to_sit_beside_the_grid_goes_under_it_whole() {
        let block = block(&[("MCP server instructions", 960)], 33_000);
        let beside = shown(&block, 118);
        assert!(
            beside[5].ends_with("\u{26C1} MCP server instructions: 960 tokens (0.1%)"),
            "{beside:#?}"
        );
        let rows = shown(&block, 80);
        assert!(rows.iter().all(|row| str_width(row) <= 80), "{rows:#?}");
        for whole in [
            "\u{26C1} MCP server instructions: 960 tokens (0.1%)",
            "\u{26DD} Autocompact buffer: 33k tokens (3.3%)",
        ] {
            assert!(rows.iter().any(|row| row.trim() == whole), "{rows:#?}");
        }
    }

    /// On a narrow panel the column goes under the grid.
    #[test]
    fn a_narrow_block_puts_the_column_under_the_grid() {
        let rows = shown(&block(&[("Skills", 6_000)], 0), 60);
        assert_eq!(rows.len(), DOWN + 7);
        assert_eq!(rows[DOWN], "     Opus 5.5");
    }
}
