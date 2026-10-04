//! Claude Code's You should know, directly above the input as Claude Code draws it:
//! a two-cell star column with `✦` in the suggestion color, the tag dim, the line wrapped
//! to the width, and the buttons on the row below, indented 2, three spaces apart, numbered in
//! order with Dismiss as `0`. Argonauts draws whatever tree comes, so a button it has never
//! seen works the same.

use argonauts_core::screen::{Drawn, DrawnLook, DrawnRow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::render::RenderCtx;
use crate::text::{sanitize_line, str_width, wrap_spans};
use crate::theme::{Role, Theme};

/// Claude Code caps a heads-up's line at this many characters.
const MAX_CHARS: usize = 240;
const STAR: &str = "✦";
const BUTTON_GAP: &str = "   ";
/// The mode line while the heads-up has the keys.
pub const HEADS_UP_KEYS: &str = "←/→ to choose · enter to press · esc for the input";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeadsUp {
    pub drawn: Drawn,
    /// Ctrl+X Ctrl+A hid it.
    pub hidden: bool,
    /// The button in reverse video while it has the keys.
    pub selected: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadsUpEvent {
    /// Press the button at this index of `drawn.buttons`.
    Press(usize),
    /// Esc: the keys go back to the input.
    Leave,
    Moved,
    Unhandled,
}

impl HeadsUp {
    pub fn new(drawn: Drawn) -> Self {
        Self {
            drawn,
            ..Self::default()
        }
    }

    /// A new drawing of the same heads-up keeps where your keys were.
    pub fn redraw(&mut self, drawn: Drawn) {
        let buttons = drawn.buttons.len();
        self.drawn = drawn;
        self.selected = self
            .selected
            .filter(|_| buttons > 0)
            .map(|at| at.min(buttons - 1));
    }

    pub fn shows(&self) -> bool {
        !self.hidden && !self.drawn.is_empty()
    }

    pub fn has_focus(&self) -> bool {
        self.selected.is_some() && self.shows()
    }

    /// Ctrl+X Tab. False when there's nothing to give the keys to.
    pub fn focus(&mut self) -> bool {
        if !self.shows() || self.drawn.buttons.is_empty() {
            return false;
        }
        self.selected = Some(0);
        true
    }

    pub fn blur(&mut self) {
        self.selected = None;
    }

    /// Ctrl+X Ctrl+A.
    pub fn toggle(&mut self) {
        self.hidden = !self.hidden;
        if self.hidden {
            self.selected = None;
        }
    }

    /// The key a button is pressed by: `1` up in order, and `0` for Dismiss.
    pub fn number(&self, at: usize) -> char {
        if is_dismiss(&self.drawn.buttons[at].label) {
            return '0';
        }
        let before = self.drawn.buttons[..at]
            .iter()
            .filter(|button| !is_dismiss(&button.label))
            .count();
        char::from_digit(u32::try_from(before + 1).unwrap_or(9).min(9), 10).unwrap_or('9')
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> HeadsUpEvent {
        let Some(selected) = self.selected else {
            return HeadsUpEvent::Unhandled;
        };
        let count = self.drawn.buttons.len();
        if count == 0 {
            return HeadsUpEvent::Leave;
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                self.selected = None;
                HeadsUpEvent::Leave
            }
            KeyCode::Left => {
                self.selected = Some(selected.checked_sub(1).unwrap_or(count - 1));
                HeadsUpEvent::Moved
            }
            KeyCode::Right | KeyCode::Tab => {
                self.selected = Some((selected + 1) % count);
                HeadsUpEvent::Moved
            }
            KeyCode::BackTab => {
                self.selected = Some(selected.checked_sub(1).unwrap_or(count - 1));
                HeadsUpEvent::Moved
            }
            KeyCode::Enter | KeyCode::Char(' ') if plain => HeadsUpEvent::Press(selected),
            KeyCode::Char(digit) if plain && digit.is_ascii_digit() => {
                match (0..count).find(|&at| self.number(at) == digit) {
                    Some(at) => HeadsUpEvent::Press(at),
                    None => HeadsUpEvent::Moved,
                }
            }
            // The quit keys and two-key bindings still act.
            _ if !plain => HeadsUpEvent::Unhandled,
            // Typing never goes to it, and never to the input while it has the keys.
            _ => HeadsUpEvent::Moved,
        }
    }

    /// Its rows, then a blank row before the input. Empty when it doesn't show.
    pub fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        if !self.shows() {
            return Vec::new();
        }
        let width = usize::from(ctx.width).max(8);
        let mut lines: Vec<Line<'static>> = self
            .drawn
            .rows
            .iter()
            .flat_map(|row| row_lines(row, width, ctx.theme))
            .collect();
        lines.extend(self.button_lines(width, ctx.theme));
        lines.push(Line::default());
        lines
    }

    /// Indented 2 and three spaces apart, wrapping as words do.
    fn button_lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut row: Vec<Span<'static>> = vec![Span::raw("  ")];
        let mut used = 2;
        for (at, button) in self.drawn.buttons.iter().enumerate() {
            let label = sanitize_line(&button.label).into_owned();
            let number = self.number(at).to_string();
            let cells = str_width(&number) + 1 + str_width(&label);
            let gap = if used > 2 { str_width(BUTTON_GAP) } else { 0 };
            if used > 2 && used + gap + cells > width {
                lines.push(Line::from(std::mem::replace(
                    &mut row,
                    vec![Span::raw("  ")],
                )));
                used = 2;
            } else if gap > 0 {
                row.push(Span::raw(BUTTON_GAP));
                used += gap;
            }
            let pressed_by = theme.style(Role::Dim);
            let mut text = theme.style(Role::Text);
            if self.selected == Some(at) {
                text = text.add_modifier(Modifier::REVERSED);
                row.push(Span::styled(format!("{number} {label}"), text));
            } else {
                row.push(Span::styled(number, pressed_by));
                row.push(Span::styled(format!(" {label}"), text));
            }
            used += cells;
        }
        if used > 2 {
            lines.push(Line::from(row));
        }
        lines
    }
}

fn is_dismiss(label: &str) -> bool {
    label.trim().eq_ignore_ascii_case("dismiss")
}

/// A row of the tree: a star column when it starts with `✦`, then its text wrapped under it.
fn row_lines(row: &DrawnRow, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut budget = MAX_CHARS;
    for span in &row.spans {
        if budget == 0 {
            break;
        }
        let text: String = sanitize_line(&span.text).chars().take(budget).collect();
        budget = budget.saturating_sub(text.chars().count());
        spans.push(Span::styled(text, style(span.look, theme)));
    }
    let starred = spans
        .first()
        .is_some_and(|span| span.content.trim_start().starts_with(STAR));
    if !starred {
        return wrap_spans(&spans, width, &[], &[]);
    }
    // The star stands in its own two-cell column, and the text wraps beside it.
    let first = spans.remove(0);
    let after = first
        .content
        .trim_start()
        .trim_start_matches(STAR)
        .to_owned();
    if !after.is_empty() {
        spans.insert(0, Span::styled(after, first.style));
    }
    if let Some(lead) = spans.first_mut() {
        let trimmed = lead.content.trim_start().to_owned();
        lead.content = trimmed.into();
    }
    let star = vec![Span::styled(STAR, theme.style(Role::Card)), Span::raw(" ")];
    wrap_spans(&spans, width, &star, &[Span::raw("  ")])
}

fn style(look: DrawnLook, theme: &Theme) -> Style {
    match look {
        DrawnLook::Plain => theme.style(Role::Text),
        DrawnLook::Dim => theme.style(Role::Dim),
        DrawnLook::Bold => theme.style(Role::Text).add_modifier(Modifier::BOLD),
        DrawnLook::Accent => theme.style(Role::Card),
        DrawnLook::Warning => theme.style(Role::Warning),
        DrawnLook::Error => theme.style(Role::Error),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::screen::{DrawnButton, DrawnSpan};

    use super::*;

    fn heads_up(text: &str) -> HeadsUp {
        let span = |text: &str, look| DrawnSpan {
            text: text.to_owned(),
            look,
        };
        let button = |label: &str, handle| DrawnButton {
            label: label.to_owned(),
            plugin: "cc-plugin-you-should-know".to_owned(),
            handle,
        };
        HeadsUp::new(Drawn {
            rows: vec![DrawnRow {
                spans: vec![
                    span("✦", DrawnLook::Dim),
                    span(" Heads up · ", DrawnLook::Dim),
                    span(text, DrawnLook::Plain),
                ],
            }],
            buttons: vec![
                button("Learn more", 1),
                button("Knew this already", 2),
                button("Dismiss", 3),
            ],
            left_out: Vec::new(),
        })
    }

    #[test]
    fn buttons_are_numbered_with_dismiss_as_zero() {
        let up = heads_up("x");
        let numbers: Vec<char> = (0..3).map(|at| up.number(at)).collect();
        assert_eq!(numbers, ['1', '2', '0']);
    }

    #[test]
    fn keys_move_press_and_leave() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut up = heads_up("x");
        assert_eq!(up.handle_key(key(KeyCode::Enter)), HeadsUpEvent::Unhandled);
        assert!(up.focus());
        assert_eq!(up.handle_key(key(KeyCode::Right)), HeadsUpEvent::Moved);
        assert_eq!(up.handle_key(key(KeyCode::Enter)), HeadsUpEvent::Press(1));
        assert_eq!(
            up.handle_key(key(KeyCode::Char('0'))),
            HeadsUpEvent::Press(2)
        );
        assert_eq!(up.handle_key(key(KeyCode::Left)), HeadsUpEvent::Moved);
        assert_eq!(up.selected, Some(0));
        assert_eq!(up.handle_key(key(KeyCode::Char('q'))), HeadsUpEvent::Moved);
        assert_eq!(up.handle_key(key(KeyCode::Esc)), HeadsUpEvent::Leave);
        assert!(!up.has_focus());
        up.toggle();
        assert!(!up.shows() && !up.focus());
    }
}
