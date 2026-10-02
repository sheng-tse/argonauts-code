//! Prompt history in the composer: ↑ and ↓ walk it once the cursor leaves the draft's first or
//! last row, and Ctrl+R searches it. Enter in a search fills the input and never sends, so a
//! stray match can't go out as a prompt.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::edit::Draft;
use super::{Composer, ComposerEvent};
use crate::prompt_history::HISTORY_LIMIT;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Search {
    pub(super) query: String,
    /// The history index on show.
    found: Option<usize>,
    /// The query has letters that no entry holds.
    pub(super) failed: bool,
    /// The draft from before the search, back on cancel.
    stash: Draft,
}

impl Composer {
    /// Replaces the history, oldest first, such as with `history.jsonl` at start.
    pub fn set_history(&mut self, entries: impl IntoIterator<Item = String>) {
        self.history.clear();
        for entry in entries {
            self.remember(entry);
        }
        self.browsing = None;
        self.stash = None;
    }

    /// A repeat moves to the newest place, so each prompt is in history once.
    pub fn remember(&mut self, entry: impl Into<String>) {
        let entry = entry.into();
        if entry.trim().is_empty() || self.history.last() == Some(&entry) {
            return;
        }
        if let Some(at) = self.history.iter().position(|old| *old == entry) {
            self.history.remove(at);
        }
        self.history.push(entry);
        if self.history.len() > HISTORY_LIMIT {
            self.history.remove(0);
        }
    }

    /// Oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn is_searching(&self) -> bool {
        self.search.is_some()
    }

    /// What Ctrl+R's search has typed so far.
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|search| search.query.as_str())
    }

    pub(super) fn older(&mut self) -> ComposerEvent {
        let index = match self.browsing {
            None if self.history.is_empty() => return ComposerEvent::AtTop,
            None => {
                self.stash = Some(self.draft.clone());
                self.history.len() - 1
            }
            Some(0) => return ComposerEvent::AtTop,
            Some(index) => index - 1,
        };
        self.show_history(index);
        ComposerEvent::Moved
    }

    pub(super) fn newer(&mut self) -> ComposerEvent {
        let Some(index) = self.browsing else {
            return ComposerEvent::Unhandled;
        };
        if index + 1 < self.history.len() {
            self.show_history(index + 1);
        } else {
            self.draft = self.stash.take().unwrap_or_default();
            self.browsing = None;
        }
        self.preferred_col = None;
        ComposerEvent::Moved
    }

    fn show_history(&mut self, index: usize) {
        self.draft = Draft::from_text(&self.history[index]);
        self.browsing = Some(index);
        self.preferred_col = None;
    }

    pub(super) fn start_search(&mut self) -> ComposerEvent {
        self.browsing = None;
        self.stash = None;
        self.search = Some(Search {
            query: String::new(),
            found: None,
            failed: false,
            stash: self.draft.clone(),
        });
        ComposerEvent::Moved
    }

    /// Letters narrow the search, Ctrl+R goes to the next older match, Enter, Tab, Esc, and the
    /// arrows keep what's shown, and Ctrl+C or Ctrl+G put the draft back.
    pub(super) fn search_key(&mut self, key: KeyEvent) -> ComposerEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let Some(search) = self.search.as_mut() else {
            return ComposerEvent::Unhandled;
        };
        match key.code {
            KeyCode::Char('r') if ctrl => {
                let before = search.found.unwrap_or(self.history.len());
                self.search_before(before)
            }
            KeyCode::Char('c' | 'g') if ctrl => self.cancel_search(),
            KeyCode::Char(c) if ctrl == alt => {
                search.query.push(c);
                self.search_before(self.history.len())
            }
            KeyCode::Backspace => {
                search.query.pop();
                self.search_before(self.history.len())
            }
            KeyCode::Enter
            | KeyCode::Tab
            | KeyCode::Esc
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End => self.accept_search(),
            _ => ComposerEvent::Moved,
        }
    }

    /// The newest entry older than `before` that holds the query, ignoring case. With no match
    /// the last one found stays on show.
    fn search_before(&mut self, before: usize) -> ComposerEvent {
        let Some(search) = self.search.as_mut() else {
            return ComposerEvent::Unhandled;
        };
        if search.query.is_empty() {
            search.found = None;
            search.failed = false;
            self.draft = search.stash.clone();
            return ComposerEvent::Moved;
        }
        let query = search.query.to_lowercase();
        let found = self.history[..before.min(self.history.len())]
            .iter()
            .rposition(|entry| entry.to_lowercase().contains(&query));
        search.failed = found.is_none();
        if let Some(index) = found {
            search.found = Some(index);
            self.draft = Draft::from_text(&self.history[index]);
        }
        ComposerEvent::Moved
    }

    fn accept_search(&mut self) -> ComposerEvent {
        self.search = None;
        self.draft.cursor = self.draft.text.len();
        self.preferred_col = None;
        ComposerEvent::Edited
    }

    fn cancel_search(&mut self) -> ComposerEvent {
        if let Some(search) = self.search.take() {
            self.draft = search.stash;
        }
        ComposerEvent::Edited
    }
}
