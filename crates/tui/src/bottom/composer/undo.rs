//! The input's edit history. Without vim keys, Ctrl+_ undoes, as in Claude Code, and a step is
//! a run of one kind of edit: a word typed, a run of deletes, a paste. With vim keys, `u`
//! undoes and Ctrl+R redoes, as in vim: everything typed between entering INSERT and Esc is
//! one step, and so is each change made in NORMAL.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Composer, ComposerEvent, Saved};
use crate::bottom::VimMode;

/// Claude Code keeps fifty.
const STEPS: usize = 50;

#[derive(Debug, Clone, Default)]
pub(super) struct EditHistory {
    /// The drafts before each step, the latest last.
    undo: Vec<Saved>,
    /// The drafts undo left, the latest undone last.
    redo: Vec<Saved>,
    /// The kind of the step under way, which the next edit of the same kind joins.
    group: Option<EditKind>,
    /// The draft as INSERT began, which Esc makes one step of everything typed since.
    insert_start: Option<Saved>,
}

/// What an edit did, so a run of the same kind is one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditKind {
    Typing,
    Deleting,
    /// A paste, a new line, a yank: a step each.
    Other,
}

impl EditKind {
    pub(super) fn of(key: &KeyEvent) -> Self {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(_) if ctrl == alt => EditKind::Typing,
            KeyCode::Backspace | KeyCode::Delete => EditKind::Deleting,
            KeyCode::Char('w' | 'u' | 'k' | 'h' | 'd') if ctrl => EditKind::Deleting,
            _ => EditKind::Other,
        }
    }
}

/// Ctrl+_ as terminals send it: the legacy byte reads as Ctrl+7, and the keyboard protocol
/// gives Ctrl+_ or Ctrl+-, with or without Shift.
pub(super) fn is_undo_key(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('_' | '-' | '7'))
}

impl EditHistory {
    fn push(&mut self, before: Saved) {
        if self.undo.last() == Some(&before) {
            return;
        }
        self.undo.push(before);
        if self.undo.len() > STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
}

impl Composer {
    /// An edit without vim keys changed the draft from `before`. It joins the step under way
    /// when it is of the same kind, and a typed space ends a word's step.
    pub(super) fn edited_from(&mut self, before: Saved, kind: EditKind, ends_word: bool) {
        match self.vim {
            // Typed in INSERT: Esc makes it one step, from where INSERT began.
            Some(VimMode::Insert) => {
                self.edits.insert_start.get_or_insert(before);
                return;
            }
            // A paste in NORMAL or VISUAL, a dropped picture's path among them, began no
            // INSERT, so it is a step of its own.
            Some(_) => {
                self.edits.push(before);
                self.edits.group = None;
                return;
            }
            None => {}
        }
        let history = &mut self.edits;
        if kind == EditKind::Other || history.group != Some(kind) {
            history.push(before);
        }
        history.group = (kind != EditKind::Other && !ends_word).then_some(kind);
    }

    /// The cursor moved without an edit, so the next edit starts a step of its own.
    pub(super) fn edit_step_ends(&mut self) {
        self.edits.group = None;
    }

    /// A change made in vim's NORMAL or VISUAL, one step each.
    pub(super) fn vim_changed_from(&mut self, before: Saved) {
        self.edits.push(before);
    }

    /// A command that enters INSERT, such as `cw`: its change and what is typed after it are
    /// one step, which Esc closes.
    pub(super) fn vim_insert_began(&mut self, before: Saved) {
        self.edits.insert_start = Some(before);
    }

    /// Esc left INSERT: what was typed there becomes one step.
    pub(super) fn vim_insert_ended(&mut self) {
        if let Some(start) = self.edits.insert_start.take()
            && start != self.saved()
        {
            self.edits.push(start);
        }
    }

    /// Back one step. The draft as it was goes on the redo list.
    pub(super) fn undo(&mut self) -> ComposerEvent {
        // What INSERT typed and Esc hasn't closed yet is undone first, as a whole.
        self.vim_insert_ended();
        let Some(before) = self.edits.undo.pop() else {
            return ComposerEvent::Moved;
        };
        let now = self.saved();
        self.edits.redo.push(now);
        self.edits.group = None;
        self.put_back(before);
        self.edited()
    }

    /// Forward the step undo went back.
    pub(super) fn redo(&mut self) -> ComposerEvent {
        let Some(after) = self.edits.redo.pop() else {
            return ComposerEvent::Moved;
        };
        let now = self.saved();
        self.edits.undo.push(now);
        self.edits.group = None;
        self.put_back(after);
        self.edited()
    }

    /// A new draft, as after sending: the old one's steps go with it.
    pub(super) fn forget_edits(&mut self) {
        self.edits = EditHistory::default();
    }
}

#[cfg(test)]
#[path = "undo_tests.rs"]
mod tests;
