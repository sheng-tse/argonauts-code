//! The copies a run works in, and what becomes of them when it ends. Each run has full
//! copies of your folder under Argonauts Code's home: `base` for read-only sessions, one for
//! each task, and the one its tasks merge in. They're only for the run, so they go when it
//! ends, and so do its branches once its work is merged or abandoned. A run that only waits,
//! or that you stopped and may reopen, keeps everything for `/resume`.

use std::path::{Path, PathBuf};

use argonauts_core::base::log::COPIES_REMOVED;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::{Note, NoteLevel, RunId, runs::unborn};

use crate::engine::host::Host;
use crate::git::Git;
use crate::merge::branch::{PREFIX, run_worktree};
use crate::merge::{kept, your_folder};

/// Ends the name of the copy a task's checks run in, beside the task's own.
const CHECKS: &str = "-checks";

/// Whether a run that ended this way is over for good. A stopped run opens again with
/// `/resume`, in the copies it has.
pub fn ends_for_good(outcome: RunOutcome) -> bool {
    outcome != RunOutcome::Stopped
}

/// Whether a run that ended this way still needs its branches: they are what it leaves you
/// when it's left on its branch or goes up as a pull request, and a failed run's work is
/// only on them.
fn keeps_its_branches(outcome: RunOutcome) -> bool {
    matches!(
        outcome,
        RunOutcome::BranchLeft | RunOutcome::PullRequest | RunOutcome::Failed
    )
}

/// Removes what the run that ended with `outcome` no longer needs, and notes what went. Git
/// refusing to remove a copy never fails the run; the copy stays for `/git clean`.
pub async fn remove_at_end(host: &Host, outcome: RunOutcome) {
    if !ends_for_good(outcome) {
        return;
    }
    your_folder::say_if_it_has_git_now(host);
    let kept = kept::at_end(host, outcome).await;
    if host.settings().runs.keep_copies {
        if let Some(kept) = &kept {
            kept.leave_in_copy(host).await;
        }
        return;
    }
    let repo = host.repo();
    let git = host.git();
    let merge_copy = run_worktree(host);
    let removed = remove_copies(git, &repo.root, &repo.worktrees, &merge_copy).await;
    if !keeps_its_branches(outcome) {
        remove_branches(git, &repo.root, host.run_id()).await;
    } else if let Some(kept) = &kept {
        kept.leave(host).await;
    }
    if let Some(removed) = removed.named() {
        let note = Note {
            level: NoteLevel::Info,
            text: format!("{COPIES_REMOVED}{removed}"),
        };
        // The copies are gone either way.
        let _ = host.record(None, None, RunEventKind::Note(note));
    }
}

/// The copies removed for one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Removed {
    pub base: bool,
    pub tasks: usize,
    pub merge: bool,
}

impl Removed {
    /// `base, 2 tasks, merge`, or `None` when nothing went.
    pub fn named(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.base {
            parts.push("base".to_owned());
        }
        match self.tasks {
            0 => {}
            1 => parts.push("1 task".to_owned()),
            tasks => parts.push(format!("{tasks} tasks")),
        }
        if self.merge {
            parts.push("merge".to_owned());
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    pub fn count(&self) -> usize {
        usize::from(self.base) + self.tasks + usize::from(self.merge)
    }
}

/// Removes every copy under `worktrees`, one run's folder of them, from the repository at
/// `root`, then the folder itself. `merge_copy` is the one the run's tasks merge in.
pub async fn remove_copies(git: &Git, root: &Path, worktrees: &Path, merge_copy: &Path) -> Removed {
    let mut removed = Removed::default();
    for copy in copies_in(worktrees) {
        let gone = match git.remove_worktree(root, &copy).await {
            Ok(()) => true,
            // Git no longer knows a copy whose own `.git` file is lost; its files are still
            // only the run's.
            Err(_) if !copy.join(".git").exists() => std::fs::remove_dir_all(&copy).is_ok(),
            Err(err) => {
                tracing::warn!("removing {}: {err}", copy.display());
                false
            }
        };
        if !gone {
            continue;
        }
        let name = copy.file_name().map(|name| name.to_string_lossy());
        if copy == merge_copy {
            removed.merge = true;
        } else if name.as_deref() == Some("base") {
            removed.base = true;
        } else if !name.is_some_and(|name| name.ends_with(CHECKS)) {
            // A task's copy for its checks goes with it, and isn't a task of its own.
            removed.tasks += 1;
        }
    }
    if let Err(err) = git.prune_worktrees(root).await {
        tracing::warn!("pruning worktrees: {err}");
    }
    // Only when empty: a copy git refused to remove stays where it is.
    let _ = std::fs::remove_dir(worktrees);
    removed
}

fn copies_in(worktrees: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(worktrees) else {
        return Vec::new();
    };
    let mut copies: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    copies.sort();
    copies
}

/// Deletes every branch and ref the run made, all under `argonauts/<run>/`, and returns how
/// many branches went.
pub async fn remove_branches(git: &Git, root: &Path, run: &RunId) -> usize {
    let prefix = format!("{PREFIX}{run}/");
    let mut removed = 0;
    match git.branches_under(root, &prefix).await {
        Ok(branches) => {
            for branch in branches {
                match git.delete_branch(root, &branch).await {
                    Ok(()) => removed += 1,
                    Err(err) => tracing::warn!("deleting {branch}: {err}"),
                }
            }
        }
        Err(err) => tracing::warn!("listing the run's branches: {err}"),
    }
    for name in [unborn::base_ref(run), kept::kept_ref(run)] {
        if let Err(err) = git.delete_ref(root, &name).await {
            tracing::warn!("deleting {name}: {err}");
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_went_is_named_in_the_order_a_run_makes_it() {
        let all = Removed {
            base: true,
            tasks: 2,
            merge: true,
        };
        assert_eq!(all.named().as_deref(), Some("base, 2 tasks, merge"));
        assert_eq!(all.count(), 4);
        let one = Removed {
            tasks: 1,
            ..Removed::default()
        };
        assert_eq!(one.named().as_deref(), Some("1 task"));
        assert_eq!(Removed::default().named(), None);
    }

    #[test]
    fn only_a_run_that_can_open_again_keeps_its_copies() {
        for outcome in [
            RunOutcome::Merged,
            RunOutcome::PullRequest,
            RunOutcome::BranchLeft,
            RunOutcome::Abandoned,
            RunOutcome::Failed,
        ] {
            assert!(ends_for_good(outcome), "{outcome:?}");
        }
        assert!(!ends_for_good(RunOutcome::Stopped));
        assert!(!keeps_its_branches(RunOutcome::Merged));
        assert!(!keeps_its_branches(RunOutcome::Abandoned));
        assert!(keeps_its_branches(RunOutcome::BranchLeft));
        assert!(keeps_its_branches(RunOutcome::PullRequest));
        assert!(keeps_its_branches(RunOutcome::Failed));
    }
}
