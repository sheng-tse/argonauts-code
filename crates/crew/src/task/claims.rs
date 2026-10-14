//! Which new files in a task's worktree an agent meant to create. A command leaves caches,
//! build output and test leftovers beside the work, in every language, and no list of file
//! types tells them apart. What does is who made the file: an agent's edit tool wrote it, or
//! the agent's report lists it. Those are its claims, and a commit takes only the new files
//! they cover. Changes to files git already tracks are always the task's work, and so are
//! the new files `commit.always_keep` names. A CLI's own workflow is the exception: its
//! agents run in the background and no edit of theirs reaches the stream, so every new file
//! but build and cache output is claimed for it.

use std::path::Path;

use argonauts_core::TaskId;
use argonauts_core::settings::delivery::Unclaimed;

use crate::engine::host::Host;
use crate::git::Claims;
use crate::task::session::report_of;

/// The claims every session of `task` made so far, or `None` when the settings commit every
/// new file whoever made it.
pub(crate) fn of_task(host: &Host, task: &TaskId, worktree: &Path) -> Option<Claims> {
    if host.settings().commit.unclaimed == Unclaimed::Keep {
        return None;
    }
    let (written, reported, ran_a_workflow) = host.read(|state| {
        let mut written = Vec::new();
        let mut reported = Vec::new();
        let mut ran_a_workflow = false;
        for session in state.sessions.by_key.values() {
            if session.task.as_ref() != Some(task) {
                continue;
            }
            written.extend(session.written.iter().cloned());
            if let Some(report) = session.end.as_ref().and_then(report_of) {
                reported.extend(report.files_changed);
            }
            ran_a_workflow |= session.workflow.is_some();
        }
        (written, reported, ran_a_workflow)
    });
    let mut claims = Claims::default().always_keeping(&host.settings().commit.always_keep);
    if ran_a_workflow || host.shared().claude_codes_own(task).is_some() {
        claims = claims.every_new_file();
    }
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
