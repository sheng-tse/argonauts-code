//! Git for what a run leaves outside its own copies: pushing the branch a pull request opens
//! from, and removing what an ended run leaves in a repository, the worktrees git still lists
//! and the branches and refs under the run's name.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{Git, GitError};

impl Git {
    /// What its commands run with over the process's own environment, for a tool run beside
    /// git, such as `gh`.
    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    /// Where `program` is installed, looked for on the `PATH` git itself runs with.
    pub fn installed(&self, program: &str) -> Option<PathBuf> {
        let path = self
            .env
            .iter()
            .find(|(name, _)| name == "PATH")
            .map(|(_, path)| OsString::from(path))
            .or_else(|| std::env::var_os("PATH"))?;
        let here = std::env::current_dir().ok()?;
        which::which_in(program, Some(path), here).ok()
    }

    /// Forgets worktrees whose folders are gone.
    pub async fn prune_worktrees(&self, repo: &Path) -> Result<(), GitError> {
        self.run_on_worktrees(repo, &["worktree", "prune"])
            .await
            .map(drop)
    }

    /// The branches whose names start with `prefix`, such as `argonauts/<run>/`.
    pub async fn branches_under(&self, repo: &Path, prefix: &str) -> Result<Vec<String>, GitError> {
        let under = format!("refs/heads/{prefix}");
        let args = ["for-each-ref", "--format=%(refname:short)", &under];
        let listed = self.run(repo, &args).await?;
        Ok(listed
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Deletes the ref `name`. One that isn't there is no error.
    pub async fn delete_ref(&self, repo: &Path, name: &str) -> Result<(), GitError> {
        self.run(repo, &["update-ref", "-d", name]).await.map(drop)
    }

    /// Pushes `branch` to `remote` under its own name, which a pull request needs before it
    /// can be opened. Git never asks for a password here: a push that needs one fails.
    pub async fn push(&self, repo: &Path, remote: &str, branch: &str) -> Result<(), GitError> {
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        self.run(repo, &["push", "--quiet", remote, &refspec])
            .await
            .map(drop)
    }

    /// Pushes `commit` to `remote` as `branch`, for a branch whose own history stays here.
    pub async fn push_commit(
        &self,
        repo: &Path,
        remote: &str,
        commit: &str,
        branch: &str,
    ) -> Result<(), GitError> {
        let refspec = format!("{commit}:refs/heads/{branch}");
        self.run(repo, &["push", "--quiet", remote, &refspec])
            .await
            .map(drop)
    }

    /// Points the ref `name` at `commit`, making it when it isn't there.
    pub async fn set_ref(&self, repo: &Path, name: &str, commit: &str) -> Result<(), GitError> {
        self.run(repo, &["update-ref", name, commit])
            .await
            .map(drop)
    }

    /// Points `branch` at `commit` wherever it stood, for a branch checked out nowhere.
    pub async fn set_branch(
        &self,
        repo: &Path,
        branch: &str,
        commit: &str,
    ) -> Result<(), GitError> {
        self.set_ref(repo, &format!("refs/heads/{branch}"), commit)
            .await
    }

    /// The bytes the repository's own folder takes on disk, packed and loose objects alike.
    pub async fn size_on_disk(&self, repo: &Path) -> Result<u64, GitError> {
        let counted = self.run(repo, &["count-objects", "-v"]).await?;
        let kilobytes = |key: &str| {
            counted
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .and_then(|size| size.trim().parse::<u64>().ok())
                .unwrap_or(0)
        };
        Ok((kilobytes("size:") + kilobytes("size-pack:")).saturating_mul(1024))
    }

    /// Packs the repository and drops what no branch reaches anymore, at once, since the
    /// commits of ended runs are what takes the room.
    pub async fn pack(&self, repo: &Path) -> Result<(), GitError> {
        let expire = ["reflog", "expire", "--expire=now", "--all"];
        self.run(repo, &expire).await?;
        self.run(repo, &["gc", "--quiet", "--prune=now"])
            .await
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{TestRepo, git_test_env};

    #[tokio::test]
    async fn a_runs_branches_are_listed_by_its_prefix_and_deleted() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        let root = repo.root.as_path();
        let head = git.resolve(root, "HEAD").await.unwrap();
        for branch in [
            "argonauts/run-1/merged",
            "argonauts/run-1/task-001",
            "argonauts/run-10/merged",
        ] {
            git.create_branch(root, branch, &head).await.unwrap();
        }
        let listed = git.branches_under(root, "argonauts/run-1/").await.unwrap();
        assert_eq!(
            listed,
            ["argonauts/run-1/merged", "argonauts/run-1/task-001"]
        );

        for branch in listed {
            git.delete_branch(root, &branch).await.unwrap();
        }
        let left = git.branches_under(root, "argonauts/").await.unwrap();
        assert_eq!(left, ["argonauts/run-10/merged"]);

        git.delete_ref(root, "refs/argonauts/run-1/base")
            .await
            .unwrap();
        assert!(git.size_on_disk(root).await.unwrap() > 0);
        git.pack(root).await.unwrap();
        assert_eq!(git.resolve(root, "HEAD").await.unwrap(), head);
    }
}
