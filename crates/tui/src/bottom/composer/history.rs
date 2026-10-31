//! Prompt history in the composer: ↑ and ↓ walk it once the cursor leaves the draft's first or
//! last row. Searching it is the bottom's [`HistorySearch`](crate::bottom::HistorySearch).

use super::{Composer, ComposerEvent};
use crate::base::input::prompt_history::HISTORY_LIMIT;

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

    pub(super) fn older(&mut self) -> ComposerEvent {
        let index = match self.browsing {
            None if self.history.is_empty() => return ComposerEvent::AtTop,
            None => {
                self.stash = Some(self.saved());
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
            let saved = self.stash.take().unwrap_or_default();
            self.put_back(saved);
            self.browsing = None;
        }
        self.preferred_col = None;
        ComposerEvent::Moved
    }

    fn show_history(&mut self, index: usize) {
        self.show_entry(index);
        self.browsing = Some(index);
        self.preferred_col = None;
    }
}
