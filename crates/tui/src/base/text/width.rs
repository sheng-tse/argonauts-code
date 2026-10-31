use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

/// Sets `ui.ambiguous_width`, like vim's `ambiwidth`. Anything but 2 means 1.
pub fn set_ambiguous_width(cells: u8) {
    setting::store(if cells == 2 { 2 } else { 1 });
}

/// Cells an East Asian ambiguous glyph takes now: 1 or 2.
pub fn ambiguous_width() -> u8 {
    setting::load()
}

/// One value for the whole process, because every layout on the screen has to agree on it.
#[cfg(not(test))]
mod setting {
    use std::sync::atomic::{AtomicU8, Ordering};

    static AMBIGUOUS_WIDTH: AtomicU8 = AtomicU8::new(1);

    pub fn store(cells: u8) {
        AMBIGUOUS_WIDTH.store(cells, Ordering::Relaxed);
    }

    pub fn load() -> u8 {
        AMBIGUOUS_WIDTH.load(Ordering::Relaxed)
    }
}

/// Unit tests run in parallel threads and nearly all of them measure text, so each keeps its
/// own value and a test that sets 2 can't move another's columns.
#[cfg(test)]
mod setting {
    use std::cell::Cell;

    thread_local! {
        static AMBIGUOUS_WIDTH: Cell<u8> = const { Cell::new(1) };
    }

    pub fn store(cells: u8) {
        AMBIGUOUS_WIDTH.with(|width| width.set(cells));
    }

    pub fn load() -> u8 {
        AMBIGUOUS_WIDTH.with(Cell::get)
    }
}

/// Columns `text` takes on screen, with ambiguous glyphs as wide as `ui.ambiguous_width` says.
pub fn str_width(text: &str) -> usize {
    width_with(text, ambiguous_width())
}

/// Columns `text` takes when each East Asian ambiguous glyph takes `ambiguous` cells.
pub fn width_with(text: &str, ambiguous: u8) -> usize {
    if ambiguous == 2 {
        text.width_cjk()
    } else {
        text.width()
    }
}

/// A glyph whose width depends on the terminal: one cell in most, two where ambiguous glyphs
/// are drawn wide.
pub fn is_ambiguous(text: &str) -> bool {
    text.width_cjk() != text.width()
}

pub fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| str_width(&span.content)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_width_two_counts_two_cells() {
        for glyph in ["●", "◇", "※", "…", "─", "○"] {
            assert!(is_ambiguous(glyph), "{glyph:?}");
            assert_eq!(width_with(glyph, 1), 1, "{glyph:?}");
            assert_eq!(width_with(glyph, 2), 2, "{glyph:?}");
        }
        assert_eq!(width_with("● done", 1), 6);
        assert_eq!(width_with("● done", 2), 7);
        // CJK is wide and ASCII narrow whatever the setting.
        for cells in [1, 2] {
            assert_eq!(width_with("修复", cells), 4);
            assert_eq!(width_with("abc", cells), 3);
        }
        for plain in ["abc", "修复", "❯", "⎿", "✔"] {
            assert!(!is_ambiguous(plain), "{plain:?}");
        }
    }

    #[test]
    fn the_default_is_one_cell() {
        assert_eq!(ambiguous_width(), 1);
        assert_eq!(str_width("●"), 1);
        assert_eq!(spans_width(&[Span::raw("● "), Span::raw("修复")]), 6);
    }

    #[test]
    fn the_setting_changes_every_measure() {
        set_ambiguous_width(2);
        assert_eq!(str_width("●"), 2);
        assert_eq!(spans_width(&[Span::raw("● "), Span::raw("修复")]), 7);
        set_ambiguous_width(7);
        assert_eq!(ambiguous_width(), 1);
    }
}
