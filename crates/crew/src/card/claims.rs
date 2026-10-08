//! Which new files in a task's worktree an agent meant to create. A command leaves caches,
//! build output and test leftovers beside the work, in every language, and no list of file
//! types tells them apart. What does is who made the file: an agent's edit tool wrote it, or
//! the agent's report lists it. Those are its claims, and a commit takes only the new files
//! they cover. Changes to files git already tracks are always the task's work, and so are
//! the new files `commit.always_keep` names.

use std::path::Path;

use argonauts_core::CardId;
use argonauts_core::settings::delivery::Unclaimed;

use crate::card::session::report_of;
use crate::engine::host::Host;
use crate::git::Claims;

/// The claims every session of `card` made so far, or `None` when the settings commit every
/// new file whoever made it.
pub(crate) fn of_card(host: &Host, card: &CardId, worktree: &Path) -> Option<Claims> {
    if host.settings().commit.unclaimed == Unclaimed::Keep {
        return None;
    }
    let (written, reported) = host.read(|state| {
        let mut written = Vec::new();
        let mut reported = Vec::new();
        for session in state.sessions.by_key.values() {
            if session.card.as_ref() != Some(card) {
                continue;
            }
            written.extend(session.written.iter().cloned());
            if let Some(report) = session.end.as_ref().and_then(report_of) {
                reported.extend(report.files_changed);
            }
        }
        (written, reported)
    });
    let mut claims = Claims::default().always_keeping(&host.settings().commit.always_keep);
    for path in written.iter().chain(&reported) {
        claims.add(worktree, path);
    }
    Some(claims)
}

/// The claims a merge on the run's own branch is finished with: none. Checks run there with
/// the merge staged, and what they leave behind is nobody's work, so the merge commit takes
/// what the two sides committed. `None` when the settings commit every new file.
pub(crate) fn of_a_merge(host: &Host) -> Option<Claims> {
    (host.settings().commit.unclaimed != Unclaimed::Keep).then(Claims::default)
}
