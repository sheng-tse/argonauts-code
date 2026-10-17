//! The word `ultracode` in a draft asks for a workflow for that one request. Claude Code colors
//! the word, says so in a notice, and lets Opt+W ignore it for the prompt. Taking the word out
//! of the draft forgets the ignore.

use argonauts_core::keywords::{self, Keyword};

use super::Composer;

/// The notice while the draft holds the word, with the key that ignores it.
pub fn requested_notice(key: &str) -> String {
    format!("Dynamic workflow requested for this turn · {key} to ignore")
}

/// The notice after the key ignored the word, with the key that brings it back.
pub fn ignored_notice(key: &str) -> String {
    format!("Ultracode keyword ignored for this prompt · {key} to undo")
}

impl Composer {
    /// The draft holds the word where it counts: a message for the agent, never a shell
    /// command or a field inside a question, while the trigger is on.
    pub fn holds_workflow_word(&self) -> bool {
        !self.workflow_word_off
            && !self.shell
            && !self.field
            && keywords::asks_for_workflow(&self.draft.text)
    }

    /// The draft asks for a workflow: it holds the word, and Opt+W hasn't ignored it.
    pub fn workflow_requested(&self) -> bool {
        if !self.holds_workflow_word() {
            self.workflow_ignored.set(false);
            return false;
        }
        !self.workflow_ignored.get()
    }

    /// Opt+W: ignores the word for this prompt, or brings it back. `None` when the draft has
    /// no such word, so the key is free for whatever else takes it.
    pub fn toggle_workflow_word(&mut self) -> Option<bool> {
        if !self.holds_workflow_word() {
            return None;
        }
        let ignored = !self.workflow_ignored.get();
        self.workflow_ignored.set(ignored);
        Some(ignored)
    }

    /// Whether the prompt just sent asked for a workflow. Read once, by what sends it on.
    pub fn take_workflow_request(&mut self) -> bool {
        std::mem::take(&mut self.workflow_sent)
    }

    /// The keywords the input draws: the main agent CLI's, and the workflow word while it
    /// counts.
    pub(super) fn drawn_keywords(&self) -> Vec<Keyword> {
        let mut drawn = self.keywords.clone();
        if self.workflow_requested() {
            drawn.push(keywords::ULTRACODE);
        }
        drawn
    }
}
