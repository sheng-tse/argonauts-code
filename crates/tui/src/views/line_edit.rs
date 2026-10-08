//! One line of text you change in place, with a cursor you move: what the Goal tab's `e` opens
//! on a goal's lines. A line you only add to, such as a message to an agent, needs no cursor.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

use crate::theme::{Role, Theme};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineEdit {
    text: Vec<char>,
    /// Before which character the cursor sits; `text.len()` is the end.
    cursor: usize,
}

impl LineEdit {
    /// `text` to change, the cursor at its end.
    pub fn new(text: &str) -> Self {
        let text: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
        Self {
            cursor: text.len(),
            text,
        }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// Takes a key that edits or moves. `false` for any other, Enter and Esc among them.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            // Ctrl+U clears back to the start, as a shell's line does.
            KeyCode::Char('u') if ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => self.text.truncate(self.cursor),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            KeyCode::Backspace | KeyCode::Delete => {}
            KeyCode::Char(c) if !ctrl && !alt => {
                self.text.insert(self.cursor, c);
                self.cursor += 1;
            }
            _ => return false,
        }
        true
    }

    /// The text around the cursor in `width` cells, the cursor drawn where it sits, and `…`
    /// at an end the text goes on past.
    pub fn spans(&self, width: usize, theme: &Theme) -> Vec<Span<'static>> {
        let cells = |c: &char| c.width().unwrap_or(0);
        // One cell is the cursor's, and one at each end may be an ellipsis.
        let room = width.saturating_sub(3).max(1);
        let mut start = self.cursor;
        let mut end = self.cursor;
        let mut used = 0;
        // Grow toward the start first, so what you just typed stays in view, then the end.
        while start > 0 && used + cells(&self.text[start - 1]) <= room {
            start -= 1;
            used += cells(&self.text[start]);
        }
        while end < self.text.len() && used + cells(&self.text[end]) <= room {
            used += cells(&self.text[end]);
            end += 1;
        }
        let part = |range: std::ops::Range<usize>| -> String { self.text[range].iter().collect() };
        let text = theme.style(Role::Text);
        let dim = theme.style(Role::Dim);
        let mut spans = Vec::new();
        if start > 0 {
            spans.push(Span::styled(theme.glyphs.ellipsis, dim));
        }
        spans.push(Span::styled(part(start..self.cursor), text));
        spans.push(Span::styled("▌", dim));
        spans.push(Span::styled(part(self.cursor..end), text));
        if end < self.text.len() {
            spans.push(Span::styled(theme.glyphs.ellipsis, dim));
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::spans_width;
    use crate::theme::Glyphs;

    fn press(line: &mut LineEdit, code: KeyCode) -> bool {
        line.key(KeyEvent::from(code))
    }

    fn drawn(line: &LineEdit, width: usize) -> String {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let spans = line.spans(width, &theme);
        assert!(spans_width(&spans) <= width, "{spans:?}");
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn the_cursor_moves_and_text_goes_in_and_out_where_it_sits() {
        let mut line = LineEdit::new("the tests pass; the CSV has a header");
        assert_eq!(drawn(&line, 60), "the tests pass; the CSV has a header▌");
        press(&mut line, KeyCode::Home);
        for _ in 0..4 {
            press(&mut line, KeyCode::Right);
        }
        for c in "export ".chars() {
            press(&mut line, KeyCode::Char(c));
        }
        assert_eq!(line.text(), "the export tests pass; the CSV has a header");
        press(&mut line, KeyCode::Backspace);
        press(&mut line, KeyCode::Delete);
        assert_eq!(line.text(), "the exportests pass; the CSV has a header");
        assert_eq!(
            drawn(&line, 60),
            "the export▌ests pass; the CSV has a header"
        );

        line.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(line.text(), "the export");
        line.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(line.text(), "");
        assert!(press(&mut line, KeyCode::Backspace), "nothing left to take");
    }

    /// Enter and Esc are the view's to act on, and a control key types nothing.
    #[test]
    fn keys_that_do_not_edit_are_left_to_the_view() {
        let mut line = LineEdit::new("a line");
        assert!(!press(&mut line, KeyCode::Enter));
        assert!(!press(&mut line, KeyCode::Esc));
        assert!(!line.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)));
        assert_eq!(line.text(), "a line");
    }

    /// A line longer than its room shows what is around the cursor, with `…` where it goes
    /// on, and never draws wider than it was given.
    #[test]
    fn a_long_line_keeps_the_cursor_in_view() {
        let mut line =
            LineEdit::new("tasks overdue lists tasks due before today; every test passes");
        assert_eq!(drawn(&line, 24), "…ay; every test passes▌");
        press(&mut line, KeyCode::Home);
        assert_eq!(drawn(&line, 24), "▌tasks overdue lists t…");
        for _ in 0..30 {
            press(&mut line, KeyCode::Right);
        }
        let middle = drawn(&line, 24);
        assert!(middle.starts_with('…') && middle.contains('▌'), "{middle}");
        // Two cells a character: what fits whole is shown, never half of one.
        assert_eq!(drawn(&LineEdit::new("日本語のゴール"), 8), "…ール▌");
    }
}
