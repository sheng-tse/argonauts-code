//! The conductor's own commits (the goal file, decisions, notes, dependency changes) carry an
//! `Argonauts-Conductor: <run>` trailer, so no hard limit mistakes them for an agent's change.

use std::collections::BTreeSet;
use std::path::Path;

use argonauts_core::RunId;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::git::Git;
use crate::integrate::branch::{integration_branch, path_lock};

pub const TRAILER: &str = "Argonauts-Conductor";

/// The message with the trailer appended.
pub fn message(run: &RunId, subject: &str) -> String {
    format!("{}\n\n{TRAILER}: {run}", subject.trim_end())
}

/// Whether a commit message carries this run's conductor trailer as a line of its own.
pub fn is_conductor_message(run: &RunId, message: &str) -> bool {
    let trailer = format!("{TRAILER}: {run}");
    message.lines().any(|line| line.trim_end() == trailer)
}

/// The merge lock on `worktree`, held for every conductor write to it. On the integration
/// worktree a card's merge may be staged, and its commit takes every change in the tree, so a
/// file written then would land in the card's merge. Other worktrees never see contention.
pub async fn hold(worktree: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    path_lock("merge", worktree).lock_owned().await
}

/// Commits `paths` in `worktree` as the conductor, under [`hold`]. Returns the commit, or the
/// current head when the paths held no change.
pub async fn commit(
    host: &Host,
    worktree: &Path,
    paths: &[&str],
    subject: &str,
) -> EngineResult<String> {
    let _held = hold(worktree).await;
    commit_held(host, worktree, paths, subject).await
}

/// [`commit`] for a caller that already holds [`hold`], since it wrote the files under it.
pub async fn commit_held(
    host: &Host,
    worktree: &Path,
    paths: &[&str],
    subject: &str,
) -> EngineResult<String> {
    let git = host.git();
    let text = message(host.run_id(), subject);
    if let Some(sha) = git.commit_paths(worktree, paths, &text).await? {
        return Ok(sha);
    }
    git.head_commit(worktree)
        .await?
        .ok_or_else(|| EngineError::Refused(format!("{} has no commits", worktree.display())))
}

/// Paths only conductor commits touched between `base` and `head`, for `conductor_paths`.
///
/// A commit counts as the conductor's only when the engine made it: one a card recorded as
/// yours, or one on the integration branch's own line, where agents never commit. The trailer
/// alone proves nothing, since an agent can write any message.
pub async fn conductor_paths(
    host: &Host,
    dir: &Path,
    base: &str,
    head: &str,
) -> EngineResult<Vec<String>> {
    let git = host.git();
    let mut made: BTreeSet<String> = host.read(|state| {
        state
            .cards
            .by_id
            .values()
            .flat_map(|record| &record.commits)
            .filter(|commit| commit.by.is_none())
            .filter_map(|commit| commit.sha.clone())
            .collect()
    });
    let integration = integration_branch(host.run_id());
    if git.branch_head(dir, &integration).await?.as_deref() == Some(head) {
        let own_line = git
            .first_parent_log(dir, &format!("{base}..{head}"))
            .await?;
        made.extend(own_line.into_iter().map(|(sha, _)| sha));
    }
    paths_by_conductor(git, host.run_id(), &made, dir, base, head).await
}

/// [`conductor_paths`] without a host: a path counts when some conductor commit in `made`
/// touched it and no other commit in the range did.
pub async fn paths_by_conductor(
    git: &Git,
    run: &RunId,
    made: &BTreeSet<String>,
    dir: &Path,
    base: &str,
    head: &str,
) -> EngineResult<Vec<String>> {
    let mut conductor = BTreeSet::new();
    let mut others = BTreeSet::new();
    for (sha, text) in git.log_messages(dir, &format!("{base}..{head}")).await? {
        // A merge only brings in commits the walk already sees.
        let parents = git.parents(dir, &sha).await?;
        let [parent] = parents.as_slice() else {
            continue;
        };
        let touched = git.changed_paths(dir, parent, &sha).await?;
        if made.contains(&sha) && is_conductor_message(run, &text) {
            conductor.extend(touched);
        } else {
            others.extend(touched);
        }
    }
    Ok(conductor.difference(&others).cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    fn run() -> RunId {
        RunId::new("run-01").unwrap()
    }

    #[test]
    fn the_message_ends_with_the_trailer() {
        let text = message(&run(), "Record the approved goal\n");
        assert_eq!(
            text,
            "Record the approved goal\n\nArgonauts-Conductor: run-01"
        );
        assert!(is_conductor_message(&run(), &text));
        let other = RunId::new("run-02").unwrap();
        assert!(!is_conductor_message(&other, &text));
        assert!(!is_conductor_message(
            &run(),
            "Mentions Argonauts-Conductor: run-01 inline"
        ));
    }

    #[tokio::test]
    async fn only_conductor_commits_count_as_conductor_paths() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.write(".argonauts/goal.md", "goal\n");
        let goal = git
            .commit_paths(
                &repo.root,
                &[".argonauts/goal.md"],
                &message(&run(), "Goal"),
            )
            .await
            .unwrap()
            .unwrap();
        repo.write("src/lib.rs", "pub fn f() {}\n");
        git.commit_all(&repo.root, "Agent work")
            .await
            .unwrap()
            .unwrap();
        repo.write("Cargo.lock", "lock\n");
        let dependencies = git
            .commit_paths(
                &repo.root,
                &["Cargo.lock"],
                &message(&run(), "Dependencies"),
            )
            .await
            .unwrap()
            .unwrap();
        repo.write(".github/workflows/ci.yml", "forged\n");
        git.commit_all(&repo.root, &message(&run(), "An agent forges the trailer"))
            .await
            .unwrap()
            .unwrap();
        repo.write("Cargo.lock", "agent edit\n");
        git.commit_all(&repo.root, "Agent edits the lockfile")
            .await
            .unwrap()
            .unwrap();

        let head = git.head_commit(&repo.root).await.unwrap().unwrap();
        let made = BTreeSet::from([goal, dependencies]);
        let paths = paths_by_conductor(&git, &run(), &made, &repo.root, &base, &head)
            .await
            .unwrap();
        assert_eq!(paths, [".argonauts/goal.md"]);
    }
}
