//! The kit every view that shows numbers is built from: a frame, a header with its badge,
//! bars and meters, and a legend, with words for no color and for screen readers. Each part
//! takes plain data, so any view can feed it, and draws rows of spans a given width.
//!
//! Four rules hold everywhere. No two block meters sit on back-to-back rows, since a terminal
//! joins them into one slab; [`Drawn`] records where each one is so the frame checker can tell.
//! Ratios inside a frame are bars, and in a one-line row they're pills. Color is never the
//! only signal: every bar has its number and every badge its words. And cards stand one blank
//! row apart, never two.

pub mod badge;
pub mod bars;
pub mod bodies;
pub mod context;
pub mod frame;
pub mod header;
pub mod legend;
pub mod level;
pub mod logo;
pub mod state;
pub mod words;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::base::text::spans_width;
use crate::base::theme::{Role, Theme};

pub use badge::Badge;
pub use bodies::{Chart, ListRow, ListSlots, MeterRow, MeterSlots, Window};
pub use context::{ContextCard, ContextPart, Limit, LimitKind};
pub use frame::{Frame, Toggle};
pub use header::Header;
pub use legend::Entry;
pub use level::{Level, LevelWatch, Levels};
pub use state::{State, States};
pub use words::KitGlyphs;

/// How much of a mod shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// Frame, header, body, and legend.
    Full,
    /// Frame, header, and the bar, for a crowded band or half-width pairs.
    Compact,
    /// One row, no frame, as a folded mod or on a narrow screen.
    Line,
    /// A few words on the bottom line.
    Chip,
}

/// What the kit draws with: the theme, and the glyphs that fit it.
#[derive(Debug, Clone, Copy)]
pub struct Kit<'a> {
    pub theme: &'a Theme,
    pub glyphs: KitGlyphs,
}

impl<'a> Kit<'a> {
    pub fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            glyphs: KitGlyphs::for_theme(theme),
        }
    }

    /// Without color, bars draw used and free as glyphs and badges get brackets.
    pub fn has_color(&self) -> bool {
        self.theme.has_color()
    }

    pub fn style(&self, role: Role) -> Style {
        self.theme.style(role)
    }

    /// The role's color as text, even where the palette gives it as a fill.
    pub fn fg(&self, role: Role) -> Style {
        match self.theme.color(role) {
            Some(color) => Style::new().fg(color),
            None => self.style(role),
        }
    }

    /// A cell drawn in `fg` on `bg`, with both roles' colors whichever way the palette gives them.
    pub fn cell(&self, fg: Role, bg: Role) -> Style {
        let mut style = Style::new();
        style.fg = self.theme.color(fg);
        style.bg = self.theme.color(bg);
        style
    }

    /// A cell filled with the role's color.
    pub fn fill(&self, role: Role) -> Style {
        let mut style = Style::new();
        style.bg = self.theme.color(role);
        style
    }

    pub fn bold(&self, role: Role) -> Style {
        self.style(role).add_modifier(Modifier::BOLD)
    }

    pub fn span(&self, text: impl Into<String>, role: Role) -> Span<'static> {
        Span::styled(text.into(), self.style(role))
    }

    pub fn dim(&self, text: impl Into<String>) -> Span<'static> {
        self.span(text, Role::Dim)
    }

    pub fn strong(&self, text: impl Into<String>) -> Span<'static> {
        Span::styled(text.into(), self.bold(Role::Bright))
    }
}

/// Rows a mod drew, and the block meters among them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Drawn {
    pub lines: Vec<Line<'static>>,
    /// Each block meter's cells, in the rows and columns of `lines`. A meter drawn on two rows
    /// on purpose, as the Context tab's bar is, is one area two rows high.
    pub meters: Vec<Rect>,
}

impl Drawn {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn height(&self) -> usize {
        self.lines.len()
    }

    pub fn push(&mut self, spans: Vec<Span<'static>>) {
        self.lines.push(Line::from(spans));
    }

    /// Adds a block meter `cells` wide, starting `left` columns in, drawn on `rows` rows. A
    /// meter drawn taller on purpose is still one meter.
    pub fn push_meter(
        &mut self,
        spans: Vec<Span<'static>>,
        left: usize,
        cells: usize,
        rows: usize,
    ) {
        let top = self.lines.len() as u16;
        self.meters
            .push(Rect::new(left as u16, top, cells as u16, rows as u16));
        for _ in 0..rows {
            self.push(spans.clone());
        }
    }

    /// Puts `other` below these rows.
    pub fn append(&mut self, other: Drawn) {
        let top = self.lines.len() as u16;
        self.meters.extend(other.meters.into_iter().map(|mut area| {
            area.y += top;
            area
        }));
        self.lines.extend(other.lines);
    }

    /// Moves every row `cols` columns to the right.
    pub fn indent(&mut self, cols: usize) {
        if cols == 0 {
            return;
        }
        for line in &mut self.lines {
            line.spans.insert(0, Span::raw(" ".repeat(cols)));
        }
        for area in &mut self.meters {
            area.x += cols as u16;
        }
    }

    /// The widest row, in cells.
    pub fn width(&self) -> usize {
        self.lines
            .iter()
            .map(|line| spans_width(&line.spans))
            .max()
            .unwrap_or(0)
    }
}

/// Cards one under another with one blank row between them, never two.
pub fn stack_cards(cards: impl IntoIterator<Item = Drawn>) -> Drawn {
    let mut out = Drawn::new();
    for card in cards {
        if card.lines.is_empty() {
            continue;
        }
        if !out.lines.is_empty() {
            out.lines.push(Line::default());
        }
        out.append(card);
    }
    out
}

/// `right` beside `left`, starting `at` columns in. The shorter one ends in blank rows.
pub fn side_by_side(left: Drawn, right: Drawn, at: usize) -> Drawn {
    let rows = left.lines.len().max(right.lines.len());
    let mut out = Drawn::new();
    let mut lefts = left.lines.into_iter();
    let mut rights = right.lines.into_iter();
    for _ in 0..rows {
        let mut row = lefts.next().map(|line| line.spans).unwrap_or_default();
        let pad = at.saturating_sub(spans_width(&row));
        row.push(Span::raw(" ".repeat(pad)));
        row.extend(rights.next().map(|line| line.spans).unwrap_or_default());
        out.push(row);
    }
    out
}

/// `spans` padded with spaces to `width` cells, or cut with `…` when wider.
pub fn fit(mut spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let used = spans_width(&spans);
    if used > width {
        return crate::base::text::truncate_spans(&spans, width);
    }
    if used < width {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
    spans
}

/// `left` and `right` on one row `width` cells wide, with the space between them.
pub fn spread(
    mut left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> Vec<Span<'static>> {
    let gap = width.saturating_sub(spans_width(&left) + spans_width(&right));
    if gap > 0 {
        left.push(Span::raw(" ".repeat(gap)));
    }
    left.extend(right);
    left
}

/// Joins neighbors drawn the same way, so a bar is a few spans rather than one per cell.
pub fn merge(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    for span in spans {
        match out.last_mut() {
            Some(last) if last.style == span.style => {
                last.content.to_mut().push_str(&span.content);
            }
            _ => out.push(span),
        }
    }
    out
}

/// Text padded on the right to `width` cells.
pub(crate) fn ljust(text: &str, width: usize) -> String {
    let used = crate::base::text::str_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(used)))
}

/// Text padded on the left to `width` cells.
pub(crate) fn rjust(text: &str, width: usize) -> String {
    let used = crate::base::text::str_width(text);
    format!("{}{text}", " ".repeat(width.saturating_sub(used)))
}

/// The row the first start after an update shows above the input until your first key, as
/// `● Updated to 0.1.1 · /release-notes`: the bullet in the success colour, the words in the
/// text's, and the command they end with in the code colour. The words are the app's, which
/// it sends once, the first start a newer version took over.
pub fn updated_row(words: &str, theme: &Theme) -> ratatui::text::Line<'static> {
    use ratatui::text::{Line, Span};

    use crate::base::theme::Role;

    let words = crate::base::text::sanitize_line(words);
    let (said, command) = match words.rfind(" /") {
        Some(at) => words.split_at(at + 1),
        None => (words.as_ref(), ""),
    };
    Line::from(vec![
        Span::styled(
            format!("{} ", theme.glyphs.bullet),
            theme.style(Role::Success),
        ),
        Span::styled(said.to_owned(), theme.style(Role::Text)),
        Span::styled(command.to_owned(), theme.style(Role::Code)),
    ])
}

#[cfg(test)]
mod tests;
