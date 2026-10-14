//! Git for landing a run that started from what you hadn't committed. Its base is a snapshot:
//! your branch's tip with your edits on top. Those edits are yours to commit, so the commit
//! that lands holds the run's work without them, your files get the run's work on top of
//! them, and they stay uncommitted, as you left them.

use std::path::Path;

use super::{Git, GitError, PATHS_PER_CALL};

impl Git {
    /// The tree to land on `main` for `head`, which grew from `snapshot`: `main`'s files with
    /// what changed from `snapshot` to `head`, so nothing `snapshot` held over your branch is
    /// in it. Where the run rewrote the very lines you had edited, its lines go in, since
    /// they are its work. Built without touching a file, your index or a branch; the index
    /// at `scratch` is used only for a file git can't merge line by line.
    pub async fn tree_without_yours(
        &self,
        dir: &Path,
        scratch: &Path,
        snapshot: &str,
        main: &str,
        head: &str,
    ) -> Result<String, GitError> {
        let base = format!("--merge-base={snapshot}");
        let merged = match self.merge_tree(dir, &[&base, "-Xtheirs"], main, head).await {
            Ok(merged) => merged,
            // A git that can't be told whose lines win leaves such a file to the next step.
            Err(_) => self.merge_tree(dir, &[&base], main, head).await?,
        };
        if merged.both_changed.is_empty() {
            return Ok(merged.tree);
        }
        self.tree_with(dir, scratch, &merged.tree, head, &merged.both_changed)
            .await
    }

    /// Puts your checkout under `commit`, which the run just made on `main` beside your edits,
    /// once your branch points at it. Your index moves to `commit` at the paths it changed,
    /// but for one where you staged a version that is neither `main`'s nor `snapshot`'s,
    /// which stays staged. A file that changed and still holds what `main` has, because you
    /// took your edit back, gets `commit`'s version, as a fast-forward would give it. Doing
    /// it twice changes nothing more.
    pub async fn follow_beside(
        &self,
        checkout: &Path,
        scratch: &Path,
        snapshot: &str,
        main: &str,
        commit: &str,
    ) -> Result<(), GitError> {
        let changed = self.changed_paths(checkout, main, commit).await?;
        if changed.is_empty() {
            return Ok(());
        }
        let staged = self
            .staged_apart(checkout, main, snapshot, &changed)
            .await?;
        let follow: Vec<&str> = changed
            .iter()
            .filter(|path| !staged.contains(*path))
            .map(String::as_str)
            .collect();
        for chunk in follow.chunks(PATHS_PER_CALL) {
            let mut args = vec!["--literal-pathspecs", "reset", "--quiet", commit, "--"];
            args.extend(chunk);
            self.run(checkout, &args).await?;
        }
        let as_main = self.holding(checkout, main, &changed).await?;
        if !as_main.is_empty() {
            let others: Vec<String> = changed
                .into_iter()
                .filter(|path| !as_main.contains(path))
                .collect();
            self.write_files(checkout, scratch, main, commit, &others)
                .await?;
        }
        // It exits 1 for each file of yours that is different, which is what stays yours.
        self.output(checkout, &["update-index", "-q", "--refresh"], &[])
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    /// A repository where you edited `yours.txt` and `both.txt` without committing, and a
    /// run that started from that changed `both.txt` further down and added `new.txt`.
    /// Returns your branch's tip, the snapshot and the run's head, with your checkout back
    /// as it was when the run started.
    async fn started_from_your_edits(repo: &TestRepo, git: &Git) -> (String, String, String) {
        repo.write("yours.txt", "one\n");
        repo.write("both.txt", "a\nb\nc\nd\ne\nf\ng\nh\n");
        let main = git.commit_all(&repo.root, "files").await.unwrap().unwrap();
        repo.write("yours.txt", "one, edited\n");
        repo.write("both.txt", "a, edited\nb\nc\nd\ne\nf\ng\nh\n");
        let snapshot = git
            .commit_all(&repo.root, "snapshot")
            .await
            .unwrap()
            .unwrap();
        repo.write("both.txt", "a, edited\nb\nc\nd\ne\nf\ng\nh, by the run\n");
        repo.write("new.txt", "new\n");
        let head = git
            .commit_all(&repo.root, "the run")
            .await
            .unwrap()
            .unwrap();
        repo.git(&["reset", "-q", "--hard", &snapshot]);
        repo.git(&["reset", "-q", "--mixed", &main]);
        (main, snapshot, head)
    }

    #[tokio::test]
    async fn the_tree_for_your_branch_holds_the_runs_work_and_none_of_your_edits() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        let (main, snapshot, head) = started_from_your_edits(&repo, &git).await;
        let scratch = repo.root.join(".git/scratch.index");

        let tree = git
            .tree_without_yours(&repo.root, &scratch, &snapshot, &main, &head)
            .await
            .unwrap();
        let show = |path: &str| repo.git(&["show", &format!("{tree}:{path}")]);
        assert_eq!(show("yours.txt"), "one");
        assert_eq!(show("both.txt"), "a\nb\nc\nd\ne\nf\ng\nh, by the run");
        assert_eq!(show("new.txt"), "new");
    }

    #[tokio::test]
    async fn lines_the_run_rewrote_over_your_edit_are_the_runs() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        repo.write("both.txt", "a\nb\nc\n");
        let main = git.commit_all(&repo.root, "files").await.unwrap().unwrap();
        repo.write("both.txt", "a, edited\nb\nc\n");
        let snapshot = git
            .commit_all(&repo.root, "snapshot")
            .await
            .unwrap()
            .unwrap();
        repo.write("both.txt", "a, edited and finished\nb\nc\n");
        let head = git
            .commit_all(&repo.root, "the run")
            .await
            .unwrap()
            .unwrap();
        let scratch = repo.root.join(".git/scratch.index");

        let tree = git
            .tree_without_yours(&repo.root, &scratch, &snapshot, &main, &head)
            .await
            .unwrap();
        assert_eq!(
            repo.git(&["show", &format!("{tree}:both.txt")]),
            "a, edited and finished\nb\nc"
        );
    }

    #[tokio::test]
    async fn your_index_follows_the_commit_and_your_edits_stay_uncommitted() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        let (main, snapshot, head) = started_from_your_edits(&repo, &git).await;
        let scratch = repo.root.join(".git/scratch.index");
        let tree = git
            .tree_without_yours(&repo.root, &scratch, &snapshot, &main, &head)
            .await
            .unwrap();
        let commit = git
            .commit_tree(&repo.root, &tree, &main, "the run's work")
            .await
            .unwrap();
        // You took your edit to both.txt back while the run worked.
        repo.write("both.txt", "a\nb\nc\nd\ne\nf\ng\nh\n");
        git.write_files(
            &repo.root,
            &scratch,
            &snapshot,
            &head,
            &["both.txt".to_owned()],
        )
        .await
        .unwrap();
        git.update_branch(&repo.root, "main", &commit, &main)
            .await
            .unwrap();

        for _ in 0..2 {
            git.follow_beside(&repo.root, &scratch, &snapshot, &main, &commit)
                .await
                .unwrap();
            assert_eq!(repo.git(&["status", "--porcelain"]), "M yours.txt");
            assert_eq!(
                std::fs::read_to_string(repo.root.join("both.txt")).unwrap(),
                "a\nb\nc\nd\ne\nf\ng\nh, by the run\n"
            );
        }
    }
}
