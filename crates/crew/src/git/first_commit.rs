//! A branch's first commit, made for a run that started before the branch had one
//! (`merge.unborn = "first_commit"`). The commit is made without a worktree, the branch is
//! created only while it still has nothing on it, and your index is then moved under the
//! commit, so the only changes git shows afterwards are the ones you made yourself.

use std::collections::BTreeSet;
use std::path::Path;

use super::{Git, GitError, failed};

/// Staged entries per `git update-index`, well under any command-line limit.
const ENTRIES_PER_CALL: usize = 200;

impl Git {
    /// A commit of `tree` with no parent, made without a worktree or an index. No branch
    /// points at it until the caller starts one there.
    pub async fn root_commit(
        &self,
        dir: &Path,
        tree: &str,
        message: &str,
    ) -> Result<String, GitError> {
        let identity = self.identity_env(dir).await;
        let args = ["commit-tree", tree, "-m", message];
        let out = self.output(dir, &args, &identity).await?;
        if !out.status.success() {
            return Err(failed(&args, &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Gives `branch` its first commit. Git refuses when the branch has one by now, so a
    /// commit you made in the meantime is never replaced.
    pub async fn start_branch(
        &self,
        dir: &Path,
        branch: &str,
        commit: &str,
    ) -> Result<(), GitError> {
        let reference = format!("refs/heads/{branch}");
        // An empty old value means the ref must not exist yet.
        self.run(dir, &["update-ref", &reference, commit, ""])
            .await
            .map(drop)
    }

    /// Moves your index in `checkout` under `commit`, the first commit of a branch that had
    /// none when `snapshot` was taken of your files. The index then lists what `commit`
    /// holds, so files you left alone show no change. What you staged that `snapshot` doesn't
    /// hold stays staged, and your files aren't touched. Doing it twice changes nothing more.
    pub async fn index_under_first_commit(
        &self,
        checkout: &Path,
        snapshot: &str,
        commit: &str,
    ) -> Result<(), GitError> {
        let yours = self.staged_apart_from(checkout, snapshot).await?;
        self.run(checkout, &["read-tree", commit]).await?;
        for entries in yours.chunks(ENTRIES_PER_CALL) {
            let mut args = vec!["update-index", "--add"];
            for entry in entries {
                args.extend(["--cacheinfo", entry.as_str()]);
            }
            self.run(checkout, &args).await?;
        }
        // A fresh index knows no file's size or time, so git would read every file again the
        // next time you ask for the status. It exits 1 for each file of yours that is different.
        self.output(checkout, &["update-index", "-q", "--refresh"], &[])
            .await?;
        Ok(())
    }

    /// What your index holds that `snapshot` doesn't, each as `mode,object,path`: a file you
    /// staged since, or another version of one it has.
    async fn staged_apart_from(
        &self,
        checkout: &Path,
        snapshot: &str,
    ) -> Result<Vec<String>, GitError> {
        let differing = [
            "diff-index",
            "--cached",
            "--name-only",
            "--no-renames",
            "-z",
            // A file of the snapshot your index lacks was never staged, which is no change.
            "--diff-filter=d",
            snapshot,
        ];
        let differing = self.run(checkout, &differing).await?;
        let differing: BTreeSet<&str> = differing
            .split('\0')
            .filter(|path| !path.is_empty())
            .collect();
        if differing.is_empty() {
            return Ok(Vec::new());
        }
        let entries = self.run(checkout, &["ls-files", "--stage", "-z"]).await?;
        Ok(entries
            .split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                let mut meta = meta.split(' ');
                let (mode, object) = (meta.next()?, meta.next()?);
                differing
                    .contains(path)
                    .then(|| format!("{mode},{object},{path}"))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    fn git() -> Git {
        Git::with_env(git_test_env())
    }

    /// Your files with nothing on your branch: `kept.txt` and `edited.txt` staged, `loose.txt`
    /// never staged, and a snapshot of all three as a run would take it. Returns the snapshot
    /// and a commit of the run's on top of it, which rewrites `kept.txt`.
    async fn before_a_first_commit(repo: &TestRepo, git: &Git) -> (String, String) {
        repo.write("kept.txt", "kept\n");
        repo.write("edited.txt", "edited\n");
        repo.write("loose.txt", "loose\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "your files"]);
        let tree = git.tree_of(&repo.root, "HEAD").await.unwrap();
        let snapshot = git
            .root_commit(&repo.root, &tree, "your files")
            .await
            .unwrap();
        repo.git(&["checkout", "-q", "--detach", &snapshot]);
        repo.write("kept.txt", "kept, and the run's line\n");
        repo.git(&["commit", "-qam", "the run"]);
        let run = git.head_commit(&repo.root).await.unwrap().unwrap();

        // Back to how the run found things: no commit on main, and one file never staged.
        repo.git(&["checkout", "-q", "main"]);
        repo.git(&["update-ref", "-d", "refs/heads/main"]);
        repo.git(&["rm", "-q", "--cached", "loose.txt"]);
        assert_eq!(git.head_commit(&repo.root).await.unwrap(), None);
        (snapshot, run)
    }

    #[tokio::test]
    async fn a_root_commit_has_no_parent_and_starts_only_a_branch_with_nothing_on_it() {
        let repo = TestRepo::new();
        let git = git();
        let (_, run) = before_a_first_commit(&repo, &git).await;
        let tree = git.tree_of(&repo.root, &run).await.unwrap();

        let first = git.root_commit(&repo.root, &tree, "first").await.unwrap();
        assert!(git.parents(&repo.root, &first).await.unwrap().is_empty());
        assert_eq!(git.tree_of(&repo.root, &first).await.unwrap(), tree);
        git.start_branch(&repo.root, "main", &first).await.unwrap();
        assert_eq!(
            git.head_commit(&repo.root).await.unwrap().as_deref(),
            Some(first.as_str())
        );

        let other = git.root_commit(&repo.root, &tree, "other").await.unwrap();
        assert!(
            git.start_branch(&repo.root, "main", &other).await.is_err(),
            "a branch with a commit was started over"
        );
        assert_eq!(
            git.head_commit(&repo.root).await.unwrap().as_deref(),
            Some(first.as_str())
        );
    }

    /// After the first commit only what you did since the snapshot shows as a change: an
    /// edit you staged stays staged, a new file you staged stays staged, and a file you never
    /// staged is simply committed.
    #[tokio::test]
    async fn the_index_follows_the_first_commit_and_keeps_what_you_staged_since() {
        let repo = TestRepo::new();
        let git = git();
        let (snapshot, run) = before_a_first_commit(&repo, &git).await;
        repo.write("edited.txt", "edited by you\n");
        repo.write("new.txt", "new\n");
        repo.git(&["add", "edited.txt", "new.txt"]);
        // Staged in one version and edited on, so only your index holds the staged one.
        repo.write("new.txt", "new, and more\n");
        // The run's work reaches your files before the commit is made.
        repo.write("kept.txt", "kept, and the run's line\n");

        let tree = git.tree_of(&repo.root, &run).await.unwrap();
        let first = git.root_commit(&repo.root, &tree, "first").await.unwrap();
        git.start_branch(&repo.root, "main", &first).await.unwrap();
        for _ in 0..2 {
            git.index_under_first_commit(&repo.root, &snapshot, &first)
                .await
                .unwrap();
            let status = repo.git(&["status", "--porcelain"]);
            let status: Vec<&str> = status.lines().collect();
            assert_eq!(status, ["M  edited.txt", "AM new.txt"]);
        }
        assert_eq!(repo.git(&["show", ":new.txt"]), "new");
        assert_eq!(repo.git(&["show", ":edited.txt"]), "edited by you");
        assert_eq!(repo.git(&["show", "HEAD:loose.txt"]), "loose");
    }

    #[tokio::test]
    async fn with_nothing_staged_at_all_the_index_is_simply_the_commit() {
        let repo = TestRepo::new();
        let git = git();
        let (snapshot, run) = before_a_first_commit(&repo, &git).await;
        std::fs::remove_file(repo.root.join(".git/index")).unwrap();
        repo.write("kept.txt", "kept, and the run's line\n");

        let tree = git.tree_of(&repo.root, &run).await.unwrap();
        let first = git.root_commit(&repo.root, &tree, "first").await.unwrap();
        git.start_branch(&repo.root, "main", &first).await.unwrap();
        git.index_under_first_commit(&repo.root, &snapshot, &first)
            .await
            .unwrap();
        assert_eq!(repo.git(&["status", "--porcelain"]), "");
    }
}
