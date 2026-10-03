//! Selecting text in the fullscreen app, which has the mouse, so the terminal can't select by
//! itself. A drag selects screen cells, a double-click a word, and a triple-click a row, and
//! the text is copied on release. A selection lives in screen cells and holds still while the
//! frame under it changes; a finished one goes away once it no longer covers what it copied.
//!
//! The text copied is what the renderer marked as text: a gutter glyph, a border, or a rule
//! copies as a blank, and a row a long line wrapped onto joins the row above it, as the
//! line was written (see [`crate::text::marks`]).

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::text::marks::{self, Join};
use crate::text::width_with;

/// Presses this close together on one cell make a double or a triple click.
const MULTI_CLICK: Duration = Duration::from_millis(500);

/// Besides letters and digits, what a double-click keeps in a word, so paths and URLs come
/// whole.
const WORD_PUNCTUATION: &str = "_-./~+@%#?&=:";

/// Ends a sentence more often than a path, so a double-click leaves it off the end.
const TRAILING_PUNCTUATION: &str = ".:?";

/// A screen cell. Rows come first, so cells order the way you read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: u16,
    pub column: u16,
}

impl Point {
    pub fn new(column: u16, row: u16) -> Self {
        Self { row, column }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Cell,
    Word,
    Row,
}

#[derive(Debug, Clone)]
struct Selection {
    /// What the press picked: a cell, a word, or a row. Dragging grows the selection from it
    /// in the same unit.
    anchor: (Point, Point),
    unit: Unit,
    /// Both ends are selected.
    start: Point,
    end: Point,
    /// The button is still down.
    held: bool,
    /// What the release copied, to tell when the screen under the selection changed.
    copied: Option<String>,
}

impl Selection {
    /// A press that hasn't moved selects nothing yet, unless it was a double or triple click.
    fn shows(&self) -> bool {
        self.unit != Unit::Cell || self.start != self.end
    }
}

#[derive(Debug, Default)]
pub struct Selecting {
    selection: Option<Selection>,
    last_press: Option<(Instant, Point)>,
    clicks: u8,
}

impl Selecting {
    /// The left button went down at `at` over `screen`. True when the screen should redraw.
    pub fn press(&mut self, at: Point, now: Instant, screen: &Buffer) -> bool {
        let again = self.last_press.is_some_and(|(when, spot)| {
            spot == at && now.saturating_duration_since(when) <= MULTI_CLICK
        });
        self.clicks = if again { self.clicks % 3 + 1 } else { 1 };
        self.last_press = Some((now, at));
        let unit = match self.clicks {
            2 => Unit::Word,
            3 => Unit::Row,
            _ => Unit::Cell,
        };
        let shown = self.is_shown();
        let anchor = expand(screen, at, unit);
        self.selection = Some(Selection {
            anchor,
            unit,
            start: anchor.0,
            end: anchor.1,
            held: true,
            copied: None,
        });
        shown || self.is_shown()
    }

    /// The pointer moved to `at` with the button down. True when the selection changed.
    pub fn drag(&mut self, at: Point, screen: &Buffer) -> bool {
        let Some(selection) = self.selection.as_mut().filter(|selection| selection.held) else {
            return false;
        };
        // After a drag, a press where it began is a new first click.
        if self.last_press.is_some_and(|(_, spot)| spot != at) {
            self.last_press = None;
        }
        let head = expand(screen, at, selection.unit);
        let start = selection.anchor.0.min(head.0);
        let end = selection.anchor.1.max(head.1);
        let changed = (start, end) != (selection.start, selection.end);
        selection.start = start;
        selection.end = end;
        changed
    }

    /// The button came up at `at`: the text to copy, if a drag or a double or triple click
    /// selected any.
    pub fn release(&mut self, at: Point, screen: &Buffer) -> Option<String> {
        self.drag(at, screen);
        let selection = self.selection.as_mut().filter(|selection| selection.held)?;
        selection.held = false;
        let text = selection
            .shows()
            .then(|| copy_text(screen, selection.start, selection.end))
            .filter(|text| !text.is_empty());
        match &text {
            Some(text) => selection.copied = Some(text.clone()),
            None => self.selection = None,
        }
        text
    }

    /// Whether a selection shows on screen.
    pub fn is_shown(&self) -> bool {
        self.selection.as_ref().is_some_and(Selection::shows)
    }

    /// True when a selection showed.
    pub fn clear(&mut self) -> bool {
        let shown = self.is_shown();
        self.selection = None;
        shown
    }

    /// Draws the selection on `frame` in `style`. A finished selection whose text moved away,
    /// such as when the view scrolled, goes instead.
    pub fn paint(&mut self, frame: &mut Buffer, style: Style) {
        let Some(selection) = self
            .selection
            .as_ref()
            .filter(|selection| selection.shows())
        else {
            return;
        };
        if let Some(copied) = &selection.copied
            && copy_text(frame, selection.start, selection.end) != *copied
        {
            self.selection = None;
            return;
        }
        let area = frame.area;
        let last_row = selection.end.row.min(area.bottom().saturating_sub(1));
        for row in selection.start.row.max(area.top())..=last_row {
            let from = if row == selection.start.row {
                selection.start.column
            } else {
                area.left()
            };
            let to = if row == selection.end.row {
                selection.end.column
            } else {
                area.right().saturating_sub(1)
            };
            // Like the copied text, the highlight stops where the row's text does.
            let Some(text_end) = row_cells(frame, row)
                .iter()
                .rev()
                .find(|cell| cell.column <= to && cell.is_text())
                .map(|cell| cell.column + cell.width - 1)
            else {
                continue;
            };
            let to = to.min(text_end);
            if from <= to {
                frame.set_style(Rect::new(from, row, to - from + 1, 1), style);
            }
        }
    }
}

/// One character on screen, which may take two cells, with what the renderer marked on it.
#[derive(Debug)]
struct ScreenChar<'a> {
    column: u16,
    width: u16,
    symbol: &'a str,
    /// Drawn only to decorate, such as a gutter glyph or a border, so it copies as a blank.
    decoration: bool,
    /// Where a wrapped line resumes, and how it joins the row above.
    join: Option<Join>,
}

impl ScreenChar<'_> {
    fn is_text(&self) -> bool {
        !self.decoration && !is_blank(self.symbol)
    }

    /// What it adds to the copied text.
    fn copied(&self) -> &str {
        if self.decoration { " " } else { self.symbol }
    }
}

/// The characters of `row`, a wide one once rather than with the cell it hides.
fn row_cells(screen: &Buffer, row: u16) -> Vec<ScreenChar<'_>> {
    let area = screen.area;
    let mut cells = Vec::with_capacity(usize::from(area.width));
    if row < area.top() || row >= area.bottom() {
        return cells;
    }
    let mut column = area.left();
    while column < area.right() {
        let cell = &screen[(column, row)];
        let symbol = cell.symbol();
        let width = u16::try_from(width_with(symbol, 1).max(1)).unwrap_or(1);
        cells.push(ScreenChar {
            column,
            width,
            symbol,
            decoration: marks::is_decoration(cell),
            join: marks::join_at(cell),
        });
        column = column.saturating_add(width);
    }
    cells
}

/// How `cells` go on from the row above, and the column the wrapped text resumes at. Only
/// blanks and decoration may come before it: a mark further in, such as in a table's second
/// column, is a cell's own wrap, and the row is a row of its own.
fn row_join(cells: &[ScreenChar<'_>]) -> Option<(Join, u16)> {
    for cell in cells {
        if let Some(join) = cell.join {
            return Some((join, cell.column));
        }
        if cell.is_text() {
            return None;
        }
    }
    None
}

/// The first and last cell of what one press picks at `at`.
fn expand(screen: &Buffer, at: Point, unit: Unit) -> (Point, Point) {
    let area = screen.area;
    let at = Point::new(
        at.column.clamp(area.left(), area.right().saturating_sub(1)),
        at.row.clamp(area.top(), area.bottom().saturating_sub(1)),
    );
    match unit {
        Unit::Cell => (at, at),
        Unit::Row => (
            Point::new(area.left(), at.row),
            Point::new(area.right().saturating_sub(1), at.row),
        ),
        Unit::Word => word_at(screen, at),
    }
}

/// A run of word characters, or of blanks, or one other character.
fn word_at(screen: &Buffer, at: Point) -> (Point, Point) {
    let cells = row_cells(screen, at.row);
    let Some(hit) = cells
        .iter()
        .position(|cell| at.column < cell.column + cell.width)
    else {
        return (at, at);
    };
    let kind_of = |cell: &ScreenChar<'_>| char_kind(cell.copied());
    let kind = kind_of(&cells[hit]);
    let same = |cell: &ScreenChar<'_>| kind_of(cell) == kind;
    let mut first = hit;
    while first > 0 && same(&cells[first - 1]) {
        first -= 1;
    }
    let mut last = hit;
    while last + 1 < cells.len() && same(&cells[last + 1]) {
        last += 1;
    }
    if kind == CharKind::Word {
        while last > hit
            && cells[last]
                .symbol
                .chars()
                .all(|c| TRAILING_PUNCTUATION.contains(c))
        {
            last -= 1;
        }
    }
    let end = &cells[last];
    (
        Point::new(cells[first].column, at.row),
        Point::new(end.column + end.width - 1, at.row),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Word,
    Blank,
    Other,
}

fn char_kind(symbol: &str) -> CharKind {
    match symbol.chars().next() {
        None | Some(' ') => CharKind::Blank,
        Some(c) if c.is_alphanumeric() || WORD_PUNCTUATION.contains(c) => CharKind::Word,
        Some(_) => CharKind::Other,
    }
}

/// The selected text as you'd paste it: without the decoration, the margin the rows share, or
/// the padding on the right, and with each wrapped line whole again. Blank rows at either end
/// go too.
pub fn copy_text(screen: &Buffer, start: Point, end: Point) -> String {
    let area = screen.area;
    let last_row = end.row.min(area.bottom().saturating_sub(1));
    let mut rows: Vec<(Vec<ScreenChar<'_>>, u16, u16, Join)> = Vec::new();
    for row in start.row.max(area.top())..=last_row {
        let cells = row_cells(screen, row);
        let mut from = if row == start.row {
            start.column
        } else {
            area.left()
        };
        let to = if row == end.row {
            end.column
        } else {
            area.right().saturating_sub(1)
        };
        // A wrapped row resumes the row above, so whatever leads it, such as a hanging
        // indent, stays out. The first row joins nothing.
        let join = match row_join(&cells) {
            Some((join, resumes)) if !rows.is_empty() => {
                from = from.max(resumes);
                join
            }
            _ => Join::NewLine,
        };
        rows.push((cells, from, to, join));
    }
    let picked =
        |cell: &ScreenChar<'_>, from: u16, to: u16| cell.column >= from && cell.column <= to;
    // The margin is where the leftmost selected row's text starts, so nested rows such as code
    // keep their indent relative to it.
    let margin = rows
        .iter()
        .filter(|(_, _, _, join)| *join == Join::NewLine)
        .filter(|(cells, from, to, _)| {
            cells
                .iter()
                .any(|cell| picked(cell, *from, *to) && cell.is_text())
        })
        .filter_map(|(cells, _, _, _)| {
            cells
                .iter()
                .find(|cell| cell.is_text())
                .map(|cell| cell.column)
        })
        .min()
        .unwrap_or(area.left());
    let mut lines: Vec<String> = Vec::new();
    for (cells, from, to, join) in &rows {
        let from = match join {
            Join::NewLine => (*from).max(margin),
            Join::Space | Join::Direct => *from,
        };
        let text: String = cells
            .iter()
            .filter(|cell| picked(cell, from, *to))
            .map(ScreenChar::copied)
            .collect();
        let text = text.trim_end();
        match (join, lines.last_mut()) {
            (Join::Space, Some(line)) => {
                line.truncate(line.trim_end().len());
                line.push(' ');
                line.push_str(text);
            }
            (Join::Direct, Some(line)) => line.push_str(text),
            _ => lines.push(text.to_owned()),
        }
    }
    let first = lines.iter().position(|line| !line.is_empty());
    let last = lines.iter().rposition(|line| !line.is_empty());
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => String::new(),
    }
}

fn is_blank(symbol: &str) -> bool {
    symbol.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};
    use ratatui::text::{Line, Span};

    use super::*;
    use crate::terminal::paint_rows;
    use crate::text::marks::{continues, decorative};

    /// A row as a renderer marks it: `⟦...⟧` is decoration, and `»` or `›` is where a wrapped
    /// line resumes, after a space or inside a word.
    fn row(text: &str) -> Line<'static> {
        let mut spans = Vec::new();
        let mut join = None;
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('⟦') {
                let (decoration, tail) = after.split_once('⟧').expect("a closing bracket");
                spans.push(decorative(decoration.to_owned(), Style::new()));
                rest = tail;
            } else if let Some(tail) = rest.strip_prefix('»') {
                join = Some((spans.len(), Join::Space));
                rest = tail;
            } else if let Some(tail) = rest.strip_prefix('›') {
                join = Some((spans.len(), Join::Direct));
                rest = tail;
            } else {
                let end = rest.find(['⟦', '»', '›']).unwrap_or(rest.len());
                spans.push(Span::raw(rest[..end].to_owned()));
                rest = &rest[end..];
            }
        }
        if let Some((at, join)) = join {
            continues(&mut spans[at..], join);
        }
        Line::from(spans)
    }

    fn screen(rows: &[&str]) -> Buffer {
        let width = 40;
        let area = Rect::new(0, 0, width, u16::try_from(rows.len()).unwrap());
        let mut buffer = Buffer::empty(area);
        let lines: Vec<Line<'_>> = rows.iter().map(|text| row(text)).collect();
        paint_rows(&mut buffer, area, &lines);
        buffer
    }

    fn text(buffer: &Buffer, from: (u16, u16), to: (u16, u16)) -> String {
        copy_text(buffer, Point::new(from.0, from.1), Point::new(to.0, to.1))
    }

    const ANSWER: [&str; 6] = [
        "⟦●⟧ Answer 4. The export writes JSON",
        "  »indents it.",
        "",
        "      fn main() {}",
        "",
        "⟦●⟧ GPT on Codex · owner",
    ];

    #[test]
    fn copied_text_leaves_out_the_decoration_margin_and_padding() {
        let buffer = screen(&ANSWER);
        assert_eq!(
            text(&buffer, (0, 0), (39, 3)),
            "Answer 4. The export writes JSON indents it.\n\n    fn main() {}"
        );
        // A selection that starts inside a row starts there.
        assert_eq!(text(&buffer, (23, 0), (8, 1)), "writes JSON indents");
        // Blank rows at either end and the right padding go.
        assert_eq!(text(&buffer, (0, 2), (39, 4)), "fn main() {}");
    }

    #[test]
    fn wrapped_lines_copy_whole_and_a_cells_own_wrap_stays_a_row() {
        let buffer = screen(&[
            "⟦●⟧ see https://example.com/going/and/",
            "  ›going/until/here",
            "⟦●⟧ run pytest -q -k 'test_*.py'",
            "  »--verbose now",
            "│ a │ a long cell │",
            "│   │ »continued  │",
        ]);
        assert_eq!(
            text(&buffer, (0, 0), (39, 3)),
            "see https://example.com/going/and/going/until/here\n\
             run pytest -q -k 'test_*.py' --verbose now"
        );
        assert_eq!(
            text(&buffer, (0, 4), (39, 5)),
            "│ a │ a long cell │\n│   │ continued  │"
        );
    }

    #[test]
    fn the_result_gutter_and_the_prompt_are_not_text() {
        let buffer = screen(&[
            "⟦●⟧ Bash(ls)",
            "  ⟦⎿⟧  Cargo.toml",
            "     src",
            "⟦❯⟧ what is in src?",
            "  ⎿  stays text when it isn't marked",
        ]);
        assert_eq!(text(&buffer, (0, 1), (39, 2)), "Cargo.toml\nsrc");
        assert_eq!(text(&buffer, (0, 1), (39, 1)), "Cargo.toml");
        assert_eq!(text(&buffer, (0, 3), (39, 3)), "what is in src?");
        assert_eq!(text(&buffer, (0, 0), (39, 1)), "Bash(ls)\n   Cargo.toml");
        assert_eq!(
            text(&buffer, (0, 4), (39, 4)),
            "⎿  stays text when it isn't marked"
        );
    }

    #[test]
    fn a_frames_lines_are_not_text_and_the_same_glyphs_in_text_are() {
        let buffer = screen(&[
            "⟦╭─────────────────╮⟧",
            "⟦│⟧ ctrl+c  clear ⟦│⟧",
            "⟦╰─────────────────╯⟧",
            "│   ├── src/",
            "╭─────╮",
            "│ app │",
            "╰─────╯",
        ]);
        assert_eq!(text(&buffer, (0, 0), (39, 2)), "ctrl+c  clear");
        assert_eq!(
            text(&buffer, (0, 3), (39, 6)),
            "│   ├── src/\n╭─────╮\n│ app │\n╰─────╯"
        );
    }

    #[test]
    fn wide_characters_copy_once() {
        let buffer = screen(&["⟦●⟧ 中文 text"]);
        assert_eq!(text(&buffer, (0, 0), (39, 0)), "中文 text");
        // A selection ending on the second half of a wide character takes it whole.
        assert_eq!(text(&buffer, (2, 0), (3, 0)), "中");
    }

    #[test]
    fn a_click_selects_nothing_and_a_drag_selects_cells() {
        let buffer = screen(&ANSWER);
        let now = Instant::now();
        let mut selecting = Selecting::default();
        assert!(!selecting.press(Point::new(2, 0), now, &buffer));
        assert!(!selecting.is_shown());
        assert_eq!(selecting.release(Point::new(2, 0), &buffer), None);
        assert!(!selecting.is_shown());

        let later = now + Duration::from_secs(2);
        selecting.press(Point::new(9, 0), later, &buffer);
        assert!(selecting.drag(Point::new(4, 0), &buffer));
        assert!(selecting.is_shown());
        // Dragging back before the press selects backwards from it.
        assert_eq!(
            selecting.release(Point::new(4, 0), &buffer).as_deref(),
            Some("swer 4")
        );
        assert!(selecting.is_shown(), "the selection stays after the copy");
        assert!(selecting.clear());
        assert!(!selecting.is_shown());

        // A quick press where the drag began is a first click, not a double one.
        selecting.press(Point::new(9, 0), later, &buffer);
        assert!(!selecting.is_shown());
    }

    #[test]
    fn double_click_takes_a_word_and_triple_click_a_row() {
        let buffer = screen(&[
            "⟦●⟧ See https://example.com/a?b=c. Then",
            "  ⟦⎿⟧  read src/app/mod.rs:12 and go",
        ]);
        let now = Instant::now();
        let at = Point::new(10, 0);
        let mut selecting = Selecting::default();
        let click = |selecting: &mut Selecting, n: u64| {
            let when = now + Duration::from_millis(100 * n);
            selecting.press(at, when, &buffer);
            selecting.release(at, &buffer)
        };
        assert_eq!(click(&mut selecting, 0), None);
        assert_eq!(
            click(&mut selecting, 1).as_deref(),
            Some("https://example.com/a?b=c")
        );
        assert_eq!(
            click(&mut selecting, 2).as_deref(),
            Some("See https://example.com/a?b=c. Then")
        );
        // A fourth press starts over.
        assert_eq!(click(&mut selecting, 3), None);

        // Too slow, or on another cell, is a new first click.
        let mut slow = Selecting::default();
        slow.press(at, now, &buffer);
        slow.release(at, &buffer);
        slow.press(at, now + Duration::from_secs(1), &buffer);
        assert!(!slow.is_shown());

        let mut path = Selecting::default();
        let on = Point::new(12, 1);
        path.press(on, now, &buffer);
        path.release(on, &buffer);
        path.press(on, now, &buffer);
        assert_eq!(
            path.release(on, &buffer).as_deref(),
            Some("src/app/mod.rs:12")
        );
    }

    #[test]
    fn dragging_after_a_double_click_grows_by_words() {
        let buffer = screen(&["alpha beta gamma delta"]);
        let now = Instant::now();
        let mut selecting = Selecting::default();
        let at = Point::new(7, 0);
        selecting.press(at, now, &buffer);
        selecting.release(at, &buffer);
        selecting.press(at, now, &buffer);
        selecting.drag(Point::new(13, 0), &buffer);
        assert_eq!(
            selecting.release(Point::new(13, 0), &buffer).as_deref(),
            Some("beta gamma")
        );
    }

    #[test]
    fn the_selection_paints_over_text_and_goes_when_the_text_moves() {
        let mut buffer = screen(&ANSWER);
        let now = Instant::now();
        let mut selecting = Selecting::default();
        selecting.press(Point::new(2, 0), now, &buffer);
        selecting.drag(Point::new(30, 1), &buffer);
        selecting.release(Point::new(30, 1), &buffer);
        let style = Style::new().bg(Color::Blue);
        selecting.paint(&mut buffer, style);
        assert_eq!(buffer[(2, 0)].bg, Color::Blue);
        assert_eq!(buffer[(33, 0)].bg, Color::Blue, "the row's last character");
        assert_eq!(buffer[(34, 0)].bg, Color::Reset, "not the padding after it");
        assert_eq!(buffer[(12, 1)].bg, Color::Blue);
        assert_eq!(buffer[(13, 1)].bg, Color::Reset);
        assert_eq!(buffer[(1, 0)].bg, Color::Reset, "before the start");

        // The same frame again keeps it; a frame whose rows moved lets it go.
        let mut same = screen(&ANSWER);
        selecting.paint(&mut same, style);
        assert!(selecting.is_shown());
        let mut moved = screen(&ANSWER[1..]);
        selecting.paint(&mut moved, style);
        assert!(!selecting.is_shown());
        assert_eq!(moved[(2, 0)].bg, Color::Reset);

        // No color: reversed cells.
        let mut plain = screen(&ANSWER);
        selecting.press(Point::new(2, 0), now + Duration::from_secs(5), &plain);
        selecting.drag(Point::new(5, 0), &plain);
        selecting.paint(&mut plain, Style::new().add_modifier(Modifier::REVERSED));
        assert!(plain[(3, 0)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn points_outside_the_screen_stay_on_its_edge() {
        let buffer = screen(&["abc"]);
        let (start, end) = expand(&buffer, Point::new(99, 9), Unit::Row);
        assert_eq!((start, end), (Point::new(0, 0), Point::new(39, 0)));
        assert_eq!(
            expand(&buffer, Point::new(99, 9), Unit::Cell).0,
            Point::new(39, 0)
        );
        assert_eq!(text(&buffer, (0, 0), (99, 9)), "abc");
    }
}
