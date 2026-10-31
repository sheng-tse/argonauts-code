//! The artifacts published while this screen watched: by the main agent in your conversation,
//! or by an agent inside a workflow. They're read off the tool calls as they
//! finish, and kept past the turn, since a turn's own calls are forgotten when the next starts.

use argonauts_core::RunId;
use argonauts_core::app::artifacts::{self, Artifact};
use argonauts_core::base::names::SessionKey;

use super::session::ToolRecord;

/// One published page and who published it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub artifact: Artifact,
    /// The agent that published it inside a run. `None` for the main agent.
    pub by: Option<(RunId, SessionKey)>,
}

/// Every artifact this conversation has published, oldest first. One published again moves to
/// the end, as the most recent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Artifacts {
    published: Vec<Published>,
}

impl Artifacts {
    /// Records the page a finished call published, if it published one.
    pub(super) fn note(&mut self, call: &ToolRecord, by: Option<(RunId, SessionKey)>) {
        if call.ok != Some(true) || !artifacts::publishes(&call.name, &call.input) {
            return;
        }
        let output = call.output.as_deref().unwrap_or_default();
        let Some(artifact) = artifacts::published(&call.input, output) else {
            return;
        };
        self.published
            .retain(|known| known.artifact.url != artifact.url);
        self.published.push(Published { artifact, by });
    }

    pub fn is_empty(&self) -> bool {
        self.published.is_empty()
    }

    /// Oldest first.
    pub fn all(&self) -> &[Published] {
        &self.published
    }

    /// The one Ctrl+] opens.
    pub fn latest(&self) -> Option<&Published> {
        self.published.last()
    }

    /// The latest artifact an agent published, for its row.
    pub fn by_agent(&self, run: &RunId, session: &SessionKey) -> Option<&Artifact> {
        self.published
            .iter()
            .rev()
            .find(|published| {
                published
                    .by
                    .as_ref()
                    .is_some_and(|(by_run, by)| by_run == run && by == session)
            })
            .map(|published| &published.artifact)
    }

    /// What the bottom line's part says beside the mode: the newest artifact's name, and how
    /// many more the conversation published. `None` until there is one.
    pub fn line_part(&self) -> Option<(&str, u32)> {
        let latest = self.latest()?;
        let others = u32::try_from(self.published.len() - 1).unwrap_or(u32::MAX);
        Some((latest.artifact.name.as_str(), others))
    }

    /// A new conversation starts with none.
    pub(super) fn clear(&mut self) {
        self.published.clear();
    }

    /// A conversation carried on brings the pages its main agent published, oldest first, in
    /// place of what the last conversation and its runs held. A run of its own that comes
    /// back on screen replays its agents' calls, and their pages with them.
    pub(super) fn resumed(&mut self, pages: &[Artifact]) {
        self.published = pages
            .iter()
            .map(|artifact| Published {
                artifact: artifact.clone(),
                by: None,
            })
            .collect();
    }
}
