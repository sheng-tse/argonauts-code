//! The deeper Behaviors map's update as the app reports it, kept while the panel is closed so
//! the tab shows it again when it opens.

use argonauts_core::deep_map::{DeepDone, DeepProgress};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeepUpdate {
    /// Where the update is, from its first request until it ends.
    pub running: Option<DeepProgress>,
    /// How the last one ended.
    pub last: Option<DeepDone>,
    /// The updates that ended, so a tab that drew one can ask for the map they made.
    pub ended: u32,
}

impl DeepUpdate {
    pub fn progressed(&mut self, progress: &DeepProgress) {
        self.running = Some(progress.clone());
    }

    pub fn done(&mut self, done: &DeepDone) {
        self.running = None;
        self.last = Some(done.clone());
        self.ended += 1;
    }
}
