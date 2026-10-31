//! What copying a selection needs to know that the screen's cells don't show: which cells only
//! decorate the text, such as a gutter glyph, a border, or a rule, and which rows go on from
//! the row above, where a long line wrapped. Renderers mark both on the styles they draw with,
//! as modifier bits ratatui leaves unused, so the marks travel with every span and every cell
//! of a frame. The terminal writer leaves them out, so they never reach the terminal.

use ratatui::buffer::Cell;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

/// The cell only decorates the text: a gutter glyph, a border, a rule.
const DECORATION: Modifier = Modifier::from_bits_retain(1 << 15);
/// The row goes on from the row above, which wrapped at a space.
const WRAPPED: Modifier = Modifier::from_bits_retain(1 << 14);
/// The row goes on from the row above, which broke inside a word or a line of code.
const SPLIT: Modifier = Modifier::from_bits_retain(1 << 13);

const MARKS: Modifier = DECORATION.union(WRAPPED).union(SPLIT);

/// How a row joins the row above it in copied text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    /// A row of its own.
    NewLine,
    /// The rest of a line that wrapped at a space, which the wrap took out.
    Space,
    /// The rest of a word or a line of code that didn't fit.
    Direct,
}

/// `style`, marked as decoration.
pub fn decoration(style: Style) -> Style {
    style.add_modifier(DECORATION)
}

/// A span drawn only to decorate, such as `●` or `⎿`.
pub fn decorative(
    content: impl Into<std::borrow::Cow<'static, str>>,
    style: Style,
) -> Span<'static> {
    Span::styled(content, decoration(style))
}

/// Marks `row`, the spans that follow a wrap, as going on from the row above. The mark goes
/// on the first span that draws something, which is where the wrapped text resumes.
pub fn continues(row: &mut [Span<'_>], join: Join) {
    let mark = match join {
        Join::NewLine => return,
        Join::Space => WRAPPED,
        Join::Direct => SPLIT,
    };
    if let Some(span) = row.iter_mut().find(|span| !span.content.is_empty()) {
        span.style = span.style.add_modifier(mark);
    }
}

pub fn is_decoration(cell: &Cell) -> bool {
    cell.modifier.contains(DECORATION)
}

/// The join a cell starts, if the wrapped text resumes on it.
pub fn join_at(cell: &Cell) -> Option<Join> {
    if cell.modifier.contains(WRAPPED) {
        Some(Join::Space)
    } else if cell.modifier.contains(SPLIT) {
        Some(Join::Direct)
    } else {
        None
    }
}

/// The modifiers a terminal draws, without the marks.
pub fn drawn(modifier: Modifier) -> Modifier {
    modifier.difference(MARKS)
}

/// `style` without its marks, for comparing what two renderings look like.
pub fn unmarked(style: Style) -> Style {
    Style {
        add_modifier: drawn(style.add_modifier),
        sub_modifier: drawn(style.sub_modifier),
        ..style
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    #[test]
    fn marks_ride_on_cells_and_never_change_what_is_drawn() {
        let style = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
        let mut cell = Cell::default();
        cell.set_style(decoration(style));
        assert!(is_decoration(&cell));
        assert_eq!(drawn(cell.modifier), Modifier::BOLD);
        assert_eq!(unmarked(decoration(style)), style);
        assert_eq!(join_at(&cell), None);

        let mut row = vec![Span::raw(""), Span::styled("rest", style)];
        continues(&mut row, Join::Space);
        assert_eq!(
            row[0].style,
            Style::default(),
            "an empty span draws nothing to mark"
        );
        let mut cell = Cell::default();
        cell.set_style(row[1].style);
        assert_eq!(join_at(&cell), Some(Join::Space));
        assert!(!is_decoration(&cell));
    }
}
