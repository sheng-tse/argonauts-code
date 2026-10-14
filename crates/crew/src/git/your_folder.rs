//! Git for writing a run into a folder that has no git of its own, where the history
//! Argonauts Code keeps follows the folder. What you changed while the run worked can't be
//! committed or stashed there, so it's recorded as a commit of that history, and git merges it
//! with the run's work the way it merges two branches. Only a file both changed in the same
//! lines is left for you to pick a version of.

use std::path::Path;

use super::{FALLBACK_IDENTITY, Git, GitError, failed};

/// Your folder merged with a run's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMerge {
    /// The merged tree. A file in `both_changed` holds git's conflict markers there until
    /// [`Git::tree_with`] puts one version in.
    pub tree: String,
    /// The files you and the run changed in the same lines.
    pub both_changed: Vec<String>,
}

impl Git {
    /// Your folder as it is now, as a commit on top of `parent` that no branch points at. It's
    /// built in the index at `scratch`, so the history's own index and branch stay as they are
    /// until you've answered. It takes what a record takes: every file your ignore rules and
    /// the leave-out list allow.
    pub async fn folder_as_commit(
        &self,
        folder: &Path,
        scratch: &Path,
        parent: &str,
    ) -> Result<String, GitError> {
        let git = self.with_index(scratch);
        let built = async {
            git.run(folder, &["read-tree", parent]).await?;
            git.run(folder, &["add", "--all"]).await?;
            let tree = git.run(folder, &["write-tree"]).await?;
            let message = "Your folder while a run waited to be written in";
            let args = ["commit-tree", &tree, "-p", parent, "-m", message];
            let out = git.output(folder, &args, &FALLBACK_IDENTITY).await?;
            if !out.status.success() {
                return Err(failed(&args, &out));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
        .await;
        if let Err(err) = std::fs::remove_file(scratch)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("removing {}: {err}", scratch.display());
        }
        built
    }

    /// Merges the commits `yours` and `runs` from where they parted, touching no file, index
    /// or branch. Needs git 2.38, which `merge-tree --write-tree` came with.
    pub async fn merge_folder(
        &self,
        dir: &Path,
        yours: &str,
        runs: &str,
    ) -> Result<FolderMerge, GitError> {
        self.merge_tree(dir, &[], yours, runs).await
    }

    /// `git merge-tree` of `yours` and `runs` with `options`: the merged tree, and the files
    /// it couldn't merge.
    pub(super) async fn merge_tree(
        &self,
        dir: &Path,
        options: &[&str],
        yours: &str,
        runs: &str,
    ) -> Result<FolderMerge, GitError> {
        let mut args = vec!["merge-tree", "--write-tree", "--name-only", "-z"];
        args.extend(options);
        args.extend([yours, runs]);
        let out = self.output(dir, &args, &[]).await?;
        // 1 says the merge has conflicts, and still names the tree.
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(failed(&args, &out));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.split('\0');
        let tree = parts.next().unwrap_or_default().trim().to_owned();
        if tree.is_empty() {
            return Err(failed(&args, &out));
        }
        // The names end at an empty one, and git's own remarks follow.
        let mut both_changed: Vec<String> = parts
            .take_while(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        both_changed.sort();
        both_changed.dedup();
        Ok(FolderMerge { tree, both_changed })
    }

    /// `tree` with each of `paths` as `rev` has it: its version of the file, or no file where
    /// it has none. Built in the index at `scratch`.
    pub async fn tree_with(
        &self,
        dir: &Path,
        scratch: &Path,
        tree: &str,
        rev: &str,
        paths: &[String],
    ) -> Result<String, GitError> {
        let git = self.with_index(scratch);
        let built = async {
            git.run(dir, &["read-tree", tree]).await?;
            let names: Vec<&str> = paths.iter().map(String::as_str).collect();
            let entries = self.entries(dir, rev, &names).await?;
            for path in paths {
                match entries.get(path) {
                    Some(entry) => {
                        let info = format!("{},{},{path}", entry.mode, entry.object);
                        git.run(dir, &["update-index", "--add", "--cacheinfo", &info])
                            .await?;
                    }
                    None => {
                        git.run(dir, &["update-index", "--force-remove", "--", path])
                            .await?;
                    }
                }
            }
            git.run(dir, &["write-tree"]).await
        }
        .await;
        if let Err(err) = std::fs::remove_file(scratch)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("removing {}: {err}", scratch.display());
        }
        built
    }

    /// Moves the history's branch and index to `rev` once your folder holds its files, and
    /// writes no file. What you changed besides stays a change the next record takes.
    pub async fn follow_folder(&self, folder: &Path, rev: &str) -> Result<(), GitError> {
        self.run(folder, &["reset", "--quiet", "--mixed", rev])
            .await
            .map(drop)
    }

    /// The diff of `paths`, taken literally, from `from` to `to`.
    pub async fn diff_of(
        &self,
        dir: &Path,
        from: &str,
        to: &str,
        paths: &[String],
    ) -> Result<String, GitError> {
        let specs: Vec<String> = paths
            .iter()
            .map(|path| format!(":(literal){path}"))
            .collect();
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", from, to, "--"];
        args.extend(specs.iter().map(String::as_str));
        self.run(dir, &args).await
    }

    /// A commit of `tree` on top of `parent`, made as Argonauts Code whatever identity your
    /// git has: the history of a folder without git is its own record, and reaches no branch
    /// of yours.
    pub async fn commit_tree_as_argonauts(
        &self,
        dir: &Path,
        tree: &str,
        parent: &str,
        message: &str,
    ) -> Result<String, GitError> {
        let args = ["commit-tree", tree, "-p", parent, "-m", message];
        let out = self.output(dir, &args, &FALLBACK_IDENTITY).await?;
        if !out.status.success() {
            return Err(failed(&args, &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;

    /// A folder with no git, its hidden history holding one commit of it, and git bound to
    /// both.
    async fn folder() -> (tempfile::TempDir, PathBuf, Git, String) {
        let root = tempfile::tempdir().unwrap();
        let (folder, hidden) = (root.path().join("folder"), root.path().join("history.git"));
        std::fs::create_dir_all(folder.join("src")).unwrap();
        let made = Command::new("git")
            .args(["init", "--quiet", "--bare", "--template="])
            .arg(&hidden)
            .status()
            .unwrap();
        assert!(made.success());
        std::fs::write(hidden.join("HEAD"), "ref: refs/heads/your-folder\n").unwrap();
        std::fs::write(folder.join("src/cli.js"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        std::fs::write(folder.join("notes.md"), "todo\n").unwrap();
        let git =
            Git::with_env(crate::testutil::git_test_env()).with_hidden(folder.clone(), hidden);
        let base = record(&git, &folder).await.unwrap();
        (root, folder, git, base)
    }

    /// Records the folder on its history's branch, as the app does before a run.
    async fn record(git: &Git, folder: &Path) -> Result<String, GitError> {
        git.run(folder, &["add", "--all"]).await?;
        let args = ["commit", "--quiet", "--allow-empty", "-m", "Your folder"];
        let out = git.output(folder, &args, &FALLBACK_IDENTITY).await?;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        git.resolve(folder, "HEAD").await
    }

    /// A commit on `base` that holds `files`, as a run's work would.
    async fn run_commit(git: &Git, folder: &Path, base: &str, files: &[(&str, &str)]) -> String {
        let scratch = folder.parent().unwrap().join("run.index");
        let index = git.with_index(&scratch);
        index.run(folder, &["read-tree", base]).await.unwrap();
        for (path, text) in files {
            let blob = folder.parent().unwrap().join("blob");
            std::fs::write(&blob, text).unwrap();
            let blob = blob.to_string_lossy().into_owned();
            let object = git
                .run(folder, &["hash-object", "-w", "--", &blob])
                .await
                .unwrap();
            let info = format!("100644,{object},{path}");
            index
                .run(folder, &["update-index", "--add", "--cacheinfo", &info])
                .await
                .unwrap();
        }
        let tree = index.run(folder, &["write-tree"]).await.unwrap();
        std::fs::remove_file(&scratch).unwrap();
        git.commit_tree_as_argonauts(folder, &tree, base, "the run")
            .await
            .unwrap()
    }

    async fn show(git: &Git, folder: &Path, tree: &str, path: &str) -> String {
        let spec = format!("{tree}:{path}");
        git.output(folder, &["show", &spec], &[])
            .await
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap()
    }

    #[tokio::test]
    async fn edits_in_other_lines_merge_by_themselves() -> Result<(), GitError> {
        let (root, folder, git, base) = folder().await;
        {
            let runs = [("src/cli.js", "one\ntwo\nthree\nfour\nfive\nsix\n")];
            let run = run_commit(&git, &folder, &base, &runs).await;
            std::fs::write(
                folder.join("src/cli.js"),
                "zero\none\ntwo\nthree\nfour\nfive\n",
            )
            .unwrap();
            std::fs::write(folder.join("notes.md"), "done\n").unwrap();

            let scratch = root.path().join("yours.index");
            let yours = git.folder_as_commit(&folder, &scratch, &base).await?;
            assert!(!scratch.exists());
            // Nothing of the history moved: the snapshot hangs off its branch.
            assert_eq!(git.resolve(&folder, "HEAD").await?, base);

            let merge = git.merge_folder(&folder, &yours, &run).await?;
            assert_eq!(merge.both_changed, Vec::<String>::new());
            assert_eq!(
                show(&git, &folder, &merge.tree, "src/cli.js").await,
                "zero\none\ntwo\nthree\nfour\nfive\nsix\n"
            );
            assert_eq!(show(&git, &folder, &merge.tree, "notes.md").await, "done\n");
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_file_both_changed_in_the_same_lines_takes_the_version_you_pick()
    -> Result<(), GitError> {
        let (root, folder, git, base) = folder().await;
        {
            let runs = [
                ("src/cli.js", "one\ntwo\nTHE RUN\nfour\nfive\n"),
                ("src/due.js", "new\n"),
            ];
            let run = run_commit(&git, &folder, &base, &runs).await;
            std::fs::write(folder.join("src/cli.js"), "one\ntwo\nyours\nfour\nfive\n").unwrap();

            let scratch = root.path().join("yours.index");
            let yours = git.folder_as_commit(&folder, &scratch, &base).await?;
            let merge = git.merge_folder(&folder, &yours, &run).await?;
            assert_eq!(merge.both_changed, ["src/cli.js"]);

            let both = &merge.both_changed;
            let kept = git
                .tree_with(&folder, &scratch, &merge.tree, &yours, both)
                .await?;
            assert_eq!(
                show(&git, &folder, &kept, "src/cli.js").await,
                "one\ntwo\nyours\nfour\nfive\n"
            );
            assert_eq!(show(&git, &folder, &kept, "src/due.js").await, "new\n");
            let taken = git
                .tree_with(&folder, &scratch, &merge.tree, &run, both)
                .await?;
            assert_eq!(
                show(&git, &folder, &taken, "src/cli.js").await,
                "one\ntwo\nTHE RUN\nfour\nfive\n"
            );

            // Written in: the files first, all or nothing, then the history follows them.
            std::fs::write(folder.join("notes.md"), "mine, after the snapshot\n").unwrap();
            let written = git
                .commit_tree_as_argonauts(&folder, &taken, &yours, "Run written in")
                .await?;
            git.write_files(&folder, &scratch, &yours, &written, &[])
                .await?;
            git.follow_folder(&folder, &written).await?;
            assert_eq!(
                std::fs::read_to_string(folder.join("src/cli.js")).unwrap(),
                "one\ntwo\nTHE RUN\nfour\nfive\n"
            );
            assert_eq!(
                std::fs::read_to_string(folder.join("src/due.js")).unwrap(),
                "new\n"
            );
            assert_eq!(git.resolve(&folder, "HEAD").await?, written);
            assert_eq!(
                show(&git, &folder, &yours, "src/cli.js").await,
                "one\ntwo\nyours\nfour\nfive\n",
                "your version stays in the history"
            );
            // What you changed besides, after the snapshot, is still yours and still a change.
            assert_eq!(git.tracked_changes(&folder).await?, [" M notes.md"]);
            assert!(
                git.diff_of(&folder, &yours, &run, both)
                    .await?
                    .contains("+THE RUN"),
                "the two versions side by side"
            );
            assert!(!folder.join(".git").exists());
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_file_the_run_removed_and_you_changed_is_yours_to_pick() -> Result<(), GitError> {
        let (root, folder, git, base) = folder().await;
        {
            let scratch = root.path().join("run.index");
            let index = git.with_index(&scratch);
            index.run(&folder, &["read-tree", &base]).await?;
            index
                .run(
                    &folder,
                    &["update-index", "--force-remove", "--", "notes.md"],
                )
                .await?;
            let tree = index.run(&folder, &["write-tree"]).await?;
            std::fs::remove_file(&scratch).unwrap();
            let run = git
                .commit_tree_as_argonauts(&folder, &tree, &base, "the run")
                .await?;
            std::fs::write(folder.join("notes.md"), "todo\nmore\n").unwrap();

            let yours = git.folder_as_commit(&folder, &scratch, &base).await?;
            let merge = git.merge_folder(&folder, &yours, &run).await?;
            assert_eq!(merge.both_changed, ["notes.md"]);
            let taken = git
                .tree_with(&folder, &scratch, &merge.tree, &run, &merge.both_changed)
                .await?;
            let held = git
                .run(&folder, &["ls-tree", "-r", "--name-only", &taken])
                .await?;
            assert_eq!(held, "src/cli.js");
            Ok(())
        }
    }
}
