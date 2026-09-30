//! The git operations the conductor runs, through the git CLI with hooks, fsmonitor, and commit
//! signing turned off so nothing in the repository runs code as a side effect.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use argonauts_core::DiffStat;
use tokio::process::Command;

const SAFE_CONFIG: [&str; 6] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "commit.gpgSign=false",
];

/// Inherited variables that would point git at another repository, index, or config.
const REPOSITORY_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_EXTERNAL_DIFF",
];

/// Identity used for conductor commits when the user has none configured.
const FALLBACK_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Argonauts"),
    ("GIT_AUTHOR_EMAIL", "argonauts@localhost"),
    ("GIT_COMMITTER_NAME", "Argonauts"),
    ("GIT_COMMITTER_EMAIL", "argonauts@localhost"),
];

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("could not run git: {0}")]
    Spawn(#[source] io::Error),
    #[error("`git {args}` failed ({status}): {stderr}")]
    Failed {
        args: String,
        status: String,
        stderr: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct Git {
    env: Vec<(String, String)>,
    bound: Option<Bound>,
}

/// A repository named explicitly instead of found from the working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Bound {
    git_dir: PathBuf,
    work_tree: PathBuf,
}

impl Git {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_env(env: Vec<(String, String)>) -> Self {
        Self { env, bound: None }
    }

    /// Git that always uses `git_dir` and `work_tree`, never the `.git` file it finds. An agent can
    /// rewrite that file to point at a repository whose config runs commands during our `git add`.
    pub fn bound_to(&self, git_dir: PathBuf, work_tree: PathBuf) -> Self {
        Self {
            env: self.env.clone(),
            bound: Some(Bound { git_dir, work_tree }),
        }
    }

    pub async fn absolute_git_dir(&self, dir: &Path) -> Result<PathBuf, GitError> {
        let out = self.run(dir, &["rev-parse", "--absolute-git-dir"]).await?;
        Ok(PathBuf::from(out))
    }

    pub async fn toplevel(&self, dir: &Path) -> Result<PathBuf, GitError> {
        let out = self.run(dir, &["rev-parse", "--show-toplevel"]).await?;
        Ok(PathBuf::from(out))
    }

    /// `None` when HEAD is detached.
    pub async fn current_branch(&self, dir: &Path) -> Result<Option<String>, GitError> {
        let args = ["symbolic-ref", "--quiet", "--short", "HEAD"];
        self.optional(dir, &args).await
    }

    /// `None` in a repository with no commits yet.
    pub async fn head_commit(&self, dir: &Path) -> Result<Option<String>, GitError> {
        let args = ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"];
        self.optional(dir, &args).await
    }

    pub async fn resolve(&self, dir: &Path, rev: &str) -> Result<String, GitError> {
        let spec = format!("{rev}^{{commit}}");
        self.run(dir, &["rev-parse", "--verify", &spec]).await
    }

    /// `git status --porcelain` lines for tracked files with uncommitted changes, staged or not.
    pub async fn tracked_changes(&self, dir: &Path) -> Result<Vec<String>, GitError> {
        let args = [
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ];
        let out = self.run(dir, &args).await?;
        Ok(out.lines().map(str::to_owned).collect())
    }

    pub async fn add_worktree(
        &self,
        repo: &Path,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<(), GitError> {
        let path = path.to_string_lossy();
        let args = ["worktree", "add", "--quiet", "-b", branch, &path, base];
        self.run(repo, &args).await.map(drop)
    }

    /// Discards anything in the worktree that wasn't committed.
    pub async fn remove_worktree(&self, repo: &Path, path: &Path) -> Result<(), GitError> {
        let path = path.to_string_lossy();
        self.run(repo, &["worktree", "remove", "--force", &path])
            .await
            .map(drop)
    }

    pub async fn delete_branch(&self, repo: &Path, branch: &str) -> Result<(), GitError> {
        self.run(repo, &["branch", "-D", branch]).await.map(drop)
    }

    /// Stages everything in `worktree` and commits it. `None` when there was nothing to commit.
    pub async fn commit_all(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<Option<String>, GitError> {
        self.run(worktree, &["add", "--all"]).await?;
        let staged = ["diff", "--cached", "--quiet", "--no-ext-diff"];
        let out = self.output(worktree, &staged, &[]).await?;
        match out.status.code() {
            Some(0) => return Ok(None),
            Some(1) => {}
            _ => return Err(failed(&staged, &out)),
        }
        let identity = self.identity_env(worktree).await;
        let commit = [
            "commit",
            "--quiet",
            "--no-verify",
            "--cleanup=whitespace",
            "-m",
            message,
        ];
        let out = self.output(worktree, &commit, &identity).await?;
        if !out.status.success() {
            return Err(failed(&commit, &out));
        }
        self.resolve(worktree, "HEAD").await.map(Some)
    }

    pub async fn diff_stat(&self, dir: &Path, from: &str, to: &str) -> Result<DiffStat, GitError> {
        let args = [
            "diff",
            "--numstat",
            "--no-ext-diff",
            "--no-textconv",
            from,
            to,
        ];
        let out = self.run(dir, &args).await?;
        Ok(parse_numstat(&out))
    }

    pub async fn diff(&self, dir: &Path, from: &str, to: &str) -> Result<String, GitError> {
        let args = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            from,
            to,
        ];
        let out = self.output(dir, &args, &[]).await?;
        if !out.status.success() {
            return Err(failed(&args, &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn changed_paths(
        &self,
        dir: &Path,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        let args = ["diff", "--name-only", "-z", "--no-ext-diff", from, to];
        let out = self.run(dir, &args).await?;
        Ok(out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Changed and untracked files under `paths`, ignored ones included, as porcelain lines.
    pub async fn status_of(&self, dir: &Path, paths: &[&str]) -> Result<Vec<String>, GitError> {
        let mut args = vec![
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
        ];
        args.extend_from_slice(paths);
        let out = self.run(dir, &args).await?;
        Ok(out.lines().map(str::to_owned).collect())
    }

    /// A failed merge is aborted, so the working tree is left as it was.
    pub async fn merge(&self, repo: &Path, rev: &str, message: &str) -> Result<(), GitError> {
        let identity = self.identity_env(repo).await;
        let args = ["merge", "--no-edit", "--quiet", "-m", message, rev];
        let out = self.output(repo, &args, &identity).await?;
        if out.status.success() {
            return Ok(());
        }
        // Undo a half-done merge. When nothing was started this fails harmlessly.
        let _ = self.output(repo, &["merge", "--abort"], &[]).await;
        Err(failed(&args, &out))
    }

    pub async fn version(&self) -> Result<String, GitError> {
        let out = self.run(Path::new("."), &["--version"]).await?;
        Ok(argonauts_core::extract_version(&out).unwrap_or(out))
    }

    async fn identity_env(&self, dir: &Path) -> Vec<(&'static str, &'static str)> {
        for var in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
            match self.output(dir, &["var", var], &[]).await {
                Ok(out) if out.status.success() => {}
                _ => return FALLBACK_IDENTITY.to_vec(),
            }
        }
        Vec::new()
    }

    /// Trimmed stdout, or `None` when git exits with status 1.
    async fn optional(&self, dir: &Path, args: &[&str]) -> Result<Option<String>, GitError> {
        let out = self.output(dir, args, &[]).await?;
        match out.status.code() {
            Some(0) => Ok(Some(stdout_trimmed(&out))),
            Some(1) => Ok(None),
            _ => Err(failed(args, &out)),
        }
    }

    async fn run(&self, dir: &Path, args: &[&str]) -> Result<String, GitError> {
        let out = self.output(dir, args, &[]).await?;
        if out.status.success() {
            Ok(stdout_trimmed(&out))
        } else {
            Err(failed(args, &out))
        }
    }

    async fn output(
        &self,
        dir: &Path,
        args: &[&str],
        extra_env: &[(&str, &str)],
    ) -> Result<Output, GitError> {
        let mut command = Command::new("git");
        command.arg("-C").arg(dir);
        if let Some(bound) = &self.bound {
            command
                .arg(flag_with_path("--git-dir=", &bound.git_dir))
                .arg(flag_with_path("--work-tree=", &bound.work_tree));
        }
        command
            .args(SAFE_CONFIG)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .kill_on_drop(true);
        for name in REPOSITORY_ENV {
            command.env_remove(name);
        }
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        command.envs(extra_env.iter().copied());
        command.output().await.map_err(GitError::Spawn)
    }
}

fn flag_with_path(flag: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(flag);
    arg.push(path);
    arg
}

fn stdout_trimmed(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_owned()
}

fn failed(args: &[&str], out: &Output) -> GitError {
    let stderr = String::from_utf8_lossy(&out.stderr);
    GitError::Failed {
        args: args.join(" "),
        status: out.status.to_string(),
        stderr: stderr.trim().lines().last().unwrap_or("").to_owned(),
    }
}

/// Sums `git diff --numstat` output. Binary files count as changed with no line counts.
fn parse_numstat(text: &str) -> DiffStat {
    let mut stat = DiffStat::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.split('\t');
        let mut count = || {
            fields
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0)
        };
        let (added, deleted) = (count(), count());
        stat.files_changed = stat.files_changed.saturating_add(1);
        stat.insertions = stat.insertions.saturating_add(added);
        stat.deletions = stat.deletions.saturating_add(deleted);
    }
    stat
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    fn git() -> Git {
        Git::with_env(git_test_env())
    }

    #[test]
    fn numstat_sums_lines_and_counts_binary_files() {
        let text = "3\t1\tsrc/a.rs\n-\t-\tlogo.png\n10\t0\tdocs/b => c.md\n";
        let stat = parse_numstat(text);
        assert_eq!(
            stat,
            DiffStat {
                files_changed: 3,
                insertions: 13,
                deletions: 1
            }
        );
        assert_eq!(parse_numstat(""), DiffStat::default());
    }

    #[tokio::test]
    async fn reads_repository_state() {
        let repo = TestRepo::new();
        let git = git();
        let nested = repo.root.join("sub");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(git.toplevel(&nested).await.unwrap(), repo.root);
        assert_eq!(
            git.current_branch(&repo.root).await.unwrap().as_deref(),
            Some("main")
        );
        let head = git.head_commit(&repo.root).await.unwrap().unwrap();
        assert_eq!(head.len(), 40);
        assert_eq!(git.resolve(&repo.root, "main").await.unwrap(), head);

        repo.git(&["checkout", "--quiet", "--detach"]);
        assert_eq!(git.current_branch(&repo.root).await.unwrap(), None);
        assert!(git.version().await.unwrap().starts_with('2'));
    }

    #[tokio::test]
    async fn empty_repositories_have_no_head() {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .envs(git_test_env())
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(git().head_commit(dir.path()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn outside_a_repository_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = git().toplevel(dir.path()).await.unwrap_err();
        assert!(matches!(err, GitError::Failed { .. }), "{err}");
    }

    #[tokio::test]
    async fn tracked_changes_ignore_untracked_files() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("notes.txt", "untracked");
        assert!(git.tracked_changes(&repo.root).await.unwrap().is_empty());
        repo.write("README.md", "changed");
        assert_eq!(
            git.tracked_changes(&repo.root).await.unwrap(),
            [" M README.md"]
        );
    }

    #[tokio::test]
    async fn worktree_commit_diff_and_merge() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let tree = repo.dir.path().join("trees/run/card");
        git.add_worktree(&repo.root, &tree, "argonauts/run/card", &base)
            .await
            .unwrap();
        assert_eq!(
            git.current_branch(&tree).await.unwrap().as_deref(),
            Some("argonauts/run/card")
        );
        assert_eq!(git.commit_all(&tree, "nothing").await.unwrap(), None);

        std::fs::write(tree.join("hello.txt"), "hi\n").unwrap();
        std::fs::write(tree.join("README.md"), "# Test\nmore\n").unwrap();
        let sha = git
            .commit_all(&tree, "# Add hello\n\nBody")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repo.git(&["log", "-1", "--format=%s", &sha]), "# Add hello");

        let stat = git.diff_stat(&tree, &base, &sha).await.unwrap();
        assert_eq!(stat.files_changed, 2);
        assert_eq!(stat.insertions, 2);
        assert_eq!(
            git.changed_paths(&tree, &base, &sha).await.unwrap(),
            ["README.md", "hello.txt"]
        );
        assert!(git.diff(&tree, &base, &sha).await.unwrap().contains("+hi"));

        git.merge(&repo.root, &sha, "Merge the card").await.unwrap();
        assert_eq!(git.head_commit(&repo.root).await.unwrap().unwrap(), sha);
        assert!(repo.root.join("hello.txt").exists());

        git.remove_worktree(&repo.root, &tree).await.unwrap();
        assert!(!tree.exists());
        git.delete_branch(&repo.root, "argonauts/run/card")
            .await
            .unwrap();
        assert!(repo.git(&["branch", "--list", "argonauts/*"]).is_empty());
    }

    #[tokio::test]
    async fn a_conflicting_merge_is_aborted() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let tree = repo.dir.path().join("tree");
        git.add_worktree(&repo.root, &tree, "card", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("README.md"), "theirs\n").unwrap();
        git.commit_all(&tree, "theirs").await.unwrap();
        repo.write("README.md", "ours\n");
        repo.git(&["commit", "--quiet", "-am", "ours"]);

        let err = git
            .merge(&repo.root, "card", "Merge card")
            .await
            .unwrap_err();
        assert!(matches!(err, GitError::Failed { .. }));
        assert!(git.tracked_changes(&repo.root).await.unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(repo.root.join("README.md")).unwrap(),
            "ours\n"
        );
    }

    #[tokio::test]
    async fn hooks_never_run() {
        let repo = TestRepo::new();
        let marker = repo.dir.path().join("hook-ran");
        let hook = repo.root.join(".git/hooks/pre-commit");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        make_executable(&hook);
        repo.write("x.txt", "x");
        let sha = git().commit_all(&repo.root, "x").await.unwrap();
        assert!(sha.is_some());
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn missing_identity_falls_back_to_argonauts() {
        let repo = TestRepo::new();
        repo.git(&["config", "--unset", "user.name"]);
        repo.git(&["config", "--unset", "user.email"]);
        repo.git(&["config", "user.useConfigOnly", "true"]);
        repo.write("x.txt", "x");
        let sha = git().commit_all(&repo.root, "x").await.unwrap().unwrap();
        assert_eq!(
            repo.git(&["log", "-1", "--format=%an <%ae>", &sha]),
            "Argonauts <argonauts@localhost>"
        );
    }

    #[tokio::test]
    async fn a_rewritten_dot_git_file_is_ignored_when_bound() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let tree = repo.dir.path().join("tree");
        git.add_worktree(&repo.root, &tree, "card", &base)
            .await
            .unwrap();
        let git_dir = git.absolute_git_dir(&tree).await.unwrap();
        assert!(
            git_dir.starts_with(repo.root.join(".git/worktrees")),
            "{git_dir:?}"
        );
        let bound = git.bound_to(git_dir, tree.clone());

        // A fake repository whose config would run a command on `git add`.
        let marker = repo.dir.path().join("filter-ran");
        let fake = tree.join(".fake");
        std::fs::create_dir_all(fake.join("objects")).unwrap();
        std::fs::create_dir_all(fake.join("refs/heads")).unwrap();
        std::fs::write(fake.join("HEAD"), "ref: refs/heads/card\n").unwrap();
        std::fs::write(
            fake.join("config"),
            format!(
                "[core]\n\trepositoryformatversion = 0\n[filter \"x\"]\n\tclean = touch '{}'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::write(tree.join(".git"), "gitdir: .fake\n").unwrap();
        std::fs::write(tree.join(".gitattributes"), "* filter=x\n").unwrap();
        std::fs::write(tree.join("hello.txt"), "hi\n").unwrap();

        assert_eq!(
            bound.current_branch(&tree).await.unwrap().as_deref(),
            Some("card")
        );
        let sha = bound.commit_all(&tree, "hello").await.unwrap().unwrap();
        assert!(!marker.exists(), "the fake repository's filter ran");
        assert_eq!(git.resolve(&repo.root, "card").await.unwrap(), sha);
    }

    #[tokio::test]
    async fn status_of_sees_untracked_and_ignored_files() {
        let repo = TestRepo::new();
        let git = git();
        repo.write(".gitignore", ".claude/settings.local.json\n");
        repo.git(&["add", ".gitignore"]);
        repo.git(&["commit", "--quiet", "-m", "ignore"]);
        assert!(
            git.status_of(&repo.root, &[".claude"])
                .await
                .unwrap()
                .is_empty()
        );
        repo.write(".claude/settings.local.json", "{}");
        repo.write("other.txt", "x");
        let changed = git
            .status_of(&repo.root, &[".claude", ".mcp.json"])
            .await
            .unwrap();
        assert_eq!(changed, ["!! .claude/settings.local.json"]);
    }

    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
