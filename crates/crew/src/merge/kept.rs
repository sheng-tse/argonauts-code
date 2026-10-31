//! What a run that started from your uncommitted edits leaves when it ends unmerged. Its
//! history grew from a snapshot of those edits, so its branch as it is would carry them
//! wherever it goes: to a remote under a pull request, or into your log if you merged it
//! later. What is pushed, and what the branch is left at, is the commit a merge would have
//! made instead: the run's work alone, on top of the branch it would have merged into.

use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::{Note, NoteLevel, RunId};

use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::merge::branch::PREFIX;
use crate::merge::{deliver, final_check, merge_packet};

/// Holds the commit made for `run`'s branch, so what was pushed and what is kept are the
/// same commit.
pub fn kept_ref(run: &RunId) -> String {
    format!("refs/argonauts/{run}/kept")
}

/// The commit that stands in for the run's own history wherever its branch goes. `None` for a
/// run that started from a commit of yours, whose history holds nothing that isn't its work.
pub async fn without_yours(host: &Host) -> EngineResult<Option<String>> {
    let Some(snapshot) = deliver::snapshot_of_yours(host) else {
        return Ok(None);
    };
    let Some(branch) = host.read(merge_packet::landing_branch_of) else {
        return Ok(None);
    };
    let git = host.git();
    let root = &host.repo().root;
    let head = git.resolve(root, &branch).await?;
    let target = match final_check::target(host) {
        Some(target) => final_check::main_commit(host, &target).await.ok(),
        None => None,
    };
    let main = match target {
        Some(main) => main,
        // No branch to merge into: the commit your checkout was at when the run started.
        None => git
            .parents(root, &snapshot)
            .await?
            .into_iter()
            .next()
            .unwrap_or(snapshot),
    };
    let commit = deliver::commit(host, root, &main, &head).await?;
    let name = kept_ref(host.run_id());
    if let Some(made) = git.ref_commit(root, &name).await?
        && git.tree_of(root, &made).await? == git.tree_of(root, &commit).await?
        && git.parents(root, &made).await? == git.parents(root, &commit).await?
    {
        return Ok(Some(made));
    }
    git.set_ref(root, &name, &commit).await?;
    Ok(Some(commit))
}

/// What a pull request says under its title for `commit`: its message after the subject.
pub async fn body_of(host: &Host, commit: &str) -> EngineResult<String> {
    let log = host
        .git()
        .log_messages(&host.repo().root, &format!("{commit}^!"))
        .await?;
    let message = log.into_iter().next().map(|(_, message)| message);
    let body = message
        .as_deref()
        .and_then(|message| message.split_once('\n'))
        .map(|(_, body)| body.trim().to_owned());
    Ok(body.unwrap_or_default())
}

/// The branch a run that ended as `outcome` leaves you, and the commit to leave it at, read
/// while the run's copies are still there. `None` when the branch stays as it is.
pub async fn at_end(host: &Host, outcome: RunOutcome) -> Option<Kept> {
    if !matches!(outcome, RunOutcome::BranchLeft | RunOutcome::PullRequest) {
        return None;
    }
    let branch = host.read(merge_packet::landing_branch_of)?;
    match without_yours(host).await {
        Ok(commit) => Some(Kept {
            branch,
            commit: commit?,
        }),
        Err(err) => {
            let note = Note {
                level: NoteLevel::Warn,
                text: format!(
                    "{branch} couldn't be rebuilt without what you hadn't committed when the \
                     run started ({err}), so its history still holds those edits. Look before \
                     you push it."
                ),
            };
            // The run ends either way.
            let _ = host.record(None, None, RunEventKind::Note(note));
            None
        }
    }
}

/// A run's branch and the commit it's left at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    branch: String,
    commit: String,
}

impl Kept {
    /// Leaves the branch at its commit once the run's copies are gone, and removes the run's
    /// other branches, whose history holds your edits.
    pub async fn leave(&self, host: &Host) {
        let git = host.git();
        let root = &host.repo().root;
        if let Err(err) = git.set_branch(root, &self.branch, &self.commit).await {
            tracing::warn!("leaving {} at {}: {err}", self.branch, self.commit);
            return;
        }
        let prefix = format!("{PREFIX}{}/", host.run_id());
        let others = git.branches_under(root, &prefix).await.unwrap_or_default();
        for other in others.iter().filter(|other| **other != self.branch) {
            if let Err(err) = git.delete_branch(root, other).await {
                tracing::warn!("deleting {other}: {err}");
            }
        }
        if let Err(err) = git.delete_ref(root, &kept_ref(host.run_id())).await {
            tracing::warn!("deleting the run's kept ref: {err}");
        }
    }

    /// Leaves the branch at its commit where `runs.keep_copies` keeps the copy it is checked
    /// out in, which follows it. The run's other branches stay with their copies.
    pub async fn leave_in_copy(&self, host: &Host) {
        let Ok((_, copy)) = merge_packet::landing_branch(host).await else {
            return;
        };
        if let Err(err) = host.git().reset_hard(&copy, &self.commit).await {
            tracing::warn!("leaving {} at {}: {err}", self.branch, self.commit);
        }
    }
}
