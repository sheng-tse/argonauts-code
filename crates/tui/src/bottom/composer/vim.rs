//! Vim keys for the composer. INSERT types as usual, NORMAL takes letters as commands, and
//! VISUAL selects. The mode is public so the bottom line can always say which one is on: a
//! NORMAL mode you can't see sends keys somewhere you didn't mean.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Composer, ComposerEvent, VimMode, layout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Operator {
    Delete,
    Change,
    Yank,
}

/// Keys waiting for the rest of a command, such as the `w` of `diw`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Pending {
    operator: Option<Operator>,
    /// `i` or `a` came, and a text object's letter comes next. True for `a`.
    object: Option<bool>,
    /// The first `g` of `gg`.
    g: bool,
}

impl Pending {
    fn is_empty(self) -> bool {
        self == Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Visual {
    anchor: usize,
    /// VISUAL LINE, from `V`.
    lines: bool,
}

impl Composer {
    /// Vim keys on start in INSERT, as Claude Code does.
    pub fn set_vim(&mut self, on: bool) {
        self.vim = match (on, self.vim) {
            (false, _) => None,
            (true, Some(mode)) => Some(mode),
            (true, None) => Some(VimMode::Insert),
        };
        self.vim_pending = Pending::default();
        self.visual = None;
        if self.vim == Some(VimMode::Visual) {
            self.vim = Some(VimMode::Normal);
        }
    }

    /// `None` without vim keys.
    pub fn vim_mode(&self) -> Option<VimMode> {
        self.vim
    }

    /// Takes a mode set from outside, such as vim keys turned on in the settings. VISUAL needs
    /// a selection, so only a key starts it; a composer that already left it stays out.
    pub fn adopt_vim_mode(&mut self, mode: Option<VimMode>) {
        if mode == self.vim {
            return;
        }
        match mode {
            None => self.set_vim(false),
            Some(VimMode::Visual) if self.vim.is_some() => {}
            Some(mode) => {
                self.vim = Some(match mode {
                    VimMode::Insert => VimMode::Insert,
                    VimMode::Normal | VimMode::Visual => VimMode::Normal,
                });
                self.vim_pending = Pending::default();
                self.visual = None;
            }
        }
    }

    /// The selected byte range while VISUAL is on.
    pub fn visual_range(&self) -> Option<Range<usize>> {
        let visual = self.visual?;
        let at = self.draft.cursor;
        let (low, high) = (visual.anchor.min(at), visual.anchor.max(at));
        Some(if visual.lines {
            self.draft.line_start(low)..self.draft.line_end(high)
        } else {
            low..self.draft.next_stop(high)
        })
    }

    /// Leaving INSERT puts the cursor on the character before it, as vim does.
    pub fn vim_left_insert(&mut self) {
        self.vim = Some(VimMode::Normal);
        self.vim_pending = Pending::default();
        self.visual = None;
        let at = self.draft.cursor;
        if at > self.draft.line_start(at) {
            self.draft.cursor = self.draft.prev_stop(at);
        }
    }

    /// `j` and `k` in NORMAL, with no command half typed and nothing selected, as the arrows
    /// they stand for, so they do all ↓ and ↑ do: walk history, then reach the list under the
    /// input from a blank prompt, and come back, as Claude Code's do.
    pub fn vim_arrow(&self, key: &KeyEvent) -> Option<KeyCode> {
        if self.vim != Some(VimMode::Normal)
            || !self.vim_pending.is_empty()
            || self.visual.is_some()
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        match key.code {
            KeyCode::Char('j') => Some(KeyCode::Down),
            KeyCode::Char('k') => Some(KeyCode::Up),
            _ => None,
        }
    }

    /// A letter in NORMAL or VISUAL. Returns what the key did, and whether it enters INSERT.
    pub fn vim_normal_key(&mut self, key: KeyEvent) -> (ComposerEvent, bool) {
        let c = match key.code {
            KeyCode::Char(c) => c,
            KeyCode::Backspace => 'h',
            _ => return (ComposerEvent::Unhandled, false),
        };
        if self.visual.is_none() {
            self.vim = Some(VimMode::Normal);
        }
        let (event, insert) = if self.visual.is_some() {
            self.visual_key(c)
        } else {
            self.normal_key(c)
        };
        if insert {
            self.vim = Some(VimMode::Insert);
            self.visual = None;
        }
        // Letters here are commands, so even one that does nothing was taken.
        let event = match event {
            ComposerEvent::Unhandled => ComposerEvent::Moved,
            event => event,
        };
        (event, insert)
    }

    /// The key as a vim command, or `None` when vim leaves it to the usual editing. Esc leaves
    /// INSERT or VISUAL, or drops a half-typed command such as `d`. Esc in NORMAL with nothing
    /// pending goes on, so it can interrupt.
    pub fn vim_key(&mut self, key: KeyEvent) -> Option<ComposerEvent> {
        let mode = self.vim?;
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let normal = mode == VimMode::Normal && self.vim_pending.is_empty();
        // `u` undoes and Ctrl+R redoes, as in vim.
        if normal && plain && key.code == KeyCode::Char('u') {
            return Some(self.undo());
        }
        if normal && key.code == KeyCode::Char('r') && key.modifiers == KeyModifiers::CONTROL {
            return Some(self.redo());
        }
        let before = self.saved();
        let event = self.vim_command(key, mode, plain)?;
        match (mode, self.vim) {
            (VimMode::Insert, Some(VimMode::Normal)) => self.vim_insert_ended(),
            (VimMode::Normal | VimMode::Visual, Some(VimMode::Insert)) => {
                self.vim_insert_began(before)
            }
            (VimMode::Normal | VimMode::Visual, _) if !super::same_text(&before, &self.saved()) => {
                self.vim_changed_from(before)
            }
            _ => {}
        }
        Some(event)
    }

    fn vim_command(&mut self, key: KeyEvent, mode: VimMode, plain: bool) -> Option<ComposerEvent> {
        let esc = key.code == KeyCode::Esc && key.modifiers.is_empty();
        match mode {
            VimMode::Insert if esc => {
                self.vim_left_insert();
                Some(ComposerEvent::Moved)
            }
            VimMode::Normal | VimMode::Visual
                if plain && matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace) =>
            {
                Some(self.vim_normal_key(key).0)
            }
            VimMode::Visual if esc => {
                self.leave_visual();
                Some(ComposerEvent::Moved)
            }
            VimMode::Normal if esc && !self.vim_pending.is_empty() => {
                self.vim_pending = Pending::default();
                Some(ComposerEvent::Moved)
            }
            _ => None,
        }
    }

    fn normal_key(&mut self, c: char) -> (ComposerEvent, bool) {
        let pending = std::mem::take(&mut self.vim_pending);
        if let Some(around) = pending.object {
            let Some(operator) = pending.operator else {
                return (ComposerEvent::Moved, false);
            };
            return match self.draft.text_object(self.draft.cursor, around, c) {
                Some(range) => self.vim_apply(operator, range, false),
                None => (ComposerEvent::Moved, false),
            };
        }
        if pending.g {
            return match (c, pending.operator) {
                ('g', None) => (self.move_to(0), false),
                ('g', Some(operator)) => {
                    let lines = 0..self.draft.line_end(self.draft.cursor);
                    let range = if operator == Operator::Delete {
                        self.whole_lines(lines)
                    } else {
                        lines
                    };
                    self.vim_apply(operator, range, true)
                }
                _ => (ComposerEvent::Moved, false),
            };
        }
        if let Some(operator) = pending.operator {
            return self.operator_key(operator, c);
        }
        let at = self.draft.cursor;
        let draft = &self.draft;
        let event = match c {
            'i' => return (ComposerEvent::Moved, true),
            'a' => {
                let end = draft.line_end(at);
                self.draft.cursor = if at < end { draft.next_stop(at) } else { end };
                return (ComposerEvent::Moved, true);
            }
            'I' => return (self.move_to(draft.first_non_blank(at)), true),
            'A' => return (self.move_to(draft.line_end(at)), true),
            'o' => {
                self.draft.cursor = draft.line_end(at);
                return (self.insert("\n"), true);
            }
            'O' => {
                self.draft.cursor = draft.line_start(at);
                self.insert("\n");
                self.draft.cursor = self.draft.prev_stop(self.draft.cursor);
                return (ComposerEvent::Edited, true);
            }
            'G' => {
                let last = draft.line_start(draft.text.len());
                self.move_to(last)
            }
            'x' if at < draft.line_end(at) => self.kill_forward(draft.next_stop(at)),
            'X' if at > draft.line_start(at) => self.kill_back(draft.prev_stop(at)),
            's' => {
                if at < draft.line_end(at) {
                    self.kill_forward(draft.next_stop(at));
                }
                return (ComposerEvent::Edited, true);
            }
            'D' => self.kill_forward(draft.line_end(at)),
            'C' => return (self.kill_forward(draft.line_end(at)), true),
            'S' => return (self.vim_line(Operator::Change).0, true),
            'd' | 'c' | 'y' => {
                self.vim_pending.operator = Some(match c {
                    'd' => Operator::Delete,
                    'c' => Operator::Change,
                    _ => Operator::Yank,
                });
                ComposerEvent::Moved
            }
            'g' => {
                self.vim_pending.g = true;
                ComposerEvent::Moved
            }
            '/' => ComposerEvent::SearchHistory,
            'p' => self.put(true),
            'P' => self.put(false),
            '~' => self.toggle_case(),
            'v' | 'V' => {
                self.visual = Some(Visual {
                    anchor: at,
                    lines: c == 'V',
                });
                self.vim = Some(VimMode::Visual);
                ComposerEvent::Moved
            }
            _ => match self.vim_motion(c, None) {
                Some((target, _)) => self.move_to(target),
                None => ComposerEvent::Moved,
            },
        };
        (event, false)
    }

    fn operator_key(&mut self, operator: Operator, c: char) -> (ComposerEvent, bool) {
        let doubled = matches!(
            (operator, c),
            (Operator::Delete, 'd') | (Operator::Change, 'c') | (Operator::Yank, 'y')
        );
        if doubled {
            return self.vim_line(operator);
        }
        match c {
            'i' | 'a' => {
                self.vim_pending = Pending {
                    operator: Some(operator),
                    object: Some(c == 'a'),
                    g: false,
                };
                (ComposerEvent::Moved, false)
            }
            'g' => {
                self.vim_pending = Pending {
                    operator: Some(operator),
                    object: None,
                    g: true,
                };
                (ComposerEvent::Moved, false)
            }
            _ => match self.vim_motion(c, Some(operator)) {
                Some((target, inclusive)) => {
                    let at = self.draft.cursor;
                    let (start, mut end) = (at.min(target), at.max(target));
                    if inclusive {
                        end = self.draft.next_stop(end);
                    }
                    self.vim_apply(operator, start..end, false)
                }
                // Anything else cancels the operator, as in vim.
                None => (ComposerEvent::Moved, false),
            },
        }
    }

    /// Where a motion key goes, and whether the character there is part of an operator's
    /// range.
    fn vim_motion(&self, c: char, operator: Option<Operator>) -> Option<(usize, bool)> {
        let draft = &self.draft;
        let at = draft.cursor;
        let target = match c {
            'h' => {
                if at > draft.line_start(at) {
                    draft.prev_stop(at)
                } else {
                    at
                }
            }
            'l' | ' ' => {
                if at < draft.line_end(at) {
                    draft.next_stop(at)
                } else {
                    at
                }
            }
            // `cw` changes to the end of the word, and `dw` stops at the end of the line.
            'w' | 'W' if operator == Some(Operator::Change) && at < draft.line_end(at) => {
                return Some((draft.word_end(at, c == 'W').max(at), true));
            }
            'w' | 'W' => {
                let next = draft.next_word_start(at, c == 'W');
                if operator.is_some() && at < draft.line_end(at) {
                    next.min(draft.line_end(at))
                } else {
                    next
                }
            }
            'e' | 'E' => return Some((draft.word_end(at, c == 'E'), true)),
            'b' | 'B' => draft.prev_word_start(at, c == 'B'),
            '0' => draft.line_start(at),
            '^' => draft.first_non_blank(at),
            '$' => draft.line_end(at),
            _ => return None,
        };
        Some((target, false))
    }

    fn visual_key(&mut self, c: char) -> (ComposerEvent, bool) {
        let pending = std::mem::take(&mut self.vim_pending);
        let Some(visual) = self.visual else {
            return (ComposerEvent::Unhandled, false);
        };
        if let Some(around) = pending.object {
            if let Some(range) = self.draft.text_object(self.draft.cursor, around, c)
                && !range.is_empty()
            {
                self.visual = Some(Visual {
                    anchor: range.start,
                    lines: false,
                });
                self.draft.cursor = self.draft.prev_stop(range.end);
            }
            return (ComposerEvent::Moved, false);
        }
        if pending.g {
            if c == 'g' {
                self.draft.cursor = 0;
            }
            return (ComposerEvent::Moved, false);
        }
        let Some(range) = self.visual_range() else {
            return (ComposerEvent::Unhandled, false);
        };
        let lines = visual.lines;
        match c {
            'i' | 'a' => {
                self.vim_pending.object = Some(c == 'a');
                (ComposerEvent::Moved, false)
            }
            'g' => {
                self.vim_pending.g = true;
                (ComposerEvent::Moved, false)
            }
            'o' => {
                self.visual = Some(Visual {
                    anchor: self.draft.cursor,
                    lines,
                });
                self.draft.cursor = visual.anchor;
                (ComposerEvent::Moved, false)
            }
            'v' | 'V' if lines == (c == 'V') => {
                self.leave_visual();
                (ComposerEvent::Moved, false)
            }
            'v' | 'V' => {
                self.visual = Some(Visual {
                    anchor: visual.anchor,
                    lines: c == 'V',
                });
                (ComposerEvent::Moved, false)
            }
            'd' | 'x' | 'X' | 'D' => {
                self.leave_visual();
                let range = if lines {
                    self.whole_lines(range)
                } else {
                    range
                };
                self.vim_apply(Operator::Delete, range, lines)
            }
            'c' | 's' | 'S' | 'C' => {
                self.leave_visual();
                self.vim_apply(Operator::Change, range, lines)
            }
            'y' | 'Y' => {
                self.leave_visual();
                self.vim_apply(Operator::Yank, range, lines)
            }
            'p' | 'P' => {
                self.leave_visual();
                let (kill, linewise) = (self.kill.clone(), self.kill_linewise);
                if kill.is_empty() {
                    return (ComposerEvent::Moved, false);
                }
                self.draft.delete(range);
                self.draft.insert_clip(&kill);
                self.kill = kill;
                self.kill_linewise = linewise;
                (self.edited(), false)
            }
            '~' | 'u' | 'U' => {
                self.leave_visual();
                (self.change_case(range, c), false)
            }
            'j' => (self.vertical_step(true), false),
            'k' => (self.vertical_step(false), false),
            'G' => {
                self.draft.cursor = self.draft.line_start(self.draft.text.len());
                (ComposerEvent::Moved, false)
            }
            _ => {
                if let Some((target, _)) = self.vim_motion(c, None) {
                    self.draft.cursor = target;
                }
                (ComposerEvent::Moved, false)
            }
        }
    }

    /// `j` and `k` in VISUAL move between rows and never into history.
    fn vertical_step(&mut self, down: bool) -> ComposerEvent {
        let (rows, width) = self.rows();
        let (row, col) = layout::cursor_position(&self.draft.text, &rows, self.draft.cursor, width);
        let target = if down { row + 1 } else { row.wrapping_sub(1) };
        if target >= rows.len() {
            return ComposerEvent::Moved;
        }
        self.vertical(&rows, width, target, col)
    }

    fn leave_visual(&mut self) {
        self.visual = None;
        self.vim_pending = Pending::default();
        if self.vim.is_some() {
            self.vim = Some(VimMode::Normal);
        }
    }

    /// Deletes, changes, or yanks `range` into the kill buffer. Returns whether INSERT starts.
    fn vim_apply(
        &mut self,
        operator: Operator,
        range: Range<usize>,
        linewise: bool,
    ) -> (ComposerEvent, bool) {
        let insert = operator == Operator::Change;
        if range.is_empty() {
            self.draft.cursor = range.start.min(self.draft.text.len());
            return (ComposerEvent::Moved, insert);
        }
        match operator {
            Operator::Yank => {
                let range = self.draft.widen(range);
                self.kill = self.draft.copy(range.clone());
                self.kill_linewise = linewise;
                self.draft.cursor = range.start;
                (ComposerEvent::Moved, false)
            }
            Operator::Delete | Operator::Change => {
                let removed = self.draft.delete(range);
                self.kill = if linewise {
                    removed.without_line_break()
                } else {
                    removed
                };
                self.kill_linewise = linewise;
                (self.edited(), insert)
            }
        }
    }

    /// `dd`, `cc`, and `yy`. `dd` takes the line and one newline, so the lines around it close
    /// up, and `cc` keeps the empty line to type on.
    fn vim_line(&mut self, operator: Operator) -> (ComposerEvent, bool) {
        let at = self.draft.cursor;
        let line = self.draft.line_start(at)..self.draft.line_end(at);
        let text = self.draft.copy(line.clone());
        match operator {
            Operator::Yank => {
                self.kill = text;
                self.kill_linewise = true;
                (ComposerEvent::Moved, false)
            }
            Operator::Change => {
                if line.is_empty() {
                    return (ComposerEvent::Moved, true);
                }
                self.kill = self.draft.delete(line);
                self.kill_linewise = true;
                (self.edited(), true)
            }
            Operator::Delete => {
                let range = self.whole_lines(line);
                if range.is_empty() {
                    return (ComposerEvent::Moved, false);
                }
                self.draft.delete(range);
                self.draft.cursor = self.draft.line_start(self.draft.cursor);
                self.kill = text;
                self.kill_linewise = true;
                (self.edited(), false)
            }
        }
    }

    /// Lines with the newline after them, or before them on the last line.
    fn whole_lines(&self, lines: Range<usize>) -> Range<usize> {
        let len = self.draft.text.len();
        if lines.end < len {
            lines.start..lines.end + 1
        } else if lines.start > 0 {
            lines.start - 1..lines.end
        } else {
            lines
        }
    }

    /// `p` puts after the cursor and `P` before it; whole lines go on a line of their own.
    fn put(&mut self, after: bool) -> ComposerEvent {
        if self.kill.is_empty() {
            return ComposerEvent::Unhandled;
        }
        let kill = self.kill.clone();
        let at = self.draft.cursor;
        if self.kill_linewise && !self.is_empty() {
            let start = if after {
                self.draft.cursor = self.draft.line_end(at);
                self.draft.insert("\n");
                self.draft.cursor
            } else {
                self.draft.line_start(at)
            };
            self.draft.cursor = start;
            self.draft.insert_clip(&kill);
            if !after {
                self.draft.insert("\n");
            }
            self.draft.cursor = start;
            return self.edited();
        }
        if after && at < self.draft.line_end(at) {
            self.draft.cursor = self.draft.next_stop(at);
        }
        self.draft.insert_clip(&kill);
        self.edited()
    }

    fn toggle_case(&mut self) -> ComposerEvent {
        let at = self.draft.cursor;
        if at >= self.draft.line_end(at) {
            return ComposerEvent::Moved;
        }
        let event = self.change_case(at..self.draft.next_stop(at), '~');
        self.draft.cursor = self.draft.next_stop(at);
        event
    }

    /// `~` swaps case, `u` lowers it, and `U` raises it. Pasted blocks keep theirs.
    fn change_case(&mut self, range: Range<usize>, how: char) -> ComposerEvent {
        if self
            .draft
            .elements
            .iter()
            .any(|e| e.range.start < range.end && e.range.end > range.start)
        {
            return ComposerEvent::Moved;
        }
        let old = &self.draft.text[range.clone()];
        let new: String = old
            .chars()
            .flat_map(|c| -> Box<dyn Iterator<Item = char>> {
                match how {
                    'u' => Box::new(c.to_lowercase()),
                    'U' => Box::new(c.to_uppercase()),
                    _ if c.is_uppercase() => Box::new(c.to_lowercase()),
                    _ => Box::new(c.to_uppercase()),
                }
            })
            .collect();
        if new == old {
            self.draft.cursor = range.start;
            return ComposerEvent::Moved;
        }
        self.draft.cursor = range.start;
        self.draft.delete(range);
        self.draft.insert(&new);
        self.draft.cursor = self.draft.cursor.saturating_sub(new.len());
        self.edited()
    }
}
