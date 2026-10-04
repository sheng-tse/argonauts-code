//! The git operations the conductor runs, through the git CLI with hooks, fsmonitor, and commit
//! signing turned off so nothing in the repository runs code as a side effect.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use argonauts_core::DiffStat;
use argonauts_core::build_output::BuildOutput;
use tokio::process::Command;

mod refs;

/// Paths per `git rm`, well under any command-line limit.
const PATHS_PER_CALL: usize = 200;

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
    /// A scaffold stub that hasn't been written yet.
    #[error("git {0} is not built yet")]
    NotBuilt(&'static str),
}

/// Where the author of each commit is recorded, so commit messages stay clean.
pub const NOTES_REF: &str = "refs/notes/argonauts";

/// What a merge did. A conflicted merge is aborted, so the tree is left as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The new merge commit.
    Merged(String),
    /// The paths that conflicted.
    Conflict(Vec<String>),
}

/// A merge started without committing, so checks can run before the merge commit exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedMerge {
    /// The merge waits in the index for [`Git::finish_merge`] or [`Git::abort_merge`].
    Staged,
    /// Nothing to merge: `rev` is already in the branch.
    UpToDate,
    /// The paths that conflicted. The merge was aborted unless conflicts were kept.
    Conflict(Vec<String>),
}

/// What a commit of everything in a worktree did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// `None` when there was nothing to commit.
    pub sha: Option<String>,
    /// Untracked build output left in the tree, uncommitted.
    pub left_out: Vec<LeftOut>,
}

/// An untracked file a commit left out as build output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub path: String,
    /// What made it build output: the folder it's in, ending in `/`, or the file itself.
    pub because: String,
}

/// Makes `dir` an empty repository of its own, with no template and so no hooks, unless it is
/// one already. A harness that looks upward for its project's files then stops at `dir`
/// instead of walking into folders its sandbox closes: OpenCode over ACP refused to start a
/// session in a no-tools scratch folder under Argonauts' home.
pub fn init_scratch_repo(dir: &Path) -> io::Result<()> {
    if dir.join(".git").exists() {
        return Ok(());
    }
    let mut command = std::process::Command::new("git");
    command
        .args(SAFE_CONFIG)
        .args(["init", "--quiet", "--template="])
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for name in REPOSITORY_ENV {
        command.env_remove(name);
    }
    let out = command.output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "git init in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Every clone and bound copy shares what git already told it, so one engine asks once.
#[derive(Debug, Clone, Default)]
pub struct Git {
    env: Vec<(String, String)>,
    bound: Option<Bound>,
    known: Arc<Known>,
    /// What commits leave out while it's untracked.
    build_output: Arc<BuildOutput>,
}

/// Answers that hold while the engine runs, by the directory they were asked in.
#[derive(Debug, Default)]
struct Known {
    /// Whether git has an author and committer for commits made there.
    identity: Mutex<HashMap<PathBuf, bool>>,
    common_dir: Mutex<HashMap<PathBuf, PathBuf>>,
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
        Self {
            env,
            ..Self::default()
        }
    }

    /// Git that always uses `git_dir` and `work_tree`, never the `.git` file it finds. An agent can
    /// rewrite that file to point at a repository whose config runs commands during our `git add`.
    pub fn bound_to(&self, git_dir: PathBuf, work_tree: PathBuf) -> Self {
        Self {
            env: self.env.clone(),
            bound: Some(Bound { git_dir, work_tree }),
            known: self.known.clone(),
            build_output: self.build_output.clone(),
        }
    }

    /// What commits leave out while it's untracked.
    pub fn build_output(&self) -> &BuildOutput {
        &self.build_output
    }

    /// Commits leave out untracked files that `extra` names, such as `merge.leave_out`, and
    /// the built-in list's when `builtin` is set.
    pub fn leaving_out(mut self, builtin: bool, extra: &[String]) -> Self {
        self.build_output = Arc::new(BuildOutput::of(builtin, extra));
        self
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

    /// Stages everything in `worktree` and commits it, except untracked files the leave-out
    /// settings name, which stay in the tree as they were. `None` when there was nothing to
    /// commit.
    pub async fn commit_all(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<Option<String>, GitError> {
        Ok(self.commit_work(worktree, message).await?.sha)
    }

    /// [`Git::commit_all`], saying which untracked files it left out as build output.
    pub async fn commit_work(&self, worktree: &Path, message: &str) -> Result<Committed, GitError> {
        let left_out = self.stage_all(worktree).await?;
        let sha = self.commit_index(worktree, message).await?;
        Ok(Committed { sha, left_out })
    }

    /// Stages everything but the untracked files the leave-out settings name. Returns what it
    /// left out.
    async fn stage_all(&self, worktree: &Path) -> Result<Vec<LeftOut>, GitError> {
        let untracked = self.untracked_files(worktree).await?;
        let left_out = self.untracked_build_output(worktree, untracked).await?;
        self.run(worktree, &["add", "--all"]).await?;
        for chunk in left_out.chunks(PATHS_PER_CALL) {
            let mut args = vec!["--literal-pathspecs", "rm", "--cached", "--quiet", "--"];
            args.extend(chunk.iter().map(|left| left.path.as_str()));
            self.run(worktree, &args).await?;
        }
        Ok(left_out)
    }

    /// The build output among `untracked`. A folder the repo already tracks files in, such as
    /// a `build/` that holds scripts, is the repo's own, so what's new there is committed,
    /// unless an inner folder or the file itself is build output, such as `build/__pycache__/`.
    async fn untracked_build_output(
        &self,
        dir: &Path,
        untracked: Vec<String>,
    ) -> Result<Vec<LeftOut>, GitError> {
        let mut tracked_folders: HashMap<String, bool> = HashMap::new();
        let mut left_out = Vec::new();
        for path in untracked {
            for found in self.build_output.matches(&path) {
                if found.folder {
                    let tracked = match tracked_folders.get(&found.path) {
                        Some(tracked) => *tracked,
                        None => {
                            let tracked = self.is_tracked(dir, &found.path).await?;
                            tracked_folders.insert(found.path.clone(), tracked);
                            tracked
                        }
                    };
                    if tracked {
                        continue;
                    }
                }
                let because = if found.folder {
                    format!("{}/", found.path)
                } else {
                    found.path
                };
                left_out.push(LeftOut { path, because });
                break;
            }
        }
        Ok(left_out)
    }

    /// Commits what is staged and nothing else. `None` when nothing is.
    pub async fn commit_index(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<Option<String>, GitError> {
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

    /// Each changed path with its lines added and removed, as `git diff --numstat` lists them.
    /// A binary file counts no lines.
    pub async fn diff_files(
        &self,
        dir: &Path,
        from: &str,
        to: &str,
    ) -> Result<Vec<(String, u32, u32)>, GitError> {
        let args = [
            "diff",
            "--numstat",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            from,
            to,
        ];
        let out = self.run(dir, &args).await?;
        Ok(numstat_files(&out))
    }

    /// [`Git::diff_stat`] over `paths` alone, taken literally. No paths count nothing.
    pub async fn diff_stat_of(
        &self,
        dir: &Path,
        from: &str,
        to: &str,
        paths: &[String],
    ) -> Result<DiffStat, GitError> {
        if paths.is_empty() {
            return Ok(DiffStat::default());
        }
        let specs: Vec<String> = paths
            .iter()
            .map(|path| format!(":(literal){path}"))
            .collect();
        let mut args = vec![
            "diff",
            "--numstat",
            "--no-ext-diff",
            "--no-textconv",
            from,
            to,
            "--",
        ];
        args.extend(specs.iter().map(String::as_str));
        let out = self.run(dir, &args).await?;
        Ok(parse_numstat(&out))
    }

    pub async fn diff(&self, dir: &Path, from: &str, to: &str) -> Result<String, GitError> {
        self.diff_with(dir, &[], from, to, &[]).await
    }

    /// The diff the secret scan reads. Git shows no lines for a binary file, and most build
    /// output is binary, so build output comes after the rest as text: its bytes are scanned
    /// like any added line instead of stopping the card as unreadable.
    pub async fn scan_diff(&self, dir: &Path, from: &str, to: &str) -> Result<String, GitError> {
        let mut rest = vec!["--".to_owned(), ".".to_owned()];
        rest.extend(BuildOutput::pathspecs(true));
        let mut built = vec!["--".to_owned()];
        built.extend(BuildOutput::pathspecs(false));
        let mut diff = self.diff_with(dir, &[], from, to, &rest).await?;
        diff.push_str(&self.diff_with(dir, &["--text"], from, to, &built).await?);
        Ok(diff)
    }

    /// The diff with each changed function shown whole, for reviewers: a bug in the lines a
    /// change keeps is out of sight in three lines of context.
    pub async fn review_diff(&self, dir: &Path, from: &str, to: &str) -> Result<String, GitError> {
        self.diff_with(dir, &["--function-context"], from, to, &[])
            .await
    }

    /// `paths` follows the two revisions: `--` and pathspecs, or nothing for every file.
    async fn diff_with(
        &self,
        dir: &Path,
        extra: &[&str],
        from: &str,
        to: &str,
        paths: &[String],
    ) -> Result<String, GitError> {
        let mut args = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv"];
        args.extend_from_slice(extra);
        args.extend([from, to]);
        args.extend(paths.iter().map(String::as_str));
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
        let cached = lock(&self.known.identity).get(dir).copied();
        let has_identity = match cached {
            Some(known) => known,
            None => match self.has_identity(dir).await {
                Some(known) => {
                    lock(&self.known.identity).insert(dir.to_path_buf(), known);
                    known
                }
                None => false,
            },
        };
        if has_identity {
            Vec::new()
        } else {
            FALLBACK_IDENTITY.to_vec()
        }
    }

    /// `None` when git couldn't be run, which isn't worth remembering.
    async fn has_identity(&self, dir: &Path) -> Option<bool> {
        for var in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
            let out = self.output(dir, &["var", var], &[]).await.ok()?;
            if !out.status.success() {
                return Some(false);
            }
        }
        Some(true)
    }

    async fn common_dir(&self, repo: &Path) -> Result<PathBuf, GitError> {
        if let Some(dir) = lock(&self.known.common_dir).get(repo) {
            return Ok(dir.clone());
        }
        let dir = repo.join(self.run(repo, &["rev-parse", "--git-common-dir"]).await?);
        lock(&self.known.common_dir).insert(repo.to_path_buf(), dir.clone());
        Ok(dir)
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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

/// Splits `log --format=%H%x00%B%x1e` output into `(sha, message)` pairs.
fn parse_log(out: &str) -> Vec<(String, String)> {
    out.split('\x1e')
        .filter_map(|entry| {
            let (sha, message) = entry.trim_start_matches('\n').split_once('\0')?;
            Some((sha.to_owned(), message.trim_end().to_owned()))
        })
        .collect()
}

/// `git diff --numstat` output, line by line: `added<TAB>removed<TAB>path`, with `-` for a
/// binary file's counts.
fn numstat_files(text: &str) -> Vec<(String, u32, u32)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next()?.parse().unwrap_or(0);
            let removed = fields.next()?.parse().unwrap_or(0);
            let path = fields.next()?.trim();
            (!path.is_empty()).then(|| (path.to_owned(), added, removed))
        })
        .collect()
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

/// The operations the crew engine adds for branches, worktrees, commits, and merges.
impl Git {
    /// A branch at `base`, not checked out anywhere.
    pub async fn create_branch(&self, repo: &Path, name: &str, base: &str) -> Result<(), GitError> {
        let args = ["branch", "--no-track", name, base];
        self.run(repo, &args).await.map(drop)
    }

    /// A worktree on a branch that already exists.
    pub async fn add_worktree_on(
        &self,
        repo: &Path,
        path: &Path,
        branch: &str,
    ) -> Result<(), GitError> {
        let path = path.to_string_lossy();
        let args = ["worktree", "add", "--quiet", &path, branch];
        self.run(repo, &args).await.map(drop)
    }

    /// A detached worktree at `rev`, such as the run's read-only base worktree.
    pub async fn add_detached_worktree(
        &self,
        repo: &Path,
        path: &Path,
        rev: &str,
    ) -> Result<(), GitError> {
        let path = path.to_string_lossy();
        let args = ["worktree", "add", "--quiet", "--detach", &path, rev];
        self.run(repo, &args).await.map(drop)
    }

    /// The administrative directory of the worktree at `worktree`, read from the main
    /// repository's own records rather than the worktree's `.git` file, which an agent can
    /// rewrite. Pass the result to [`Git::bound_to`].
    pub async fn worktree_git_dir(
        &self,
        repo: &Path,
        worktree: &Path,
    ) -> Result<Option<PathBuf>, GitError> {
        let common = self.common_dir(repo).await?;
        let wanted = normalized(&worktree.join(".git"));
        let entries = match std::fs::read_dir(common.join("worktrees")) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(GitError::Spawn(err)),
        };
        for entry in entries.flatten() {
            let Ok(recorded) = std::fs::read_to_string(entry.path().join("gitdir")) else {
                continue;
            };
            if normalized(Path::new(recorded.trim_end())) == wanted {
                return Ok(Some(entry.path()));
            }
        }
        Ok(None)
    }

    /// Stages and commits only `paths`. `None` when they held no change. Paths that neither
    /// exist nor are tracked are skipped.
    pub async fn commit_paths(
        &self,
        worktree: &Path,
        paths: &[&str],
        message: &str,
    ) -> Result<Option<String>, GitError> {
        let mut present = Vec::new();
        for path in paths {
            if worktree.join(path).exists() || self.is_tracked(worktree, path).await? {
                present.push(*path);
            }
        }
        if present.is_empty() {
            return Ok(None);
        }
        let mut add = vec!["add", "--all", "--"];
        add.extend_from_slice(&present);
        self.run(worktree, &add).await?;

        let mut staged = vec!["diff", "--cached", "--quiet", "--no-ext-diff", "--"];
        staged.extend_from_slice(&present);
        let out = self.output(worktree, &staged, &[]).await?;
        match out.status.code() {
            Some(0) => return Ok(None),
            Some(1) => {}
            _ => return Err(failed(&staged, &out)),
        }
        let identity = self.identity_env(worktree).await;
        let mut commit = vec![
            "commit",
            "--quiet",
            "--no-verify",
            "--cleanup=whitespace",
            "-m",
            message,
            "--only",
            "--",
        ];
        commit.extend_from_slice(&present);
        let out = self.output(worktree, &commit, &identity).await?;
        if !out.status.success() {
            return Err(failed(&commit, &out));
        }
        self.resolve(worktree, "HEAD").await.map(Some)
    }

    /// A merge commit even when a fast-forward is possible. A conflicted merge is aborted and
    /// returns the conflicting paths, leaving the tree as it was.
    pub async fn merge_no_ff(
        &self,
        worktree: &Path,
        rev: &str,
        message: &str,
    ) -> Result<MergeOutcome, GitError> {
        let identity = self.identity_env(worktree).await;
        let args = [
            "merge",
            "--no-ff",
            "--no-edit",
            "--quiet",
            "-m",
            message,
            rev,
        ];
        let out = self.output(worktree, &args, &identity).await?;
        if out.status.success() {
            return self
                .resolve(worktree, "HEAD")
                .await
                .map(MergeOutcome::Merged);
        }
        let files = self.unmerged_paths(worktree).await.unwrap_or_default();
        self.abort_merge(worktree).await?;
        if files.is_empty() {
            Err(failed(&args, &out))
        } else {
            Ok(MergeOutcome::Conflict(files))
        }
    }

    /// `None` when the branch doesn't exist.
    pub async fn branch_head(&self, dir: &Path, branch: &str) -> Result<Option<String>, GitError> {
        let spec = format!("refs/heads/{branch}^{{commit}}");
        self.optional(dir, &["rev-parse", "--verify", "--quiet", &spec])
            .await
    }

    /// No staged, unstaged, or untracked changes.
    pub async fn is_clean(&self, dir: &Path) -> Result<bool, GitError> {
        let args = [
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ];
        Ok(self.run(dir, &args).await?.is_empty())
    }

    /// `(sha, full message)` for each commit in `range`, newest first, such as `base..head`.
    pub async fn log_messages(
        &self,
        dir: &Path,
        range: &str,
    ) -> Result<Vec<(String, String)>, GitError> {
        let args = ["log", "--no-color", "--format=%H%x00%B%x1e", range, "--"];
        Ok(parse_log(&self.run(dir, &args).await?))
    }

    /// The parents of `rev`: none for a root commit, two or more for a merge.
    pub async fn parents(&self, dir: &Path, rev: &str) -> Result<Vec<String>, GitError> {
        let out = self
            .run(dir, &["rev-list", "--parents", "-n", "1", rev])
            .await?;
        Ok(out.split_whitespace().skip(1).map(str::to_owned).collect())
    }

    /// Merges `rev` without committing (`--no-ff --no-commit`). On a conflict the merge is
    /// aborted, unless `keep_conflicts` leaves the markers for someone to resolve.
    pub async fn merge_staged(
        &self,
        worktree: &Path,
        rev: &str,
        keep_conflicts: bool,
    ) -> Result<StagedMerge, GitError> {
        let identity = self.identity_env(worktree).await;
        let args = ["merge", "--no-ff", "--no-commit", "--quiet", rev];
        let out = self.output(worktree, &args, &identity).await?;
        if out.status.success() {
            return Ok(if self.merge_in_progress(worktree).await? {
                StagedMerge::Staged
            } else {
                StagedMerge::UpToDate
            });
        }
        let files = self.unmerged_paths(worktree).await.unwrap_or_default();
        if files.is_empty() || !keep_conflicts {
            self.abort_merge(worktree).await?;
        }
        if files.is_empty() {
            Err(failed(&args, &out))
        } else {
            Ok(StagedMerge::Conflict(files))
        }
    }

    /// Stages everything and commits the merge in progress, even when its tree equals HEAD's.
    pub async fn finish_merge(&self, worktree: &Path, message: &str) -> Result<String, GitError> {
        Ok(self.finish_merge_work(worktree, message).await?.0)
    }

    /// [`Git::finish_merge`], saying which untracked files it left out as build output.
    pub async fn finish_merge_work(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<(String, Vec<LeftOut>), GitError> {
        let left_out = self.stage_all(worktree).await?;
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
        Ok((self.resolve(worktree, "HEAD").await?, left_out))
    }

    /// Undoes a merge in progress. Does nothing when there is none, and fails when the abort
    /// did, so a merge left behind never surfaces later as an unrelated error.
    pub async fn abort_merge(&self, worktree: &Path) -> Result<(), GitError> {
        if !self.merge_in_progress(worktree).await? {
            return Ok(());
        }
        self.run(worktree, &["merge", "--abort"]).await.map(|_| ())
    }

    /// Stages the reverse of `commits`, given newest first, without committing. On a conflict
    /// the revert is undone, so the tree is left as it was, and the paths that conflicted come
    /// back.
    pub async fn revert_staged(
        &self,
        worktree: &Path,
        commits: &[String],
    ) -> Result<Vec<String>, GitError> {
        let mut args = vec!["revert", "--no-commit", "--no-edit"];
        args.extend(commits.iter().map(String::as_str));
        let out = self.output(worktree, &args, &[]).await?;
        if out.status.success() {
            return Ok(Vec::new());
        }
        let files = self.unmerged_paths(worktree).await.unwrap_or_default();
        self.run(worktree, &["revert", "--abort"]).await?;
        if files.is_empty() {
            Err(failed(&args, &out))
        } else {
            Ok(files)
        }
    }

    /// Files whose changes against HEAD, staged or not, still hold a conflict marker.
    pub async fn leftover_markers(&self, dir: &Path) -> Result<Vec<String>, GitError> {
        let args = ["diff", "--check", "--no-color", "--no-ext-diff", "HEAD"];
        let out = self.output(dir, &args, &[]).await?;
        // `--check` exits 2 for any problem it finds, whitespace included.
        if !matches!(out.status.code(), Some(0 | 2)) {
            return Err(failed(&args, &out));
        }
        let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.ends_with("leftover conflict marker"))
            .filter_map(|line| line.split_once(':').map(|(file, _)| file.to_owned()))
            .collect();
        files.dedup();
        Ok(files)
    }

    /// `(sha, full message)` along the first-parent chain in `range`, newest first: the
    /// branch's own commits, without what its merges brought in.
    pub async fn first_parent_log(
        &self,
        dir: &Path,
        range: &str,
    ) -> Result<Vec<(String, String)>, GitError> {
        let args = [
            "log",
            "--first-parent",
            "--no-color",
            "--format=%H%x00%B%x1e",
            range,
            "--",
        ];
        Ok(parse_log(&self.run(dir, &args).await?))
    }

    /// What a merge commit changed beyond git's own merge of its parents: a conflict's
    /// resolution and anything else edited while the merge was open.
    pub async fn remerge_diff(&self, dir: &Path, merge: &str) -> Result<String, GitError> {
        let args = [
            "show",
            "--remerge-diff",
            "--format=",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            merge,
        ];
        let out = self.output(dir, &args, &[]).await?;
        if !out.status.success() {
            return Err(failed(&args, &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn merge_in_progress(&self, dir: &Path) -> Result<bool, GitError> {
        let args = ["rev-parse", "--quiet", "--verify", "MERGE_HEAD"];
        Ok(self.optional(dir, &args).await?.is_some())
    }

    /// Untracked files that `.gitignore` doesn't exclude, relative to `dir`.
    pub async fn untracked_files(&self, dir: &Path) -> Result<Vec<String>, GitError> {
        let args = ["ls-files", "--others", "--exclude-standard", "-z"];
        let out = self.run(dir, &args).await?;
        Ok(out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Your files in the checkout at `dir` that moving it from `from` to `to` would overwrite,
    /// where git stops: one at a path `to` adds, one where a folder of `to` goes, and a folder
    /// with something in it where a file of `to` goes, listed with a `/`. Ignored files count
    /// too, since git would overwrite those without a word. What `from` tracks is git's to
    /// replace, so of a tracked folder only what you added to it counts.
    pub async fn in_the_way(
        &self,
        dir: &Path,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        let args = [
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "-z",
            "--diff-filter=A",
            from,
            to,
        ];
        let added = self.run(dir, &args).await?;
        let mut found = BTreeSet::new();
        'paths: for path in added.split('\0').filter(|path| !path.is_empty()) {
            let parts: Vec<&str> = path.split('/').collect();
            for end in 1..parts.len() {
                let folder = parts[..end].join("/");
                match std::fs::symlink_metadata(dir.join(&folder)) {
                    Ok(meta) if meta.is_dir() => {}
                    // A file of `from` the run turns into a folder: git replaces it.
                    Ok(_) if self.in_tree(dir, from, &folder).await? => continue 'paths,
                    Ok(_) => {
                        found.insert(folder);
                        continue 'paths;
                    }
                    // Nothing there, so nothing below it either.
                    Err(_) => continue 'paths,
                }
            }
            let Ok(meta) = std::fs::symlink_metadata(dir.join(path)) else {
                continue;
            };
            if !meta.is_dir() {
                found.insert(path.to_owned());
            } else if self.in_tree(dir, from, path).await? {
                // A folder of `from` the run turns into a file: git takes its files out, so
                // only what you added to it stays in the way.
                found.extend(self.untracked_under(dir, path).await?);
            } else if std::fs::read_dir(dir.join(path))
                .is_ok_and(|mut entries| entries.next().is_some())
            {
                found.insert(format!("{path}/"));
            }
        }
        Ok(found.into_iter().collect())
    }

    /// Your uncommitted changes, staged or not, to files that moving the checkout at `dir`
    /// from `from` to `to` changes. Git stops there; changes to other files ride along.
    pub async fn edited_in_the_way(
        &self,
        dir: &Path,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        let edited = self.edited_paths(dir).await?;
        if edited.is_empty() {
            return Ok(Vec::new());
        }
        let args = [
            "diff",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "-z",
            from,
            to,
        ];
        let changed = self.run(dir, &args).await?;
        let changed: BTreeSet<&str> = changed
            .split('\0')
            .filter(|path| !path.is_empty())
            .collect();
        Ok(edited
            .into_iter()
            .filter(|path| changed.contains(path.as_str()))
            .collect())
    }

    /// Tracked files with uncommitted changes, staged or not, both sides of a rename.
    async fn edited_paths(&self, dir: &Path) -> Result<BTreeSet<String>, GitError> {
        let args = [
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
        ];
        let out = self.run(dir, &args).await?;
        let mut paths = BTreeSet::new();
        let mut entries = out.split('\0').filter(|entry| !entry.is_empty());
        while let Some(entry) = entries.next() {
            let (status, path) = entry.split_at(entry.len().min(3));
            paths.insert(path.to_owned());
            // A rename or copy names where it came from next.
            if status.contains(['R', 'C'])
                && let Some(from) = entries.next()
            {
                paths.insert(from.to_owned());
            }
        }
        Ok(paths)
    }

    /// Whether `rev`'s tree holds `path`, as a file or a folder.
    async fn in_tree(&self, dir: &Path, rev: &str, path: &str) -> Result<bool, GitError> {
        let args = ["ls-tree", "--name-only", rev, "--", path];
        Ok(!self.run(dir, &args).await?.is_empty())
    }

    /// Untracked files under the folder `path`, ignored ones too.
    async fn untracked_under(&self, dir: &Path, path: &str) -> Result<Vec<String>, GitError> {
        let folder = format!("{}/", path.trim_end_matches('/'));
        let args = [
            "--literal-pathspecs",
            "ls-files",
            "--others",
            "-z",
            "--",
            &folder,
        ];
        let out = self.run(dir, &args).await?;
        Ok(out
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    async fn unmerged_paths(&self, dir: &Path) -> Result<Vec<String>, GitError> {
        let args = ["diff", "--name-only", "-z", "--diff-filter=U"];
        let out = self.run(dir, &args).await?;
        Ok(out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Whether git tracks `path`, or any file under it when it's a folder.
    async fn is_tracked(&self, dir: &Path, path: &str) -> Result<bool, GitError> {
        let args = ["--literal-pathspecs", "ls-files", "-z", "--", path];
        Ok(!self.run(dir, &args).await?.is_empty())
    }
}

/// `path` with `.` and `..` resolved lexically, and symlinks resolved where it exists, so two
/// spellings of one worktree compare equal.
fn normalized(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(parent) => parent.join(name),
            Err(_) => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TestRepo, git_test_env};

    fn git() -> Git {
        Git::with_env(git_test_env())
    }

    #[test]
    fn numstat_lists_each_file_with_its_lines() {
        let text = "19\t3\tinvoice/money.py\n-\t-\tlogo.png\n0\t4\tdocs/old notes.md\n";
        assert_eq!(
            numstat_files(text),
            vec![
                ("invoice/money.py".to_owned(), 19, 3),
                ("logo.png".to_owned(), 0, 0),
                ("docs/old notes.md".to_owned(), 0, 4),
            ]
        );
        assert!(numstat_files("").is_empty());
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
    async fn a_scratch_folder_becomes_an_empty_repository_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("sessions").join("s-001").join("work");
        std::fs::create_dir_all(&scratch).unwrap();
        init_scratch_repo(&scratch).unwrap();
        let top = git().toplevel(&scratch).await.unwrap();
        assert_eq!(top.canonicalize().unwrap(), scratch.canonicalize().unwrap());
        assert_eq!(git().head_commit(&scratch).await.unwrap(), None);
        let hooks = std::fs::read_dir(scratch.join(".git").join("hooks"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(hooks, 0, "no template, so no hooks");
        // Again is a no-op.
        init_scratch_repo(&scratch).unwrap();
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

    /// Git that leaves the built-in list of build output out, as `merge.leave_out_builtin` asks.
    fn listing() -> Git {
        git().leaving_out(true, &[])
    }

    #[tokio::test]
    async fn commits_leave_untracked_build_output_out() {
        let repo = TestRepo::new();
        let git = listing();
        repo.write("legacy/old.pyc", "tracked on purpose");
        repo.git(&["add", "legacy/old.pyc"]);
        repo.git(&["commit", "-qm", "track a pyc"]);
        repo.write("stats/core.py", "def median(): pass\n");
        repo.write("stats/__pycache__/core.cpython-312.pyc", "bytecode");
        repo.write(".pytest_cache/v/cache/nodeids", "[]");
        repo.write("legacy/old.pyc", "changed");
        // One the agent staged itself is one it meant to commit.
        repo.write("fixtures/Main.class", "a fixture");
        repo.git(&["add", "fixtures/Main.class"]);

        git.commit_all(&repo.root, "work").await.unwrap().unwrap();
        let committed = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
        let committed: Vec<&str> = committed.lines().collect();
        assert_eq!(
            committed,
            ["fixtures/Main.class", "legacy/old.pyc", "stats/core.py"]
        );
        let untracked = git.untracked_files(&repo.root).await.unwrap();
        assert!(
            untracked.contains(&"stats/__pycache__/core.cpython-312.pyc".to_owned()),
            "the build output stays in the tree: {untracked:?}"
        );

        // Nothing but build output left means nothing to commit.
        repo.write("stats/__pycache__/other.cpython-312.pyc", "more bytecode");
        assert_eq!(git.commit_all(&repo.root, "again").await.unwrap(), None);
    }

    #[tokio::test]
    async fn packages_caches_and_settings_patterns_stay_out() {
        let repo = TestRepo::new();
        repo.write("build/release.sh", "#!/bin/sh\n");
        repo.git(&["add", "build/release.sh"]);
        repo.git(&["commit", "-qm", "track a build script"]);
        let leaving_logs = git().leaving_out(true, &["*.log".to_owned()]);
        repo.write("app.js", "export {}\n");
        repo.write("build/deploy.sh", "#!/bin/sh\n");
        repo.write("dist/app.min.js", "minified");
        repo.write("web/node_modules/left-pad/index.js", "module.exports = 1");
        repo.write(".DS_Store", "finder");
        repo.write("logs/test.log", "ran");

        let committed = leaving_logs.commit_work(&repo.root, "work").await.unwrap();
        assert!(committed.sha.is_some());
        let names = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
        let names: Vec<&str> = names.lines().collect();
        assert_eq!(
            names,
            ["app.js", "build/deploy.sh", "dist/app.min.js"],
            "build/ and dist/ hold source as often as not"
        );
        let mut left_out: Vec<(String, String)> = committed
            .left_out
            .into_iter()
            .map(|left| (left.path, left.because))
            .collect();
        left_out.sort();
        let pair = |path: &str, because: &str| (path.to_owned(), because.to_owned());
        assert_eq!(
            left_out,
            [
                pair(".DS_Store", ".DS_Store"),
                pair("logs/test.log", "logs/test.log"),
                pair("web/node_modules/left-pad/index.js", "web/node_modules/")
            ]
        );
        // Without the setting, a log is the agent's work.
        assert!(
            git()
                .commit_all(&repo.root, "logs")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_merge_commit_leaves_build_output_out() {
        let repo = TestRepo::new();
        let git = listing();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-qb", "side"]);
        repo.write("side.py", "x = 1\n");
        repo.git(&["add", "side.py"]);
        repo.git(&["commit", "-qm", "side"]);
        let side = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-q", &base]);
        repo.git(&["checkout", "-qb", "integration"]);
        repo.write("main.py", "y = 2\n");
        repo.git(&["add", "main.py"]);
        repo.git(&["commit", "-qm", "main"]);
        assert_eq!(
            git.merge_staged(&repo.root, &side, false).await.unwrap(),
            StagedMerge::Staged
        );
        // The final check ran the tests in the tree before the merge commit.
        repo.write("__pycache__/side.cpython-312.pyc", "bytecode");
        let merge = git.finish_merge(&repo.root, "Merge side").await.unwrap();
        let names = repo.git(&[
            "show",
            "--name-only",
            "--format=",
            "-m",
            "--first-parent",
            &merge,
        ]);
        assert_eq!(names.lines().collect::<Vec<_>>(), ["side.py"]);
        assert!(repo.root.join("__pycache__/side.cpython-312.pyc").exists());
    }

    #[tokio::test]
    async fn files_in_the_way_of_a_fast_forward_are_found_before_git_refuses() {
        let repo = TestRepo::new();
        let git = git();
        repo.write(".gitignore", "*.log\n");
        repo.git(&["add", ".gitignore"]);
        repo.git(&["commit", "-qm", "ignore logs"]);
        let main = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-qb", "run"]);
        for path in [
            "stats/__pycache__/core.cpython-312.pyc",
            "notes.md",
            "lib/util.py",
            "data/a.csv",
            "debug.log",
            "clean.py",
        ] {
            repo.write(path, "the run's\n");
        }
        repo.git(&["add", "--all", "--force"]);
        repo.git(&["commit", "-qm", "run"]);
        let run = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-q", "main"]);

        assert!(
            git.in_the_way(&repo.root, &main, &run)
                .await
                .unwrap()
                .is_empty()
        );
        repo.write("stats/__pycache__/core.cpython-312.pyc", "yours");
        repo.write("notes.md", "yours");
        repo.write("lib", "a file where a folder goes");
        repo.write("data/a.csv/inner.txt", "a folder where a file goes");
        repo.write("debug.log", "ignored, and still yours");
        std::fs::create_dir_all(repo.root.join("clean.py")).unwrap();
        assert_eq!(
            git.in_the_way(&repo.root, &main, &run).await.unwrap(),
            [
                "data/a.csv/",
                "debug.log",
                "lib",
                "notes.md",
                "stats/__pycache__/core.cpython-312.pyc"
            ],
            "an empty folder is no loss"
        );
        assert!(
            git.fast_forward(&repo.root, &run).await.is_err(),
            "git refuses too"
        );
    }

    /// The review's probe: what main tracks is git's to replace, so a tracked file the run
    /// turns into a folder, or a tracked folder it turns into a file, is in nobody's way. Only
    /// what you added to that folder is.
    #[tokio::test]
    async fn what_main_tracks_is_never_in_the_way() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("config", "debug = true\n");
        repo.write("docs/guide.md", "# Guide\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "a config file and a docs folder"]);
        let main = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-qb", "run"]);
        repo.git(&["rm", "-q", "config", "docs/guide.md"]);
        repo.write("config/settings.toml", "debug = true\n");
        repo.write("docs", "see the wiki\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "run"]);
        let run = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-q", "main"]);

        assert_eq!(
            git.in_the_way(&repo.root, &main, &run).await.unwrap(),
            Vec::<String>::new()
        );
        repo.write("docs/draft.md", "yours");
        assert_eq!(
            git.in_the_way(&repo.root, &main, &run).await.unwrap(),
            ["docs/draft.md"]
        );
        std::fs::remove_file(repo.root.join("docs/draft.md")).unwrap();
        assert!(git.fast_forward(&repo.root, &run).await.unwrap());
    }

    /// Your uncommitted edits stop a fast-forward only where the merge changes the file.
    #[tokio::test]
    async fn edits_to_files_the_merge_changes_are_in_the_way() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("README.md", "# Store\n");
        repo.write("store.rs", "fn get() {}\n");
        repo.write("old.rs", "fn old() {}\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "base"]);
        let main = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-qb", "run"]);
        repo.write("store.rs", "fn get() {}\nfn put() {}\n");
        repo.git(&["commit", "-qam", "run"]);
        let run = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.git(&["checkout", "-q", "main"]);

        repo.write("README.md", "# Store, edited\n");
        repo.git(&["mv", "old.rs", "older.rs"]);
        assert!(
            git.edited_in_the_way(&repo.root, &main, &run)
                .await
                .unwrap()
                .is_empty(),
            "edits to files the merge leaves alone ride along"
        );
        repo.write("store.rs", "fn get() { todo!() }\n");
        assert_eq!(
            git.edited_in_the_way(&repo.root, &main, &run)
                .await
                .unwrap(),
            ["store.rs"]
        );
        repo.git(&["add", "store.rs"]);
        assert_eq!(
            git.edited_in_the_way(&repo.root, &main, &run)
                .await
                .unwrap(),
            ["store.rs"],
            "staged counts too"
        );
        assert!(git.fast_forward(&repo.root, &run).await.is_err());
        repo.git(&["checkout", "-q", "HEAD", "--", "store.rs"]);
        assert!(git.fast_forward(&repo.root, &run).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(repo.root.join("README.md")).unwrap(),
            "# Store, edited\n"
        );
    }

    /// The review's probe: folders named like build output that hold source, and a 3D model,
    /// all go into the commit, and each left-out file says why.
    #[tokio::test]
    async fn source_named_like_build_output_is_committed() {
        let repo = TestRepo::new();
        for path in [
            "app.py",
            "textkit/build/__init__.py",
            "textkit/build/steps.py",
            "action/dist/index.js",
            "assets/cube.obj",
            "docs/target/README.md",
            "textkit/__pycache__/steps.cpython-312.pyc",
        ] {
            repo.write(path, "work\n");
        }
        let committed = listing().commit_work(&repo.root, "work").await.unwrap();
        let names = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
        let names: Vec<&str> = names.lines().collect();
        assert_eq!(
            names,
            [
                "action/dist/index.js",
                "app.py",
                "assets/cube.obj",
                "docs/target/README.md",
                "textkit/build/__init__.py",
                "textkit/build/steps.py"
            ]
        );
        assert_eq!(
            committed.left_out,
            [LeftOut {
                path: "textkit/__pycache__/steps.cpython-312.pyc".into(),
                because: "textkit/__pycache__/".into()
            }]
        );
    }

    /// The review's probe: a folder the repo keeps files in is its own, and build output
    /// inside it still stays out.
    #[tokio::test]
    async fn build_output_inside_a_tracked_folder_stays_out() {
        let repo = TestRepo::new();
        repo.write("build/release.sh", "#!/bin/sh\n");
        repo.git(&["add", "build/release.sh"]);
        repo.git(&["commit", "-qm", "track a build script"]);
        repo.write("build/__pycache__/x.cpython-312.pyc", "bytecode");
        repo.write("build/out.o", "object");
        repo.write("build/deploy.sh", "#!/bin/sh\n");
        let leaving_build = git().leaving_out(true, &["build/".to_owned()]);
        let committed = leaving_build.commit_work(&repo.root, "work").await.unwrap();
        let names = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(names.lines().collect::<Vec<_>>(), ["build/deploy.sh"]);
        let mut because: Vec<String> = committed
            .left_out
            .into_iter()
            .map(|left| left.because)
            .collect();
        because.sort();
        assert_eq!(because, ["build/__pycache__/", "build/out.o"]);
    }

    /// By default a commit takes what git doesn't ignore, bytecode too, and
    /// `merge.leave_out` alone still leaves out what it names.
    #[tokio::test]
    async fn the_built_in_list_is_opt_in() {
        let repo = TestRepo::new();
        repo.write("__pycache__/app.cpython-312.pyc", "bytecode");
        let committed = git().commit_work(&repo.root, "work").await.unwrap();
        assert!(committed.sha.is_some());
        assert!(committed.left_out.is_empty());
        let names = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(
            names.lines().collect::<Vec<_>>(),
            ["__pycache__/app.cpython-312.pyc"]
        );

        repo.write("__pycache__/util.cpython-312.pyc", "bytecode");
        repo.write("run.log", "ran");
        let committed = git()
            .leaving_out(false, &["*.log".to_owned()])
            .commit_work(&repo.root, "work")
            .await
            .unwrap();
        let names = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(
            names.lines().collect::<Vec<_>>(),
            ["__pycache__/util.cpython-312.pyc"]
        );
        assert_eq!(committed.left_out.len(), 1);
    }

    #[tokio::test]
    async fn the_scan_diff_shows_build_output_as_text() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let binary =
            |text: &str| [b"\x6f\x0d\x0d\x0a\0\0".as_slice(), text.as_bytes(), b"\0"].concat();
        let dir = repo.root.join("stats/__pycache__");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("core.cpython-312.pyc"), binary("median")).unwrap();
        std::fs::write(repo.root.join("data.bin"), binary("blob")).unwrap();
        repo.write("stats/core.py", "def median(): pass\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "work"]);

        let plain = git.diff(&repo.root, &base, "HEAD").await.unwrap();
        assert!(
            plain.contains("Binary files /dev/null and b/stats/__pycache__/core.cpython-312.pyc")
        );
        let scan = git.scan_diff(&repo.root, &base, "HEAD").await.unwrap();
        assert!(scan.contains("+def median(): pass"), "{scan}");
        assert!(
            scan.contains("Binary files /dev/null and b/data.bin differ"),
            "{scan}"
        );
        assert!(
            !scan.contains("b/stats/__pycache__/core.cpython-312.pyc differ"),
            "{scan}"
        );
        assert!(scan.contains("median\0"), "{scan:?}");
        assert_eq!(scan.matches("diff --git").count(), 3, "{scan}");
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

    #[tokio::test]
    async fn create_branch_and_add_worktree_on_it() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        assert_eq!(
            git.branch_head(&repo.root, "argonauts/r/integration")
                .await
                .unwrap(),
            None
        );
        git.create_branch(&repo.root, "argonauts/r/integration", &base)
            .await
            .unwrap();
        assert_eq!(
            git.branch_head(&repo.root, "argonauts/r/integration")
                .await
                .unwrap()
                .as_deref(),
            Some(base.as_str())
        );
        assert_eq!(
            git.current_branch(&repo.root).await.unwrap().as_deref(),
            Some("main")
        );

        let tree = repo.dir.path().join("trees/integration");
        git.add_worktree_on(&repo.root, &tree, "argonauts/r/integration")
            .await
            .unwrap();
        assert_eq!(
            git.current_branch(&tree).await.unwrap().as_deref(),
            Some("argonauts/r/integration")
        );
        let admin = git
            .worktree_git_dir(&repo.root, &tree)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(admin, git.absolute_git_dir(&tree).await.unwrap());
        let elsewhere = repo.dir.path().join("trees/none");
        assert_eq!(
            git.worktree_git_dir(&repo.root, &elsewhere).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_detached_worktree_has_no_branch() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let tree = repo.dir.path().join("trees/base");
        git.add_detached_worktree(&repo.root, &tree, &base)
            .await
            .unwrap();
        assert_eq!(git.current_branch(&tree).await.unwrap(), None);
        assert_eq!(git.head_commit(&tree).await.unwrap().unwrap(), base);
    }

    #[tokio::test]
    async fn commit_paths_commits_only_those_paths() {
        let repo = TestRepo::new();
        let git = git();
        repo.write(".argonauts/goal.md", "goal\n");
        repo.write("src/agent.rs", "fn main() {}\n");
        let sha = git
            .commit_paths(&repo.root, &[".argonauts/goal.md", "missing.md"], "Goal")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            repo.git(&["show", "--name-only", "--format=", &sha]),
            ".argonauts/goal.md"
        );
        assert_eq!(repo.git(&["status", "--porcelain"]), "?? src/");
        assert_eq!(
            git.commit_paths(&repo.root, &[".argonauts/goal.md"], "Again")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            git.commit_paths(&repo.root, &["missing.md"], "None")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn commit_paths_records_a_deletion() {
        let repo = TestRepo::new();
        let git = git();
        std::fs::remove_file(repo.root.join("README.md")).unwrap();
        let sha = git
            .commit_paths(&repo.root, &["README.md"], "Remove readme")
            .await
            .unwrap();
        assert!(sha.is_some());
        assert!(git.is_clean(&repo.root).await.unwrap());
    }

    #[tokio::test]
    async fn merge_no_ff_always_makes_a_merge_commit() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let tree = repo.dir.path().join("tree");
        git.add_worktree(&repo.root, &tree, "card", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("a.txt"), "a\n").unwrap();
        let card = git.commit_all(&tree, "card work").await.unwrap().unwrap();

        let MergeOutcome::Merged(merge) = git
            .merge_no_ff(&repo.root, "card", "Merge card-001")
            .await
            .unwrap()
        else {
            panic!("expected a merge");
        };
        assert_ne!(merge, card, "fast-forwarded");
        let parents = repo.git(&["log", "-1", "--format=%P", &merge]);
        assert_eq!(parents, format!("{base} {card}"));
        assert_eq!(
            repo.git(&["log", "-1", "--format=%s", &merge]),
            "Merge card-001"
        );
    }

    #[tokio::test]
    async fn merge_no_ff_reports_conflicts_and_aborts() {
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
        let head = git.head_commit(&repo.root).await.unwrap().unwrap();

        let outcome = git
            .merge_no_ff(&repo.root, "card", "Merge card")
            .await
            .unwrap();
        assert_eq!(
            outcome,
            MergeOutcome::Conflict(vec!["README.md".to_owned()])
        );
        assert_eq!(git.head_commit(&repo.root).await.unwrap().unwrap(), head);
        assert!(git.is_clean(&repo.root).await.unwrap());
    }

    #[tokio::test]
    async fn is_clean_sees_untracked_and_staged_files() {
        let repo = TestRepo::new();
        let git = git();
        assert!(git.is_clean(&repo.root).await.unwrap());
        repo.write("new.txt", "x");
        assert!(!git.is_clean(&repo.root).await.unwrap());
        repo.git(&["add", "new.txt"]);
        assert!(!git.is_clean(&repo.root).await.unwrap());
        repo.git(&["commit", "--quiet", "-m", "new"]);
        assert!(git.is_clean(&repo.root).await.unwrap());
    }

    #[tokio::test]
    async fn log_messages_lists_newest_first_with_bodies() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.write("a.txt", "a");
        let first = git
            .commit_all(&repo.root, "First\n\nSession: s-001")
            .await
            .unwrap()
            .unwrap();
        repo.write("b.txt", "b");
        let second = git.commit_all(&repo.root, "Second").await.unwrap().unwrap();
        let log = git
            .log_messages(&repo.root, &format!("{base}..HEAD"))
            .await
            .unwrap();
        assert_eq!(
            log,
            [
                (second, "Second".to_owned()),
                (first, "First\n\nSession: s-001".to_owned())
            ]
        );
        assert!(
            git.log_messages(&repo.root, "HEAD..HEAD")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn parents_tell_root_plain_and_merge_commits_apart() {
        let repo = TestRepo::new();
        let git = git();
        let root = git.head_commit(&repo.root).await.unwrap().unwrap();
        assert!(git.parents(&repo.root, &root).await.unwrap().is_empty());
        git.create_branch(&repo.root, "side", &root).await.unwrap();
        let side = repo.dir.path().join("side");
        git.add_worktree_on(&repo.root, &side, "side")
            .await
            .unwrap();
        std::fs::write(side.join("b.txt"), "b").unwrap();
        git.commit_all(&side, "b").await.unwrap().unwrap();
        repo.write("a.txt", "a");
        let plain = git.commit_all(&repo.root, "a").await.unwrap().unwrap();
        assert_eq!(git.parents(&repo.root, &plain).await.unwrap(), [root]);
        let MergeOutcome::Merged(merge) = git
            .merge_no_ff(&repo.root, "side", "Merge side")
            .await
            .unwrap()
        else {
            panic!("expected a merge");
        };
        assert_eq!(git.parents(&repo.root, &merge).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_staged_merge_commits_only_when_finished() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let tree = repo.dir.path().join("tree");
        git.add_worktree(&repo.root, &tree, "card", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("a.txt"), "a\n").unwrap();
        let card = git.commit_all(&tree, "card").await.unwrap().unwrap();

        let staged = git.merge_staged(&repo.root, "card", false).await.unwrap();
        assert_eq!(staged, StagedMerge::Staged);
        assert!(git.merge_in_progress(&repo.root).await.unwrap());
        assert_eq!(git.head_commit(&repo.root).await.unwrap().unwrap(), base);
        git.abort_merge(&repo.root).await.unwrap();
        assert!(!git.merge_in_progress(&repo.root).await.unwrap());
        assert!(!repo.root.join("a.txt").exists());

        git.merge_staged(&repo.root, "card", false).await.unwrap();
        let merge = git.finish_merge(&repo.root, "Merge card").await.unwrap();
        assert_eq!(git.parents(&repo.root, &merge).await.unwrap(), [base, card]);
        assert_eq!(
            git.merge_staged(&repo.root, "card", false).await.unwrap(),
            StagedMerge::UpToDate
        );
        git.abort_merge(&repo.root).await.unwrap();
    }

    #[tokio::test]
    async fn kept_conflicts_wait_for_a_resolution() {
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

        let outcome = git.merge_staged(&repo.root, "card", false).await.unwrap();
        assert_eq!(outcome, StagedMerge::Conflict(vec!["README.md".into()]));
        assert!(!git.merge_in_progress(&repo.root).await.unwrap());

        let outcome = git.merge_staged(&repo.root, "card", true).await.unwrap();
        assert_eq!(outcome, StagedMerge::Conflict(vec!["README.md".into()]));
        assert!(git.merge_in_progress(&repo.root).await.unwrap());
        repo.write("README.md", "both\n");
        let merge = git.finish_merge(&repo.root, "Resolve").await.unwrap();
        assert_eq!(git.parents(&repo.root, &merge).await.unwrap().len(), 2);
        assert!(git.is_clean(&repo.root).await.unwrap());
    }

    #[tokio::test]
    async fn a_staged_revert_undoes_commits_or_leaves_the_tree_as_it_was() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("a.txt", "a\n");
        let first = git.commit_all(&repo.root, "a").await.unwrap().unwrap();
        repo.write("b.txt", "b\n");
        repo.write("a.txt", "a2\n");
        let second = git.commit_all(&repo.root, "b").await.unwrap().unwrap();
        repo.write("c.txt", "c\n");
        let third = git.commit_all(&repo.root, "c").await.unwrap().unwrap();
        // Left out of the commit, as the tree's other changes are.
        repo.write("scratch.txt", "x\n");

        let conflicts = git
            .revert_staged(&repo.root, &[third, second])
            .await
            .unwrap();
        assert!(conflicts.is_empty());
        let undone = git.commit_index(&repo.root, "undo b and c").await.unwrap();
        assert!(undone.is_some());
        assert!(!repo.root.join("b.txt").exists() && !repo.root.join("c.txt").exists());
        assert_eq!(
            std::fs::read_to_string(repo.root.join("a.txt")).unwrap(),
            "a\n"
        );
        assert_eq!(
            git.untracked_files(&repo.root).await.unwrap(),
            ["scratch.txt"]
        );

        // A later change to the same file can't be undone under it.
        repo.write("a.txt", "a3\n");
        git.commit_paths(&repo.root, &["a.txt"], "a3")
            .await
            .unwrap();
        let head = git.head_commit(&repo.root).await.unwrap().unwrap();
        let conflicts = git.revert_staged(&repo.root, &[first]).await.unwrap();
        assert_eq!(conflicts, ["a.txt"]);
        assert_eq!(git.head_commit(&repo.root).await.unwrap().unwrap(), head);
        assert!(git.tracked_changes(&repo.root).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn untracked_files_skip_ignored_ones() {
        let repo = TestRepo::new();
        let git = git();
        repo.write(".gitignore", "target/\n");
        repo.write("target/big.bin", "x");
        repo.write("notes/new.md", "x");
        let mut files = git.untracked_files(&repo.root).await.unwrap();
        files.sort();
        assert_eq!(files, [".gitignore", "notes/new.md"]);
    }

    #[tokio::test]
    async fn new_ops_never_run_hooks() {
        let repo = TestRepo::new();
        let marker = repo.dir.path().join("hook-ran");
        for name in [
            "pre-commit",
            "post-checkout",
            "pre-merge-commit",
            "post-merge",
        ] {
            let hook = repo.root.join(".git/hooks").join(name);
            std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
            make_executable(&hook);
        }
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        git.create_branch(&repo.root, "b", &base).await.unwrap();
        let tree = repo.dir.path().join("tree");
        git.add_worktree_on(&repo.root, &tree, "b").await.unwrap();
        std::fs::write(tree.join("x.txt"), "x").unwrap();
        git.commit_paths(&tree, &["x.txt"], "x").await.unwrap();
        git.merge_no_ff(&repo.root, "b", "Merge b").await.unwrap();
        git.add_detached_worktree(&repo.root, &repo.dir.path().join("d"), &base)
            .await
            .unwrap();
        assert!(!marker.exists());
    }

    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
