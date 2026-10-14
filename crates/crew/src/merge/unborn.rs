//! A run that started in a repository with no commits yet. Its base is a snapshot of your
//! files (`argonauts_core::unborn`), which shares no history with anything you commit later.
//! So such a run never follows your branch: nothing of it is merged in, and the run's work
//! lands as changes to your files, compared with that snapshot, for you to commit. Under
//! `merge.unborn = "first_commit"` it makes your branch's first commit instead, while the
//! branch still has none.

use crate::engine::host::Host;
use crate::git::GitError;
use crate::merge::deliver;

/// What such a run is said to merge into, where another run names its target branch.
pub const YOUR_FILES: &str = "your files";

/// How a run that started before your first commit hands its work over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lands {
    /// As changes to your files, with nothing committed or staged.
    Files,
    /// As files as well, though your settings ask for a first commit: your branch got its
    /// first commit from you while the run worked, and that one stays yours.
    FilesAfterYourCommit,
    /// As the first commit of `branch`, which still has none.
    FirstCommit { branch: String },
    /// `branch` has the run's first commit already, from a landing that was cut short before
    /// the run recorded it.
    FirstCommitMade { branch: String, commit: String },
}

impl Lands {
    /// What the run is said to merge into.
    pub fn target(&self) -> &str {
        match self {
            Self::Files | Self::FilesAfterYourCommit => YOUR_FILES,
            Self::FirstCommit { branch } | Self::FirstCommitMade { branch, .. } => branch,
        }
    }
}

/// The snapshot the run started from, when the repository had no commits then.
pub async fn base(host: &Host) -> Result<Option<String>, GitError> {
    let name = argonauts_core::unborn::base_ref(host.run_id());
    host.git().ref_commit(&host.repo().root, &name).await
}

/// How the run lands, when it started before your first commit. Read again whenever it
/// matters, since you may commit while the run works or waits.
pub async fn lands(host: &Host) -> Result<Option<Lands>, GitError> {
    if base(host).await?.is_none() {
        return Ok(None);
    }
    if !argonauts_core::unborn::makes_first_commit(host.settings()) {
        return Ok(Some(Lands::Files));
    }
    let git = host.git();
    let root = &host.repo().root;
    let Some(branch) = git.current_branch(root).await? else {
        return Ok(Some(Lands::FilesAfterYourCommit));
    };
    Ok(Some(match git.head_commit(root).await? {
        None => Lands::FirstCommit { branch },
        Some(commit) if is_the_runs_first_commit(host, &commit).await? => {
            Lands::FirstCommitMade { branch, commit }
        }
        Some(_) => Lands::FilesAfterYourCommit,
    }))
}

/// Whether `commit` is the first commit this run made for your branch: one with no parent
/// that names the run.
async fn is_the_runs_first_commit(host: &Host, commit: &str) -> Result<bool, GitError> {
    let git = host.git();
    let root = &host.repo().root;
    if !git.parents(root, commit).await?.is_empty() {
        return Ok(false);
    }
    let log = git.log_messages(root, commit).await?;
    Ok(log
        .first()
        .is_some_and(|(_, message)| deliver::names_run(message, host.run_id())))
}

/// What the merge approval says when answering it makes your branch's first commit, so
/// nobody finds their own files committed without having been told.
pub async fn approval_line(host: &Host) -> Result<Option<String>, GitError> {
    Ok(match lands(host).await? {
        Some(Lands::FirstCommit { branch }) => Some(format!(
            "{branch} has no commits yet, so merging makes its first one: your files as the \
             run found them, with the run's work (merge.unborn)"
        )),
        _ => None,
    })
}

/// Your files that no longer hold what `base` has where the run's work up to `head` would be
/// written. Your index says nothing here: it never held the snapshot, and before a first
/// commit it lists every file you staged as a change.
pub async fn edited_in_the_way(
    host: &Host,
    base: &str,
    head: &str,
) -> Result<Vec<String>, GitError> {
    let repo = host.repo();
    let scratch = repo.run_dir.join("in-the-way.index");
    host.git()
        .differing_in_the_way(&repo.root, &scratch, base, head)
        .await
}
