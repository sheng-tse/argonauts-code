//! Every screen holds its shape at every width and with any content. The frame checker proves
//! it on what was rendered: no glyph crosses the right edge, box borders stay in one column and
//! close, a vertical separator never jumps a column, no inline row is wider than the screen,
//! and no two block meters sit on back-to-back rows, which a terminal draws as one slab.
//! Snapshot tests run it at [`WIDTHS`] with the [`samples`].

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;

use crate::kit::Drawn;
use crate::text::{ambiguous_width, spans_width, width_with};

#[cfg(test)]
pub(crate) mod grid;

/// Every view and question is checked at these widths.
pub const WIDTHS: [u16; 6] = [40, 60, 80, 100, 120, 200];

/// Glyphs that are ambiguous or that some fonts draw as emoji. `ui.glyphs = "ascii"` keeps them
/// out of frames and aligned columns.
pub const UNSTABLE_GLYPHS: [&str; 5] = ["●", "◇", "✔", "⏎", "⏺"];

/// Content that breaks careless layouts.
pub mod samples {
    pub const CJK: &str = "修复导出测试中的竞态条件并添加回归测试";
    pub const EMOJI: &str = "Ship it 🚀 then celebrate 🎉 with the crew 👩‍🚀";
    pub const LONG_PATH: &str =
        "/Users/someone/github/argonauts/crates/tui/src/bottom/composer/layout.rs";
    pub const EMPTY: &str = "";

    /// A word with no break opportunity, longer than any screen.
    pub fn long_word() -> String {
        "x".repeat(260)
    }

    /// Every sample, the long word included, for sweeps that try each one.
    pub fn all() -> Vec<String> {
        vec![
            CJK.to_owned(),
            EMOJI.to_owned(),
            LONG_PATH.to_owned(),
            EMPTY.to_owned(),
            long_word(),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameIssue {
    pub row: u16,
    pub col: u16,
    pub what: String,
}

const HORIZONTAL: &[&str] = &["─", "━", "═", "╌", "┄", "┈"];
/// `|` and `+` too: views draw their dividers in ASCII under `ui.glyphs = "ascii"`.
const VERTICAL: &[&str] = &["│", "┃", "║", "╎", "┆", "┊", "|"];
const TOP_LEFT: &[&str] = &["┌", "╭", "┏", "╔"];
const TOP_RIGHT: &[&str] = &["┐", "╮", "┓", "╗"];
const BOTTOM_LEFT: &[&str] = &["└", "╰", "┗", "╚"];
const BOTTOM_RIGHT: &[&str] = &["┘", "╯", "┛", "╝"];
const LEFT_JOINS: &[&str] = &["├", "┣", "╠", "┝", "┠"];
const RIGHT_JOINS: &[&str] = &["┤", "┫", "╣", "┥", "┨"];
/// Where a divider inside a box meets its bottom edge.
const BOTTOM_JOINS: &[&str] = &["┴", "┻", "╩", "+"];

/// Checks a whole rendered screen with ambiguous glyphs as wide as `ui.ambiguous_width` says.
/// Empty when it's sound.
pub fn check(buf: &Buffer) -> Vec<FrameIssue> {
    check_with(buf, ambiguous_width())
}

/// Checks a screen as a terminal that draws each ambiguous glyph `ambiguous` cells wide would
/// show it. At 2, an ambiguous glyph with text right after it is text drawn over half a glyph.
/// Box drawing is the frame itself and counts as one cell either way.
pub fn check_with(buf: &Buffer, ambiguous: u8) -> Vec<FrameIssue> {
    let mut issues = Vec::new();
    check_wide_glyphs(buf, ambiguous, &mut issues);
    check_boxes(buf, &mut issues);
    check_separators(buf, &mut issues);
    issues
}

/// Checks rows drawn inline, such as history cells and the live region: none may be wider than
/// the screen, or the terminal wraps it and the region breaks. Boxes and columns drawn with
/// them must line up as they would on a screen.
pub fn check_lines(lines: &[Line<'_>], width: u16) -> Vec<FrameIssue> {
    let mut issues: Vec<FrameIssue> = lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| {
            let used = spans_width(&line.spans);
            (used > usize::from(width)).then(|| FrameIssue {
                row: row as u16,
                col: width,
                what: format!("row is {used} cells wide on a {width}-cell screen"),
            })
        })
        .collect();
    let buf = lines_buffer(lines, width);
    check_boxes(&buf, &mut issues);
    check_separators(&buf, &mut issues);
    issues
}

/// Every cell inside a frame that draws one of `glyphs`, or a variation selector asking for
/// emoji width. Rows outside any frame, such as a view's key hints, aren't frames.
pub fn check_glyphs(buf: &Buffer, glyphs: &[&str]) -> Vec<FrameIssue> {
    let mut issues = Vec::new();
    for frame in frames(buf) {
        for y in frame.top() + 1..frame.bottom().saturating_sub(1) {
            for x in frame.left() + 1..frame.right().saturating_sub(1) {
                let glyph = symbol(buf, x, y);
                if let Some(what) = unstable(glyph, glyphs) {
                    issues.push(FrameIssue {
                        row: y,
                        col: x,
                        what,
                    });
                }
            }
        }
    }
    issues
}

/// The boxes on the screen whose four corners are there, borders included.
fn frames(buf: &Buffer) -> Vec<Rect> {
    let area = buf.area;
    let mut found = Vec::new();
    for top in area.top()..area.bottom() {
        for left in area.left()..area.right() {
            if !is(TOP_LEFT, symbol(buf, left, top)) {
                continue;
            }
            let Some(right) =
                (left + 1..area.right()).find(|&x| is(TOP_RIGHT, symbol(buf, x, top)))
            else {
                continue;
            };
            let bottom = (top + 1..area.bottom()).find(|&y| {
                is(BOTTOM_LEFT, symbol(buf, left, y)) && is(BOTTOM_RIGHT, symbol(buf, right, y))
            });
            if let Some(bottom) = bottom {
                found.push(Rect::new(left, top, right - left + 1, bottom - top + 1));
            }
        }
    }
    found
}

/// [`check_glyphs`] for rows drawn inline.
pub fn check_line_glyphs(lines: &[Line<'_>], glyphs: &[&str]) -> Vec<FrameIssue> {
    let mut issues = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let mut col = 0;
        for span in &line.spans {
            for grapheme in span.content.graphemes(true) {
                if let Some(what) = unstable(grapheme, glyphs) {
                    issues.push(FrameIssue {
                        row: row as u16,
                        col,
                        what,
                    });
                }
                col += width_with(grapheme, 1) as u16;
            }
        }
    }
    issues
}

/// Two block meters on adjacent rows that share a column. The kit records where it drew each
/// one, since a meter's cells look like any colored cells once drawn. The agent panel's run
/// rows copy Claude Code's, which stack this way, and aren't recorded.
pub fn check_block_meters(meters: &[Rect]) -> Vec<FrameIssue> {
    let mut issues = Vec::new();
    for (i, upper) in meters.iter().enumerate() {
        for (j, lower) in meters.iter().enumerate() {
            let touching = i != j && upper.bottom() == lower.top();
            let left = upper.left().max(lower.left());
            let shared = left < upper.right().min(lower.right());
            if touching && shared {
                issues.push(FrameIssue {
                    row: lower.top(),
                    col: left,
                    what: String::from(
                        "a block meter right under another merges with it; use line meters",
                    ),
                });
            }
        }
    }
    issues
}

/// Checks what a mod drew: its rows, as [`check_lines`] does, and its block meters.
pub fn check_drawn(drawn: &Drawn, width: u16) -> Vec<FrameIssue> {
    let mut issues = check_lines(&drawn.lines, width);
    issues.extend(check_block_meters(&drawn.meters));
    issues
}

pub fn assert_drawn(drawn: &Drawn, width: u16) {
    let issues = check_drawn(drawn, width);
    if !issues.is_empty() {
        panic!("{}", report(&lines_text(&drawn.lines), &issues));
    }
}

/// Panics with the screen and every issue, for tests.
pub fn assert_frames(buf: &Buffer) {
    let issues = check(buf);
    if !issues.is_empty() {
        panic!("{}", report(&buffer_text(buf), &issues));
    }
}

pub fn assert_lines(lines: &[Line<'_>], width: u16) {
    let issues = check_lines(lines, width);
    if !issues.is_empty() {
        panic!("{}", report(&lines_text(lines), &issues));
    }
}

/// Panics when the screen draws any of `glyphs`.
pub fn assert_no_glyphs(buf: &Buffer, glyphs: &[&str]) {
    let issues = check_glyphs(buf, glyphs);
    if !issues.is_empty() {
        panic!("{}", report(&buffer_text(buf), &issues));
    }
}

/// Panics when the rows draw any of `glyphs`.
pub fn assert_no_line_glyphs(lines: &[Line<'_>], glyphs: &[&str]) {
    let issues = check_line_glyphs(lines, glyphs);
    if !issues.is_empty() {
        panic!("{}", report(&lines_text(lines), &issues));
    }
}

/// The buffer as plain text, one row per line, wide glyphs once.
pub fn buffer_text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let symbol = buf[(x, y)].symbol();
            out.push_str(symbol);
            x += width_with(symbol, 1).max(1) as u16;
        }
        out.push('\n');
    }
    out
}

fn lines_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect::<Vec<String>>()
        .join("\n")
}

/// The rows as a screen `width` cells wide would hold them, cut at its edge.
fn lines_buffer(lines: &[Line<'_>], width: u16) -> Buffer {
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    for (y, line) in lines.iter().take(usize::from(height)).enumerate() {
        buf.set_line(0, y as u16, line, width);
    }
    buf
}

fn unstable(symbol: &str, glyphs: &[&str]) -> Option<String> {
    if symbol.contains('\u{FE0F}') {
        Some(format!("`{symbol}` asks for emoji presentation"))
    } else if glyphs.contains(&symbol) {
        Some(format!("`{symbol}` can take two cells in some terminals"))
    } else {
        None
    }
}

/// Box drawing and block elements, which frames and bars are made of.
fn is_frame_glyph(symbol: &str) -> bool {
    symbol
        .chars()
        .next()
        .is_some_and(|c| ('\u{2500}'..='\u{259F}').contains(&c))
}

fn report(screen: &str, issues: &[FrameIssue]) -> String {
    let mut text = String::from("frame check failed:\n");
    for issue in issues {
        text.push_str(&format!(
            "  row {}, col {}: {}\n",
            issue.row, issue.col, issue.what
        ));
    }
    text.push_str("screen:\n");
    text.push_str(screen);
    text
}

fn symbol(buf: &Buffer, x: u16, y: u16) -> &str {
    buf[(x, y)].symbol()
}

fn is(set: &[&str], s: &str) -> bool {
    set.contains(&s)
}

fn check_wide_glyphs(buf: &Buffer, ambiguous: u8, issues: &mut Vec<FrameIssue>) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let glyph = symbol(buf, x, y);
            let drawn = width_with(glyph, 1) as u16;
            let width = if is_frame_glyph(glyph) {
                drawn
            } else {
                width_with(glyph, ambiguous) as u16
            };
            if width > 1 {
                if x + width > area.right() {
                    issues.push(FrameIssue {
                        row: y,
                        col: x,
                        what: String::from("a wide glyph crosses the right edge"),
                    });
                }
                for dx in 1..width {
                    let next = x + dx;
                    if next < area.right() && !symbol(buf, next, y).trim().is_empty() {
                        let what = if drawn > 1 {
                            String::from("something drew over half of a wide glyph")
                        } else {
                            format!("`{glyph}` takes two cells here and runs into the next")
                        };
                        issues.push(FrameIssue {
                            row: y,
                            col: next,
                            what,
                        });
                    }
                }
            }
            // The buffer holds a wide glyph's second cell whatever the terminal does.
            x += drawn.max(1);
        }
    }
}

fn check_boxes(buf: &Buffer, issues: &mut Vec<FrameIssue>) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if is(TOP_LEFT, symbol(buf, x, y)) {
                check_box(buf, x, y, issues);
            }
        }
    }
}

/// Follows one box from its top-left corner. Titles may sit on the top edge, and a position
/// label on the bottom edge.
fn check_box(buf: &Buffer, left: u16, top: u16, issues: &mut Vec<FrameIssue>) {
    let area = buf.area;
    let Some(right) = (left + 1..area.right()).find(|&x| is(TOP_RIGHT, symbol(buf, x, top))) else {
        issues.push(FrameIssue {
            row: top,
            col: left,
            what: String::from("a box's top edge never closes"),
        });
        return;
    };
    for y in top + 1..area.bottom() {
        let l = symbol(buf, left, y);
        let r = symbol(buf, right, y);
        if is(BOTTOM_LEFT, l) {
            if !is(BOTTOM_RIGHT, r) {
                issues.push(FrameIssue {
                    row: y,
                    col: right,
                    what: format!("a box's bottom corners don't line up (found `{r}`)"),
                });
            }
            // A label such as `1–24 of 34 ↓` may sit on the bottom edge, as Claude Code's
            // does, so long as the edge meets both corners.
            let edge = |x: u16| {
                let s = symbol(buf, x, y);
                is(HORIZONTAL, s) || is(BOTTOM_JOINS, s) || s.is_empty()
            };
            let labeled = (left + 1..right).any(|x| symbol(buf, x, y) == " ");
            let broken = if labeled {
                [left + 1, right.saturating_sub(1)]
                    .into_iter()
                    .find(|&x| x > left && x < right && !edge(x))
            } else {
                (left + 1..right).find(|&x| !edge(x))
            };
            if let Some(x) = broken {
                issues.push(FrameIssue {
                    row: y,
                    col: x,
                    what: String::from("a box's bottom edge is broken"),
                });
            }
            return;
        }
        if !(is(VERTICAL, l) || is(LEFT_JOINS, l)) {
            issues.push(FrameIssue {
                row: y,
                col: left,
                what: format!("a box's left border is broken (found `{l}`)"),
            });
            return;
        }
        if !(is(VERTICAL, r) || is(RIGHT_JOINS, r)) {
            issues.push(FrameIssue {
                row: y,
                col: right,
                what: format!("a box's right border is broken (found `{r}`)"),
            });
            return;
        }
    }
    issues.push(FrameIssue {
        row: top,
        col: left,
        what: String::from("a box never closes at the bottom"),
    });
}

/// A vertical separator that continues one column over on the next row is a jagged border.
fn check_separators(buf: &Buffer, issues: &mut Vec<FrameIssue>) {
    let area = buf.area;
    for y in area.top()..area.bottom().saturating_sub(1) {
        for x in area.left()..area.right() {
            if !is(VERTICAL, symbol(buf, x, y)) {
                continue;
            }
            let below = symbol(buf, x, y + 1);
            let continues = is(VERTICAL, below)
                || is(BOTTOM_LEFT, below)
                || is(BOTTOM_RIGHT, below)
                || is(LEFT_JOINS, below)
                || is(RIGHT_JOINS, below)
                || is(BOTTOM_JOINS, below)
                || below == "┼";
            if continues {
                continue;
            }
            let shifted = [x.checked_sub(1), x.checked_add(1)]
                .into_iter()
                .flatten()
                .filter(|&nx| nx >= area.left() && nx < area.right())
                .any(|nx| is(VERTICAL, symbol(buf, nx, y + 1)));
            let above_starts =
                y == area.top() || !is(VERTICAL, symbol(buf, x, y.saturating_sub(1)));
            // A one-row `│` in text is fine; a run that jumps a column isn't.
            if shifted && !above_starts {
                issues.push(FrameIssue {
                    row: y + 1,
                    col: x,
                    what: String::from("a vertical border jumps a column"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::widgets::{Block, Borders, Paragraph, Widget};

    use super::*;

    fn boxed(width: u16, text: &str) -> Buffer {
        let area = Rect::new(0, 0, width, 5);
        let mut buf = Buffer::empty(area);
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Phases"))
            .render(area, &mut buf);
        buf
    }

    #[test]
    fn a_ratatui_box_is_sound_at_every_width() {
        for width in WIDTHS {
            for text in [
                samples::CJK,
                samples::EMOJI,
                samples::LONG_PATH,
                samples::EMPTY,
            ] {
                assert_frames(&boxed(width, text));
            }
            assert_frames(&boxed(width, &samples::long_word()));
        }
    }

    #[test]
    fn text_over_a_border_is_caught() {
        let mut buf = boxed(20, "ok");
        buf[(19, 2)].set_symbol("x");
        let issues = check(&buf);
        assert!(
            issues.iter().any(|i| i.what.contains("right border")),
            "{issues:?}"
        );
    }

    #[test]
    fn a_jagged_separator_is_caught() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        buf[(4, 0)].set_symbol("│");
        buf[(4, 1)].set_symbol("│");
        buf[(5, 2)].set_symbol("│");
        let issues = check(&buf);
        assert!(
            issues.iter().any(|i| i.what.contains("jumps")),
            "{issues:?}"
        );
    }

    #[test]
    fn a_wide_glyph_at_the_edge_is_caught() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf[(3, 0)].set_symbol("中");
        assert!(!check(&buf).is_empty());
    }

    #[test]
    fn inline_rows_must_fit() {
        let lines = vec![Line::from("x".repeat(41))];
        assert_eq!(check_lines(&lines, 40).len(), 1);
        assert!(check_lines(&lines, 41).is_empty());
    }

    #[test]
    fn a_box_drawn_inline_must_close_in_one_column() {
        let sound = vec![
            Line::from("╭────╮"),
            Line::from("│ ok │"),
            Line::from("╰────╯"),
        ];
        assert!(check_lines(&sound, 10).is_empty());
        let jagged = vec![
            Line::from("╭────╮"),
            Line::from("│ 修复 │"),
            Line::from("╰────╯"),
        ];
        let issues = check_lines(&jagged, 10);
        assert!(
            issues.iter().any(|i| i.what.contains("right border")),
            "{issues:?}"
        );
    }

    #[test]
    fn ambiguous_glyphs_run_into_text_at_two_cells() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        buf.set_string(0, 0, "●done", ratatui::style::Style::default());
        assert!(check_with(&buf, 1).is_empty());
        let issues = check_with(&buf, 2);
        assert!(
            issues.iter().any(|i| i.what.contains("two cells")),
            "{issues:?}"
        );
        // Frames stay one cell: the border is the frame's own font.
        assert!(check_with(&boxed(20, "ok"), 2).is_empty());
    }

    #[test]
    fn unstable_glyphs_and_emoji_presentation_are_found() {
        let mut buf = boxed(12, "* ok");
        assert!(check_glyphs(&buf, &UNSTABLE_GLYPHS).is_empty());
        buf[(5, 1)].set_symbol("◇");
        assert_eq!(check_glyphs(&buf, &UNSTABLE_GLYPHS).len(), 1);
        // Outside a frame, such as a key hint under it, the glyph isn't in a frame.
        let mut open = Buffer::empty(Rect::new(0, 0, 8, 1));
        open.set_string(0, 0, "⏎ open", ratatui::style::Style::default());
        assert!(check_glyphs(&open, &UNSTABLE_GLYPHS).is_empty());

        let lines = vec![Line::from("ok ✔ \u{2714}\u{FE0F}"), Line::from("fine")];
        let issues = check_line_glyphs(&lines, &UNSTABLE_GLYPHS);
        assert_eq!(issues.len(), 2, "{issues:?}");
        assert_eq!(issues[0].col, 3);
    }

    #[test]
    fn an_ascii_divider_meets_the_edges() {
        let mut buf = boxed(12, "");
        for y in 1..4 {
            buf[(4, y)].set_symbol("|");
        }
        buf[(4, 0)].set_symbol("+");
        buf[(4, 4)].set_symbol("+");
        assert!(check(&buf).is_empty(), "{:?}", check(&buf));
        buf[(4, 3)].set_symbol(" ");
        buf[(5, 3)].set_symbol("|");
        assert!(check(&buf).iter().any(|i| i.what.contains("jumps")));
    }

    #[test]
    fn two_block_meters_on_adjacent_rows_fail_and_one_tall_meter_passes() {
        let upper = Rect::new(2, 3, 20, 1);
        let lower = Rect::new(10, 4, 20, 1);
        let issues = check_block_meters(&[upper, lower]);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!((issues[0].row, issues[0].col), (4, 10));
        // A row of text between them, or no shared column, keeps them apart.
        assert!(check_block_meters(&[upper, Rect::new(2, 5, 20, 1)]).is_empty());
        assert!(check_block_meters(&[upper, Rect::new(22, 4, 5, 1)]).is_empty());
        // One meter drawn two rows high on purpose is one meter.
        assert!(check_block_meters(&[Rect::new(2, 3, 20, 2)]).is_empty());
    }
}
