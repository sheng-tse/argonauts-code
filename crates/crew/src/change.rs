//! A task's change: the files its agent wrote, as its edit tools recorded them and its report
//! lists them, plus its edits to files the repository tracks. Everything else a command left
//! in the task's folder, such as bytecode, caches and build output, is no part of it.
//!
//! There is one definition, and every part that handles a change reads it: the commit at a
//! session's end holds exactly this change, the checks run on a clean copy of that commit, the
//! reviewers and the sign-off read it, and the merge takes the task's branch, which holds
//! nothing else. Before the commit is made, [`of_task`] reads the same change from the folder
//! as it stands.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use argonauts_core::CardId;
use argonauts_core::layout;
use argonauts_core::paths::create_private_dir_all;

use crate::card::claims;
use crate::card::worktree::{self, CardTree};
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::git::{Claims, Git, LeftOut};

/// A task's change as it stands in its folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskChange {
    /// The commit the change is measured from.
    pub base: String,
    /// Every path it adds, edits or removes.
    pub files: Vec<String>,
    /// The diff reviewers read, each changed function shown whole. Empty when nothing changed.
    pub diff: String,
    /// New files in the folder that aren't part of it, and why.
    pub left_out: Vec<LeftOut>,
}

impl TaskChange {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// The task's change now, committed or not: what its commit holds once the session that is
/// working on it ends. Reading it changes nothing in the folder.
pub async fn of_task(host: &Host, card: &CardId) -> EngineResult<TaskChange> {
    let reading = Reading::of(host, card).await?;
    let change = reading
        .git
        .change_since(
            &reading.tree.path,
            &reading.tree.base,
            &reading.index,
            &reading.commits,
            reading.claims.as_ref(),
        )
        .await?;
    Ok(TaskChange {
        base: reading.tree.base,
        files: change.files,
        diff: change.diff,
        left_out: change.left_out,
    })
}

/// The task's change now as a commit on no branch, holding exactly what [`of_task`] reads,
/// for checks that run while its agents are still at work.
pub async fn commit_of_task(host: &Host, card: &CardId) -> EngineResult<String> {
    let reading = Reading::of(host, card).await?;
    Ok(reading
        .git
        .commit_of_change(
            &reading.tree.path,
            &reading.index,
            &reading.commits,
            reading.claims.as_ref(),
        )
        .await?)
}

/// What reading a task's change takes: its folder, the claims on it, the agent's own commits,
/// and a throwaway index of its own.
struct Reading {
    tree: CardTree,
    git: Git,
    claims: Option<Claims>,
    commits: Vec<String>,
    index: PathBuf,
}

impl Reading {
    async fn of(host: &Host, card: &CardId) -> EngineResult<Self> {
        let tree = worktree::ensure(host, card).await?;
        let git = worktree::tree_git(host, &tree.path).await?;
        let claims = claims::of_card(host, card, &tree.path);
        // The agent's own commits may have swept in what a command left behind; they're
        // sorted the way the session's commit sorts them. Your commits and the engine's are
        // kept whole.
        let engine = worktree::conductor_commits(host, card)?;
        let commits: Vec<String> = git
            .first_parent_log(&tree.path, &format!("{}..HEAD", tree.base))
            .await?
            .into_iter()
            .map(|(sha, _)| sha)
            .filter(|sha| !engine.contains(sha))
            .collect();
        let dir = layout::card_dir(&host.repo().run_dir, card);
        create_private_dir_all(&dir)?;
        let index = dir.join(format!("change-{}.index", next_index()));
        Ok(Self {
            tree,
            git,
            claims,
            commits,
            index,
        })
    }
}

/// A number for each throwaway index, so two readers of one task never share one.
fn next_index() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
