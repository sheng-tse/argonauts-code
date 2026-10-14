//! The settings pickers, in Claude Code's dialog frame under the input: an edge rule in the
//! question color, the title with what the choice does, numbered rows with `❯`, the current value
//! with `✓`, a description column, `↓` and `… +N more` when the list goes on, and key hints.
//! The scope question after a pick uses the same frame.
//!
//! The picker only moves, filters, and reports picks. What a pick means lives in
//! [`Flow`](crate::views::settings::flow::Flow), which the settings screens keep in it.

mod render;
/// Pasted text for a line that holds one row: line breaks become spaces, and other control
/// characters go.
pub fn one_line(text: &str) -> String {
    text.trim_end_matches(['\r', '\n'])
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::kit::State;
use crate::views::list::{self, FilterKey};
use crate::views::settings::flow::Flow;

/// Rows shown at once. The rest scroll, marked with `↓` and `… +N more`.
pub const VISIBLE_ROWS: usize = 8;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Picker {
    pub title: String,
    /// What the choice does, dim under the title.
    pub about: String,
    pub rows: Vec<PickerRow>,
    /// The pointer, as a position among the rows the filter keeps.
    pub selected: usize,
    /// The first of those rows on screen.
    pub offset: usize,
    /// Typing filters the rows, unless they pick by letter.
    pub filter: String,
    /// Vim keys move the rows, and typing filters only after `/`.
    pub vim: bool,
    /// Typing the filter after `/`, with vim keys.
    pub filtering: bool,
    /// A line being typed in place of the rows, such as a value with no fixed choices.
    pub input: Option<String>,
    /// Shown while there are no rows, such as while the controller answers.
    pub placeholder: String,
    /// Over the rows in the warning color, such as why a typed value was refused.
    pub warning: Option<String>,
    /// The Enter hint, `Enter to choose` when empty.
    pub enter_hint: String,
    /// The Esc hint, such as `Esc to go back`, when Esc doesn't close or cancel.
    pub esc_hint: String,
    pub flow: Flow,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PickerRow {
    pub label: String,
    pub description: String,
    /// The value in effect now, drawn in the success color with `✓`.
    pub current: bool,
    /// The Reset row, which drops the key from the layer that sets it.
    pub reset: bool,
    /// Picks the row at once, drawn at the right. Rows with letters don't filter.
    pub key: Option<char>,
    /// Shown over the rows while the pointer is on this row.
    pub warning: Option<String>,
    /// A dim line drawn above the row, such as "Older", starting a group of rows. It takes no
    /// number and the pointer passes over it.
    pub heading: Option<String>,
    /// A dim line drawn under the row, such as the aliases under a CLI's Default model.
    pub under: Option<String>,
    /// What state the row's thing is in, drawn as that state's glyph in its color before the
    /// description, whose first word names it: `✔ on`, `✗ not installed`.
    pub state: Option<State>,
}

impl PickerRow {
    pub fn new(label: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: description.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickerEvent {
    /// A key taken without a pick. The pointer, the filter, or the typed line may have changed.
    Moved,
    /// A row, by its index in `rows`.
    Picked(usize),
    /// The Reset row, by its index in `rows`.
    Reset(usize),
    /// Enter on the typed line.
    Entered(String),
    /// Esc: nothing changes.
    Closed,
    /// ←, or `h` with vim keys where it has somewhere to go: lowers a value on the arrows, else
    /// goes back a level.
    Left,
    /// →, or `l` with vim keys where the arrows set a value: raises that value, else opens the
    /// row under the pointer.
    Right,
    /// Enter where there are no rows to pick, such as on the effort slider.
    Confirmed,
    /// Tab on the effort slider, which flips Ultracode beside it.
    Toggled,
    Unhandled,
}

impl Picker {
    pub fn new(title: impl Into<String>, about: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            about: about.into(),
            ..Self::default()
        }
    }

    /// Indices into `rows` of the rows the filter keeps, in order.
    pub fn shown(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty()
                    || row.label.to_lowercase().contains(&needle)
                    || row.description.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The row under the pointer, by its index in `rows`.
    pub fn pointed(&self) -> Option<usize> {
        self.shown().get(self.selected).copied()
    }

    /// Puts the pointer on this row of `rows`, if the filter shows it.
    pub fn point_at(&mut self, index: usize) {
        if let Some(position) = self.shown().iter().position(|&shown| shown == index) {
            self.selected = position;
            self.scroll_to_pointer();
        }
    }

    /// New rows in place of the old, with the pointer on the first and no filter.
    pub fn set_rows(&mut self, rows: Vec<PickerRow>) {
        self.rows = rows;
        self.selected = 0;
        self.offset = 0;
        self.filter.clear();
        self.filtering = false;
        self.input = None;
        self.warning = None;
    }

    fn picks_by_letter(&self) -> bool {
        self.rows.iter().any(|row| row.key.is_some())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerEvent {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.input.is_some() {
            return self.input_key(key, plain, control);
        }
        if self.flow.slider.is_some() {
            return self.slider_key(key, plain);
        }
        let key = self.vim_sideways(key, plain);
        let key = match self.vim_key(key, plain) {
            Ok(key) => key,
            Err(event) => return event,
        };
        match key.code {
            KeyCode::Esc => PickerEvent::Closed,
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Char('p') if control => self.move_by(-1),
            KeyCode::Char('n') if control => self.move_by(1),
            KeyCode::PageUp => self.move_by(-(VISIBLE_ROWS as isize)),
            KeyCode::PageDown => self.move_by(VISIBLE_ROWS as isize),
            KeyCode::Home => self.move_by(isize::MIN / 2),
            KeyCode::End => self.move_by(isize::MAX / 2),
            KeyCode::Left => PickerEvent::Left,
            KeyCode::Right => PickerEvent::Right,
            KeyCode::Delete => PickerEvent::Moved,
            KeyCode::Enter => self.pick(self.selected),
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
                self.offset = 0;
                PickerEvent::Moved
            }
            // Numbers pick until a filter starts, since model names hold digits.
            KeyCode::Char(digit @ '1'..='9')
                if plain && self.filter.is_empty() && !self.filtering =>
            {
                self.pick(digit as usize - '1' as usize)
            }
            KeyCode::Char(letter) if plain && self.picks_by_letter() => {
                let letter = letter.to_ascii_lowercase();
                let position = self
                    .shown()
                    .iter()
                    .position(|&index| self.rows[index].key == Some(letter));
                match position {
                    Some(position) => self.pick(position),
                    None => PickerEvent::Moved,
                }
            }
            KeyCode::Char(ch) if plain => {
                self.filter.push(ch);
                self.selected = 0;
                self.offset = 0;
                PickerEvent::Moved
            }
            _ => PickerEvent::Unhandled,
        }
    }

    /// With vim keys, `h` and `l` stand for ← and → where the arrows set a value, and `h` for
    /// ← where there's a level to go back to. Elsewhere `vim_key` makes them Esc and Enter.
    fn vim_sideways(&self, key: KeyEvent, plain: bool) -> KeyEvent {
        if !self.vim || self.filtering || !plain {
            return key;
        }
        let adjusts = self.flow.arrows_adjust();
        let code = match key.code {
            KeyCode::Char('h') if adjusts || self.flow.can_go_back() => KeyCode::Left,
            KeyCode::Char('l') if adjusts => KeyCode::Right,
            _ => return key,
        };
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The effort slider takes ←/→ (`h`/`l` with vim keys), Tab where it offers Ultracode,
    /// Enter, and Esc, and keeps every other plain key from the composer under it.
    fn slider_key(&mut self, key: KeyEvent, plain: bool) -> PickerEvent {
        let vim = self.vim && plain;
        let toggles = self
            .flow
            .slider
            .as_ref()
            .is_some_and(|slider| slider.ultracode.is_some());
        match key.code {
            KeyCode::Tab if plain && toggles => PickerEvent::Toggled,
            KeyCode::Left => PickerEvent::Left,
            KeyCode::Char('h') if vim => PickerEvent::Left,
            KeyCode::Right => PickerEvent::Right,
            KeyCode::Char('l') if vim => PickerEvent::Right,
            KeyCode::Enter => PickerEvent::Confirmed,
            KeyCode::Esc => PickerEvent::Closed,
            _ if plain => PickerEvent::Moved,
            _ => PickerEvent::Unhandled,
        }
    }

    /// With vim keys, `/` starts the filter and Esc leaves it. Until then letters never
    /// filter: `hjkl`, `g`, and `G` move, choose, and close, and rows still pick by their number
    /// or letter. `Err` is a key taken here.
    fn vim_key(&mut self, key: KeyEvent, plain: bool) -> Result<KeyEvent, PickerEvent> {
        if !self.vim {
            return Ok(key);
        }
        let picks =
            plain && (matches!(key.code, KeyCode::Char('1'..='9')) || self.picks_by_letter());
        match list::filter_key(key, self.filtering) {
            FilterKey::Start if self.picks_by_letter() => Ok(key),
            FilterKey::Start => {
                self.filtering = true;
                Err(PickerEvent::Moved)
            }
            FilterKey::Leave => {
                self.filtering = false;
                self.filter.clear();
                self.selected = 0;
                self.offset = 0;
                Err(PickerEvent::Moved)
            }
            FilterKey::Nothing if picks => Ok(key),
            FilterKey::Nothing => Err(PickerEvent::Moved),
            FilterKey::Key(key) => Ok(key),
        }
    }

    fn input_key(&mut self, key: KeyEvent, plain: bool, control: bool) -> PickerEvent {
        let Some(text) = self.input.as_mut() else {
            return PickerEvent::Unhandled;
        };
        match key.code {
            KeyCode::Esc => PickerEvent::Closed,
            KeyCode::Enter => PickerEvent::Entered(text.clone()),
            KeyCode::Backspace => {
                text.pop();
                PickerEvent::Moved
            }
            KeyCode::Char('u') if control => {
                text.clear();
                PickerEvent::Moved
            }
            KeyCode::Char(ch) if plain => {
                text.push(ch);
                PickerEvent::Moved
            }
            // The line is edited at its end only, so these stay here and do nothing.
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End | KeyCode::Delete => {
                PickerEvent::Moved
            }
            _ => PickerEvent::Unhandled,
        }
    }

    /// A paste goes into the typed line, or into the filter when typing filters the rows.
    /// `Unhandled` when the rows pick by letter, or vim keys wait for `/`, and a paste has
    /// nowhere to go.
    pub fn handle_paste(&mut self, text: &str) -> PickerEvent {
        let pasted = one_line(text);
        if let Some(input) = self.input.as_mut() {
            input.push_str(&pasted);
        } else if self.picks_by_letter() || (self.vim && !self.filtering) {
            return PickerEvent::Unhandled;
        } else {
            self.filter.push_str(&pasted);
            self.selected = 0;
            self.offset = 0;
        }
        PickerEvent::Moved
    }

    /// The row at this position among the shown rows, which also gets the pointer.
    fn pick(&mut self, position: usize) -> PickerEvent {
        let Some(&index) = self.shown().get(position) else {
            return PickerEvent::Moved;
        };
        self.selected = position;
        self.scroll_to_pointer();
        if self.rows[index].reset {
            PickerEvent::Reset(index)
        } else {
            PickerEvent::Picked(index)
        }
    }

    fn move_by(&mut self, delta: isize) -> PickerEvent {
        let count = self.shown().len();
        if count == 0 {
            return PickerEvent::Moved;
        }
        let last = count as isize - 1;
        self.selected = (self.selected as isize)
            .saturating_add(delta)
            .clamp(0, last) as usize;
        self.scroll_to_pointer();
        PickerEvent::Moved
    }

    fn scroll_to_pointer(&mut self) {
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + VISIBLE_ROWS {
            self.offset = self.selected + 1 - VISIBLE_ROWS;
        }
    }
}
