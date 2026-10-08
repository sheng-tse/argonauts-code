//! Tags, notes, resets, and fast-forwards: the git side of handoff checkpoints, authorship
//! notes, `/rewind`, and landing.

use std::path::Path;

use super::{Git, GitError, NOTES_REF, failed};

/// A refused merge or checkout, in git's own words. Its last line is only "Aborting", and its
/// first error says why.
fn failed_at_first_error(args: &[&str], out: &std::process::Output) -> GitError {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let why = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("error:") || line.starts_with("fatal:"))
        .or_else(|| stderr.lines().map(str::trim).rfind(|line| !line.is_empty()))
        .unwrap_or("")
        .to_owned();
    GitError::Failed {
        args: args.join(" "),
        status: out.status.to_string(),
        stderr: why,
    }
}

impl Git {
    /// Moves the checked-out branch to `rev` when that is a fast-forward. False when it isn't.
    pub async fn fast_forward(&self, worktree: &Path, rev: &str) -> Result<bool, GitError> {
        let target = self.resolve(worktree, rev).await?;
        if let Some(head) = self.head_commit(worktree).await?
            && !self.is_ancestor(worktree, &head, &target).await?
        {
            return Ok(false);
        }
        let args = ["merge", "--ff-only", "--quiet", &target];
        let out = self.output(worktree, &args, &[]).await?;
        if out.status.success() {
            return Ok(true);
        }
        Err(failed_at_first_error(&args, &out))
    }

    pub async fn is_ancestor(
        &self,
        dir: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, GitError> {
        let args = ["merge-base", "--is-ancestor", ancestor, descendant];
        let out = self.output(dir, &args, &[]).await?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(failed(&args, &out)),
        }
    }

    /// The best common ancestor of two commits, or `None` when their histories are unrelated.
    pub async fn merge_base(
        &self,
        dir: &Path,
        a: &str,
        b: &str,
    ) -> Result<Option<String>, GitError> {
        self.optional(dir, &["merge-base", a, b]).await
    }

    /// The tree SHA of `rev`, which evidence binds approvals to.
    pub async fn tree_of(&self, dir: &Path, rev: &str) -> Result<String, GitError> {
        let spec = format!("{rev}^{{tree}}");
        self.run(dir, &["rev-parse", "--verify", &spec]).await
    }

    /// A commit of `tree` on top of `parent`, made without a worktree or an index. No branch
    /// points at it until the caller moves one.
    pub async fn commit_tree(
        &self,
        dir: &Path,
        tree: &str,
        parent: &str,
        message: &str,
    ) -> Result<String, GitError> {
        let identity = self.identity_env(dir).await;
        let args = ["commit-tree", tree, "-p", parent, "-m", message];
        let out = self.output(dir, &args, &identity).await?;
        if !out.status.success() {
            return Err(failed(&args, &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Writes into the files of `checkout` what changed from `base` to `rev`, and moves
    /// nothing else: no branch, and not your index, since git works out the change in an
    /// index of its own at `scratch`. Like a fast-forward it is all or nothing, and refuses
    /// while a file of yours differs from `base` where the change would write, unless it is
    /// one of `holding`, which hold what `rev` has there already and are left as they are.
    pub async fn write_files(
        &self,
        checkout: &Path,
        scratch: &Path,
        base: &str,
        rev: &str,
        holding: &[String],
    ) -> Result<(), GitError> {
        let index = scratch.to_string_lossy();
        let with_index = [("GIT_INDEX_FILE", index.as_ref())];
        let written = async {
            self.in_index(checkout, &["read-tree", base], &with_index)
                .await?;
            // Files that still match `base` count as untouched only once git has seen them.
            // It exits 1 for each file of yours that differs, which the merge judges.
            let refresh = ["update-index", "-q", "--refresh"];
            self.output(checkout, &refresh, &with_index).await?;
            self.take_as_they_are(checkout, rev, holding, &with_index)
                .await?;
            let merge = ["read-tree", "-m", "-u", base, rev];
            self.in_index(checkout, &merge, &with_index).await
        }
        .await;
        if let Err(err) = std::fs::remove_file(scratch)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("removing {}: {err}", scratch.display());
        }
        written
    }

    /// Runs `args` on the index `with_index` names, and says what git refused in its words.
    pub(super) async fn in_index(
        &self,
        checkout: &Path,
        args: &[&str],
        with_index: &[(&str, &str)],
    ) -> Result<(), GitError> {
        let out = self.output(checkout, args, with_index).await?;
        if out.status.success() {
            Ok(())
        } else {
            Err(failed_at_first_error(args, &out))
        }
    }

    /// Your files in `checkout` that no longer hold what `base` has, changed or gone, of those
    /// the change from `base` to `rev` writes. It's what [`Self::write_files`] refuses over,
    /// found the way it works: the files are compared with `base` in an index of git's own at
    /// `scratch`, so it holds in a checkout whose own index never held `base`.
    pub async fn differing_in_the_way(
        &self,
        checkout: &Path,
        scratch: &Path,
        base: &str,
        rev: &str,
    ) -> Result<Vec<String>, GitError> {
        let index = scratch.to_string_lossy();
        let with_index = [("GIT_INDEX_FILE", index.as_ref())];
        let differing = async {
            let read = ["read-tree", base];
            let out = self.output(checkout, &read, &with_index).await?;
            if !out.status.success() {
                return Err(failed(&read, &out));
            }
            // Exits 1 for each file that differs, which the next step lists.
            let refresh = ["update-index", "-q", "--refresh"];
            self.output(checkout, &refresh, &with_index).await?;
            let list = ["diff-files", "--name-only", "--no-renames", "-z"];
            let out = self.output(checkout, &list, &with_index).await?;
            if !out.status.success() {
                return Err(failed(&list, &out));
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        .await;
        if let Err(err) = std::fs::remove_file(scratch)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("removing {}: {err}", scratch.display());
        }
        let differing = differing?;
        // Both sides of a move, since the old name is written too: it goes away.
        let written = [
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "-z",
            base,
            rev,
        ];
        let written = self.run(checkout, &written).await?;
        let written: std::collections::BTreeSet<&str> = written.split('\0').collect();
        Ok(differing
            .split('\0')
            .filter(|path| !path.is_empty() && written.contains(path))
            .map(str::to_owned)
            .collect())
    }

    /// Whether `checkout` already holds the change from `base` to `rev`: every file it writes
    /// has `rev`'s bytes, and every file it removes is gone. So a landing as files that was
    /// cut short after the writing isn't taken for your own edits in its way.
    pub async fn holds_change(
        &self,
        checkout: &Path,
        scratch: &Path,
        base: &str,
        rev: &str,
    ) -> Result<bool, GitError> {
        let unwritten = self
            .differing_in_the_way(checkout, scratch, rev, base)
            .await?;
        if !unwritten.is_empty() {
            return Ok(false);
        }
        let removed = [
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "-z",
            "--diff-filter=D",
            base,
            rev,
        ];
        let removed = self.run(checkout, &removed).await?;
        Ok(removed
            .split('\0')
            .filter(|path| !path.is_empty())
            .all(|path| std::fs::symlink_metadata(checkout.join(path)).is_err()))
    }

    /// The commit the ref `name` points at, such as `refs/argonauts/run-01/base`. `None` when
    /// there's no such ref.
    pub async fn ref_commit(&self, dir: &Path, name: &str) -> Result<Option<String>, GitError> {
        let spec = format!("{name}^{{commit}}");
        self.optional(dir, &["rev-parse", "--verify", "--quiet", &spec])
            .await
    }

    /// An annotated tag whose message records the handoff. Tagging the same commit again is a
    /// no-op, so a step re-entered after a crash doesn't fail; a tag on another commit is an
    /// error, since a checkpoint never moves.
    pub async fn tag(
        &self,
        dir: &Path,
        name: &str,
        rev: &str,
        message: &str,
    ) -> Result<(), GitError> {
        let commit = self.resolve(dir, rev).await?;
        if let Some(existing) = self.tag_target(dir, name).await? {
            if existing == commit {
                return Ok(());
            }
            return Err(GitError::Failed {
                args: format!("tag {name}"),
                status: "refused".to_owned(),
                stderr: format!("tag {name} already points at {existing}"),
            });
        }
        let identity = self.identity_env(dir).await;
        let args = [
            "tag",
            "--annotate",
            "--no-sign",
            "-m",
            message,
            name,
            &commit,
        ];
        let out = self.output(dir, &args, &identity).await?;
        if out.status.success() {
            Ok(())
        } else {
            Err(failed(&args, &out))
        }
    }

    /// The commit a tag points at, or `None` when there is no such tag.
    pub async fn tag_target(&self, dir: &Path, name: &str) -> Result<Option<String>, GitError> {
        let spec = format!("refs/tags/{name}^{{commit}}");
        self.optional(dir, &["rev-parse", "--verify", "--quiet", &spec])
            .await
    }

    /// Adds or replaces the note on `rev` under [`NOTES_REF`].
    pub async fn add_note(&self, dir: &Path, rev: &str, text: &str) -> Result<(), GitError> {
        let identity = self.identity_env(dir).await;
        let notes_ref = format!("--ref={NOTES_REF}");
        let args = ["notes", &notes_ref, "add", "--force", "-m", text, rev];
        let out = self.output(dir, &args, &identity).await?;
        if out.status.success() {
            Ok(())
        } else {
            Err(failed(&args, &out))
        }
    }

    pub async fn note(&self, dir: &Path, rev: &str) -> Result<Option<String>, GitError> {
        let notes_ref = format!("--ref={NOTES_REF}");
        self.optional(dir, &["notes", &notes_ref, "show", rev])
            .await
    }

    pub async fn reset_hard(&self, worktree: &Path, rev: &str) -> Result<(), GitError> {
        let commit = self.resolve(worktree, rev).await?;
        self.run(worktree, &["reset", "--hard", "--quiet", &commit])
            .await
            .map(drop)
    }

    /// Points `branch` at `new` only while it still points at `old`, for a branch that isn't
    /// checked out. Git refuses when someone moved it in between.
    pub async fn update_branch(
        &self,
        dir: &Path,
        branch: &str,
        new: &str,
        old: &str,
    ) -> Result<(), GitError> {
        let reference = format!("refs/heads/{branch}");
        self.run(dir, &["update-ref", &reference, new, old])
            .await
            .map(drop)
    }

    /// The names of the repository's remotes.
    pub async fn remotes(&self, dir: &Path) -> Result<Vec<String>, GitError> {
        let out = self.run(dir, &["remote"]).await?;
        Ok(out.lines().map(str::to_owned).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    fn git() -> Git {
        Git::with_env(git_test_env())
    }

    fn commit(repo: &TestRepo, file: &str, text: &str) -> String {
        repo.write(file, text);
        repo.git(&["add", file]);
        repo.git(&["commit", "--quiet", "-m", file]);
        repo.git(&["rev-parse", "HEAD"])
    }

    #[tokio::test]
    async fn ancestry_and_merge_base() {
        let repo = TestRepo::new();
        let git = git();
        let first = repo.git(&["rev-parse", "HEAD"]);
        let second = commit(&repo, "a.txt", "a");
        assert!(git.is_ancestor(&repo.root, &first, &second).await.unwrap());
        assert!(!git.is_ancestor(&repo.root, &second, &first).await.unwrap());
        assert!(git.is_ancestor(&repo.root, &second, &second).await.unwrap());

        repo.git(&["checkout", "--quiet", "-b", "side", &first]);
        let side = commit(&repo, "b.txt", "b");
        assert_eq!(
            git.merge_base(&repo.root, &second, &side).await.unwrap(),
            Some(first)
        );
        let err = git.is_ancestor(&repo.root, "nope", &side).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn fast_forward_only_when_possible() {
        let repo = TestRepo::new();
        let git = git();
        let first = repo.git(&["rev-parse", "HEAD"]);
        let ahead = commit(&repo, "a.txt", "a");
        repo.git(&["reset", "--hard", "--quiet", &first]);

        assert!(git.fast_forward(&repo.root, &ahead).await.unwrap());
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), ahead);
        assert!(repo.root.join("a.txt").exists());

        repo.git(&["checkout", "--quiet", "-b", "side", &first]);
        commit(&repo, "b.txt", "b");
        assert!(!git.fast_forward(&repo.root, &ahead).await.unwrap());
        assert!(!repo.root.join("a.txt").exists());
    }

    #[tokio::test]
    async fn tree_of_matches_git() {
        let repo = TestRepo::new();
        let tree = git().tree_of(&repo.root, "HEAD").await.unwrap();
        assert_eq!(tree, repo.git(&["rev-parse", "HEAD^{tree}"]));
        assert_eq!(tree.len(), 40);
    }

    #[tokio::test]
    async fn tags_are_annotated_and_never_move() {
        let repo = TestRepo::new();
        let git = git();
        let first = repo.git(&["rev-parse", "HEAD"]);
        let name = "argonauts/run-01/task-001/fallback-1";
        assert_eq!(git.tag_target(&repo.root, name).await.unwrap(), None);

        git.tag(
            &repo.root,
            name,
            "HEAD",
            "from: claude-code/opus\nreason: usage limit",
        )
        .await
        .unwrap();
        assert_eq!(
            git.tag_target(&repo.root, name).await.unwrap(),
            Some(first.clone())
        );
        assert_eq!(repo.git(&["cat-file", "-t", name]), "tag");
        let message = repo.git(&["tag", "-l", "--format=%(contents)", name]);
        assert!(message.contains("reason: usage limit"), "{message}");

        // The branch moves on; the tag stays, and tagging the same commit again is fine.
        let second = commit(&repo, "a.txt", "a");
        git.tag(&repo.root, name, &first, "again").await.unwrap();
        assert!(git.tag(&repo.root, name, &second, "moved").await.is_err());
        assert_eq!(git.tag_target(&repo.root, name).await.unwrap(), Some(first));
    }

    #[tokio::test]
    async fn notes_are_added_replaced_and_read() {
        let repo = TestRepo::new();
        let git = git();
        assert_eq!(git.note(&repo.root, "HEAD").await.unwrap(), None);
        git.add_note(&repo.root, "HEAD", "by codex/gpt-5.6")
            .await
            .unwrap();
        git.add_note(&repo.root, "HEAD", "by agy/gemini-3.6")
            .await
            .unwrap();
        assert_eq!(
            git.note(&repo.root, "HEAD").await.unwrap().as_deref(),
            Some("by agy/gemini-3.6")
        );
        // Notes live under their own ref, so the default notes and the message stay clean.
        assert!(!repo.git(&["for-each-ref", NOTES_REF]).is_empty());
        assert_eq!(repo.git(&["log", "-1", "--format=%B"]), "init");
    }

    #[tokio::test]
    async fn reset_hard_discards_later_work() {
        let repo = TestRepo::new();
        let git = git();
        let first = repo.git(&["rev-parse", "HEAD"]);
        commit(&repo, "a.txt", "a");
        repo.write("README.md", "dirty\n");
        git.reset_hard(&repo.root, &first).await.unwrap();
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), first);
        assert!(!repo.root.join("a.txt").exists());
        assert!(git.tracked_changes(&repo.root).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_branch_checks_the_old_value() {
        let repo = TestRepo::new();
        let git = git();
        let first = repo.git(&["rev-parse", "HEAD"]);
        let second = commit(&repo, "a.txt", "a");
        repo.git(&["branch", "target", &first]);

        assert!(
            git.update_branch(&repo.root, "target", &second, &second)
                .await
                .is_err()
        );
        git.update_branch(&repo.root, "target", &second, &first)
            .await
            .unwrap();
        assert_eq!(repo.git(&["rev-parse", "target"]), second);

        // An empty old value creates a branch, and never overwrites one.
        git.update_branch(&repo.root, "kept", &first, "")
            .await
            .unwrap();
        assert!(
            git.update_branch(&repo.root, "kept", &second, "")
                .await
                .is_err()
        );
        assert_eq!(repo.git(&["rev-parse", "kept"]), first);
    }

    #[tokio::test]
    async fn remotes_are_listed() {
        let repo = TestRepo::new();
        let git = git();
        assert!(git.remotes(&repo.root).await.unwrap().is_empty());
        repo.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
        assert_eq!(git.remotes(&repo.root).await.unwrap(), ["origin"]);
    }
}
