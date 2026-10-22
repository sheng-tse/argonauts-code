//! The git operations Argonauts Code runs, through the git CLI with hooks, fsmonitor, and commit
//! signing turned off so nothing in the repository runs code as a side effect.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use argonauts_core::DiffStat;
use argonauts_core::build_output::{BuildOutput, FilePatterns};
use tokio::process::Command;

mod absorb;
mod beside;
mod claimed_ignored;
mod first_commit;
mod leftovers;
mod refs;
mod your_folder;

pub use your_folder::FolderMerge;

/// Paths per `git rm`, well under any command-line limit.
const PATHS_PER_CALL: usize = 200;

/// The most removed files one commit looks for under a new name.
const MOVES_LOOKED_FOR: usize = 200;

const MB: u64 = 1024 * 1024;

/// Held while a call adds, removes, or prunes a worktree, or deletes a branch. Each of those
/// reads every worktree's records, and a worktree still being added has an empty one, which
/// fails the read. So this process makes such calls one at a time.
static WORKTREE_RECORDS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// Identity used for Argonauts Code's commits when the user has none configured.
const FALLBACK_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Argonauts Code"),
    ("GIT_AUTHOR_EMAIL", "argonauts@localhost"),
    ("GIT_COMMITTER_NAME", "Argonauts Code"),
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

/// What a commit would hold, read without making it: see [`Git::change_since`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// Every path the change adds, edits or removes.
    pub files: Vec<String>,
    /// The diff, each changed function shown whole.
    pub diff: String,
    /// The new files it leaves out, which stay in the worktree.
    pub left_out: Vec<LeftOut>,
}

/// An untracked file a commit left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub path: String,
    /// What it's named by when you're told: the folder it's in, ending in `/`, when that
    /// whole folder stayed out, or the file itself.
    pub because: String,
    pub why: LeftOutWhy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftOutWhy {
    /// The leave-out settings name it as build or cache output.
    BuildOutput,
    /// No agent meant to create it: no edit tool wrote it, and no report lists it.
    Unclaimed,
    /// Bigger than `commit.max_new_file_mb`, whoever made it.
    TooBig,
    /// An agent meant to create it, but your ignore rules match it.
    Ignored,
}

/// The new files agents meant to create in a worktree: the ones their edit tools wrote and
/// the ones their reports list, by path from the worktree. A listed folder covers the files
/// under it. The files `commit.always_keep` names count as claimed, whoever made them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claims {
    files: BTreeSet<String>,
    folders: Vec<String>,
    always: FilePatterns,
    /// Set when the agents' edits can't be seen: every new file is claimed but the build and
    /// cache output this list names.
    unseen: Option<BuildOutput>,
}

impl Claims {
    /// Claims every new file `patterns` name, such as `commit.always_keep`.
    pub fn always_keeping(mut self, patterns: &[String]) -> Self {
        self.always = FilePatterns::new(patterns);
        self
    }

    /// Claims every new file but what the built-in list names as build or cache output. For
    /// agents whose edit tools never reach the stream, such as the ones a CLI's own workflow
    /// starts in the background.
    pub fn every_new_file(mut self) -> Self {
        self.unseen = Some(BuildOutput::with(&[]));
        self
    }

    /// Claims `path`, given from the worktree or as an absolute path inside it. A path that
    /// leaves the worktree, or names all of it, claims nothing: `.` in a report would claim
    /// every cache a command left behind.
    pub fn add(&mut self, worktree: &Path, path: &str) {
        let given = Path::new(path.trim());
        let inside = if given.is_absolute() {
            // A CLI may name the file through a symlink the worktree's own path doesn't use,
            // or the other way round, as with `/tmp` on macOS.
            let real_folder = given.parent().and_then(|folder| folder.canonicalize().ok());
            let real = match (real_folder, given.file_name()) {
                (Some(folder), Some(name)) => folder.join(name),
                _ => given.to_path_buf(),
            };
            let root = worktree
                .canonicalize()
                .unwrap_or_else(|_| worktree.to_path_buf());
            match given
                .strip_prefix(worktree)
                .or_else(|_| real.strip_prefix(&root))
            {
                Ok(inside) => inside.to_path_buf(),
                Err(_) => return,
            }
        } else {
            given.to_path_buf()
        };
        let mut parts = Vec::new();
        for part in inside.components() {
            match part {
                std::path::Component::Normal(part) => parts.push(part.to_string_lossy()),
                std::path::Component::CurDir => {}
                _ => return,
            }
        }
        if parts.is_empty() {
            return;
        }
        let clean = parts.join("/");
        if path.trim().ends_with('/') || worktree.join(&clean).is_dir() {
            self.folders.push(format!("{clean}/"));
        } else {
            self.files.insert(clean);
        }
    }

    pub fn covers(&self, path: &str) -> bool {
        self.files.contains(path)
            || self.folders.iter().any(|folder| path.starts_with(folder))
            || self.always.contains(path)
            || self
                .unseen
                .as_ref()
                .is_some_and(|output| !output.contains(path))
    }

    /// Whether anything claimed sits in `folder`, which ends in `/`.
    fn reach_into(&self, folder: &str) -> bool {
        self.files.iter().any(|file| file.starts_with(folder))
            || self
                .folders
                .iter()
                .any(|claimed| claimed.starts_with(folder) || folder.starts_with(claimed))
    }
}

/// Makes `dir` an empty repository of its own, with no template and so no hooks, unless it is
/// one already. A CLI that looks upward for its project's files then stops at `dir`
/// instead of walking into folders its sandbox closes: OpenCode over ACP refused to start a
/// session in a no-tools scratch folder under Argonauts Code's home.
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
    /// A folder with no git of its own, and the hidden history kept for it.
    hidden: Option<Arc<Bound>>,
    known: Arc<Known>,
    /// What commits leave out while it's untracked.
    build_output: Arc<BuildOutput>,
    /// The most bytes a new file may hold to be committed. 0 is no limit.
    max_new_file: u64,
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
            hidden: self.hidden.clone(),
            known: self.known.clone(),
            build_output: self.build_output.clone(),
            max_new_file: self.max_new_file,
        }
    }

    /// The hidden history this git was bound to when the run started, for a folder that had
    /// no git of its own then. It stays the run's history whatever the folder becomes
    /// meanwhile, such as a repository after a `git init`.
    pub fn hidden_history(&self) -> Option<&Path> {
        self.hidden.as_deref().map(|bound| bound.git_dir.as_path())
    }

    /// Git that, for calls made in `folder` itself, uses the history at `git_dir` with the
    /// folder as its work tree. `folder` has no `.git`, so nothing else says where its history
    /// is. Worktrees made from that history find it through their own `.git` files as usual.
    pub fn with_hidden(mut self, folder: PathBuf, git_dir: PathBuf) -> Self {
        self.hidden = Some(Arc::new(Bound {
            git_dir,
            work_tree: folder,
        }));
        self
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

    /// Commits leave out a new file bigger than `megabytes`, as `commit.max_new_file_mb`
    /// asks. 0 lifts the limit.
    pub fn capping_new_files(mut self, megabytes: u64) -> Self {
        self.max_new_file = megabytes.saturating_mul(MB);
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
        self.run_on_worktrees(repo, &args).await.map(drop)
    }

    /// Discards anything in the worktree that wasn't committed.
    pub async fn remove_worktree(&self, repo: &Path, path: &Path) -> Result<(), GitError> {
        let path = path.to_string_lossy();
        self.run_on_worktrees(repo, &["worktree", "remove", "--force", &path])
            .await
            .map(drop)
    }

    pub async fn delete_branch(&self, repo: &Path, branch: &str) -> Result<(), GitError> {
        self.run_on_worktrees(repo, &["branch", "-D", branch])
            .await
            .map(drop)
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
        self.commit_claimed(worktree, message, None).await
    }

    /// Commits every change to a tracked file, and of the new files only the ones `claims`
    /// covers, or all of them without `claims`. What the leave-out settings name stays out
    /// either way. Says what it left out, which stays in the tree as it was.
    pub async fn commit_claimed(
        &self,
        worktree: &Path,
        message: &str,
        claims: Option<&Claims>,
    ) -> Result<Committed, GitError> {
        let left_out = self.stage_all(worktree, claims).await?;
        let sha = self.commit_index(worktree, message).await?;
        Ok(Committed { sha, left_out })
    }

    /// Stages everything but the untracked files the leave-out settings name and, with
    /// `claims`, the ones no agent claimed. Returns what it left out.
    async fn stage_all(
        &self,
        worktree: &Path,
        claims: Option<&Claims>,
    ) -> Result<Vec<LeftOut>, GitError> {
        let untracked = self.untracked_files(worktree).await?;
        let left_out = self.left_out_of(worktree, &untracked, claims).await?;
        if left_out.is_empty() {
            self.run(worktree, &["add", "--all"]).await?;
            return Ok(left_out);
        }
        // What goes in is named, where `add --all` would first hash everything that stays
        // out: a `node_modules/` a command left behind never reaches the object store.
        self.run(worktree, &["add", "--update"]).await?;
        let out: BTreeSet<&str> = left_out.iter().map(|left| left.path.as_str()).collect();
        let kept: Vec<&str> = untracked
            .iter()
            .map(String::as_str)
            .filter(|path| !out.contains(path))
            .collect();
        for chunk in kept.chunks(PATHS_PER_CALL) {
            let mut args = vec!["--literal-pathspecs", "add", "--"];
            args.extend(chunk);
            self.run(worktree, &args).await?;
        }
        Ok(left_out)
    }

    /// Takes the new files `commits` added that a commit of Argonauts Code's could leave out
    /// back out of the index, and leaves them in the tree, where the next commit sorts them
    /// like any file a command left behind: the ones `claims` doesn't cover, and whoever
    /// made them, the ones the leave-out settings name or that are too big. An agent's own
    /// commit is no claim: its `git add -A` sweeps in bytecode and caches as readily as work.
    pub async fn unstage_left_out(
        &self,
        worktree: &Path,
        commits: &[String],
        claims: Option<&Claims>,
    ) -> Result<(), GitError> {
        let mut added = BTreeSet::new();
        for sha in commits {
            // A file moved from a tracked name isn't new, and a merge lists nothing here, so
            // what it brought in from the run's branch stays.
            let args = [
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
                "--find-renames",
                "--diff-filter=A",
                sha,
            ];
            let listed = self.run(worktree, &args).await?;
            added.extend(
                listed
                    .split('\0')
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned),
            );
        }
        // Loosely: whatever goes is sorted again by the next commit, which puts back a file
        // the leave-out settings name in a folder the repository tracked before.
        let mut out: BTreeSet<&str> = added
            .iter()
            .filter(|path| {
                claims.is_some_and(|claims| !claims.covers(path))
                    || !self.build_output.matches(path).is_empty()
            })
            .map(String::as_str)
            .collect();
        let added: Vec<String> = added.iter().cloned().collect();
        let too_big = self.too_big(worktree, &added, &[]).await?;
        out.extend(too_big.iter().map(|left| left.path.as_str()));
        let out: Vec<&str> = out.into_iter().collect();
        for chunk in out.chunks(PATHS_PER_CALL) {
            // Forced, since the file may have changed since its commit. Only the index
            // entry goes: `--cached` never touches the file.
            let mut args = vec![
                "--literal-pathspecs",
                "rm",
                "--cached",
                "--force",
                "--quiet",
                "--ignore-unmatch",
                "--",
            ];
            args.extend(chunk);
            self.run(worktree, &args).await?;
        }
        Ok(())
    }

    /// The new files in `worktree` a commit under `claims` would leave out as nobody's, read
    /// without staging anything.
    pub async fn unclaimed_files(
        &self,
        worktree: &Path,
        claims: &Claims,
    ) -> Result<Vec<LeftOut>, GitError> {
        let untracked = self.untracked_files(worktree).await?;
        let mut left_out = self.left_out_of(worktree, &untracked, Some(claims)).await?;
        left_out.retain(|left| left.why == LeftOutWhy::Unclaimed);
        Ok(left_out)
    }

    /// Which of the `untracked` files a commit leaves out: what the leave-out settings name
    /// and, with `claims`, what no agent claimed.
    async fn left_out_of(
        &self,
        worktree: &Path,
        untracked: &[String],
        claims: Option<&Claims>,
    ) -> Result<Vec<LeftOut>, GitError> {
        let mut left_out = self
            .untracked_build_output(worktree, untracked.to_vec(), &self.build_output)
            .await?;
        let too_big = self.too_big(worktree, untracked, &left_out).await?;
        left_out.extend(too_big);
        let Some(claims) = claims else {
            return Ok(left_out);
        };
        if let Some(output) = &claims.unseen {
            let named: BTreeSet<&str> = left_out.iter().map(|left| left.path.as_str()).collect();
            let rest = untracked
                .iter()
                .filter(|path| !named.contains(path.as_str()))
                .cloned()
                .collect();
            let cache = self.untracked_build_output(worktree, rest, output).await?;
            left_out.extend(cache);
            return Ok(left_out);
        }
        left_out.extend(self.claimed_but_ignored(worktree, claims).await?);
        let unclaimed: Vec<String> = {
            let named: BTreeSet<&str> = left_out.iter().map(|left| left.path.as_str()).collect();
            untracked
                .iter()
                .filter(|path| !named.contains(path.as_str()) && !claims.covers(path))
                .cloned()
                .collect()
        };
        if unclaimed.is_empty() {
            return Ok(left_out);
        }
        let mut claims = claims.clone();
        // A folder that holds a file kept by a pattern isn't left out whole.
        let always_kept = untracked.iter().filter(|path| claims.always.contains(path));
        claims.files.extend(always_kept.cloned());
        claims
            .files
            .extend(self.moved_here(worktree, &unclaimed).await?);
        let unclaimed = unclaimed.into_iter().filter(|path| !claims.covers(path));
        left_out.extend(self.unclaimed(worktree, unclaimed, &claims).await?);
        Ok(left_out)
    }

    /// The files among `untracked`, less the ones `left_out` already, that are bigger than
    /// a commit takes. A size and never a type: what your `.gitattributes` give to Git LFS
    /// is stored outside the repository, so it's exempt.
    async fn too_big(
        &self,
        dir: &Path,
        untracked: &[String],
        left_out: &[LeftOut],
    ) -> Result<Vec<LeftOut>, GitError> {
        if self.max_new_file == 0 {
            return Ok(Vec::new());
        }
        let out: BTreeSet<&str> = left_out.iter().map(|left| left.path.as_str()).collect();
        let mut too_big = Vec::new();
        for path in untracked.iter().filter(|path| !out.contains(path.as_str())) {
            let size = std::fs::symlink_metadata(dir.join(path))
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map_or(0, |meta| meta.len());
            if size <= self.max_new_file {
                continue;
            }
            let filter = ["check-attr", "-z", "filter", "--", path];
            if self.run(dir, &filter).await?.split('\0').nth(2) == Some("lfs") {
                continue;
            }
            too_big.push(LeftOut {
                path: path.clone(),
                because: format!("{path} ({} MB)", size / MB),
                why: LeftOutWhy::TooBig,
            });
        }
        Ok(too_big)
    }

    /// The files among `new` that hold the bytes of a tracked file now gone from the
    /// worktree: a file a command moved, such as `mv old.py new.py`. No edit tool wrote the
    /// new name, but leaving it out would commit the removal alone and lose the file.
    async fn moved_here(&self, dir: &Path, new: &[String]) -> Result<BTreeSet<String>, GitError> {
        let args = [
            "diff-index",
            "--raw",
            "-z",
            "--no-abbrev",
            "--no-renames",
            "--diff-filter=D",
            "HEAD",
        ];
        // Each removal reads `:<mode> <mode> <blob> <blob> D`, then its path. Only a plain
        // file can come back as one, and only one that's gone: a file taken out of the index
        // and still in the tree under its own name was unstaged, not moved.
        let removed = self.run(dir, &args).await?;
        let mut fields = removed.split('\0');
        let mut blobs = BTreeSet::new();
        while let (Some(entry), Some(path)) = (fields.next(), fields.next()) {
            let gone = std::fs::symlink_metadata(dir.join(path)).is_err();
            if blobs.len() < MOVES_LOOKED_FOR && gone && entry.starts_with(":100") {
                blobs.extend(entry.split(' ').nth(2));
            }
        }
        if blobs.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut sizes = BTreeSet::new();
        for blob in &blobs {
            if let Ok(size) = self.run(dir, &["cat-file", "-s", blob]).await {
                sizes.extend(size.parse::<u64>());
            }
        }
        // Only a file as long as one that went can be it, so a folder of thousands of new
        // files costs a look at each one's size and no more.
        let mut moved = BTreeSet::new();
        for path in new {
            let same_size = std::fs::symlink_metadata(dir.join(path))
                .is_ok_and(|meta| meta.is_file() && sizes.contains(&meta.len()));
            if !same_size {
                continue;
            }
            let args = ["hash-object", "--", path];
            if blobs.contains(self.run(dir, &args).await?.as_str()) {
                moved.insert(path.clone());
            }
        }
        Ok(moved)
    }

    /// New files nobody claimed, each named by its top folder when that folder is new and
    /// nothing claimed sits in it: the whole folder stayed out, so it's told once, however
    /// many files it holds.
    async fn unclaimed(
        &self,
        dir: &Path,
        paths: impl Iterator<Item = String>,
        claims: &Claims,
    ) -> Result<Vec<LeftOut>, GitError> {
        let mut whole_folders: HashMap<String, bool> = HashMap::new();
        let mut left_out = Vec::new();
        for path in paths {
            let top = path.split_once('/').map(|(top, _)| top.to_owned());
            let mut because = path.clone();
            if let Some(top) = top {
                let folder = format!("{top}/");
                let whole = match whole_folders.get(&folder) {
                    Some(whole) => *whole,
                    None => {
                        let whole =
                            !claims.reach_into(&folder) && !self.is_tracked(dir, &top).await?;
                        whole_folders.insert(folder.clone(), whole);
                        whole
                    }
                };
                if whole {
                    because = folder;
                }
            }
            left_out.push(LeftOut {
                path,
                because,
                why: LeftOutWhy::Unclaimed,
            });
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
        output: &BuildOutput,
    ) -> Result<Vec<LeftOut>, GitError> {
        let mut tracked_folders: HashMap<String, bool> = HashMap::new();
        let mut left_out = Vec::new();
        for path in untracked {
            for found in output.matches(&path) {
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
                left_out.push(LeftOut {
                    path,
                    because,
                    why: LeftOutWhy::BuildOutput,
                });
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
    /// like any added line instead of stopping the task as unreadable.
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

    /// Git whose commands use the index at `index` instead of the worktree's own, so what they
    /// stage leaves the worktree and its index as they were.
    pub fn with_index(&self, index: &Path) -> Self {
        let mut git = self.clone();
        let index = index.to_string_lossy().into_owned();
        git.env.push(("GIT_INDEX_FILE".to_owned(), index));
        git
    }

    /// What a commit in `worktree` would hold now, against `from`, read through a throwaway
    /// index at `index`: the same sorting [`Git::commit_claimed`] does, after the same
    /// [`Git::unstage_left_out`] of what the agent's own `commits` swept in. So what reviewers
    /// read before the commit is exactly what the commit then holds.
    pub async fn change_since(
        &self,
        worktree: &Path,
        from: &str,
        index: &Path,
        commits: &[String],
        claims: Option<&Claims>,
    ) -> Result<Change, GitError> {
        let _ = std::fs::remove_file(index);
        let scratch = self.with_index(index);
        let staged = async {
            scratch.run(worktree, &["read-tree", "HEAD"]).await?;
            scratch.unstage_left_out(worktree, commits, claims).await?;
            let left_out = scratch.stage_all(worktree, claims).await?;
            let names = [
                "diff",
                "--cached",
                "--name-only",
                "-z",
                "--no-renames",
                from,
            ];
            let files = scratch.run(worktree, &names).await?;
            let args = [
                "diff",
                "--cached",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--function-context",
                from,
            ];
            let out = scratch.output(worktree, &args, &[]).await?;
            if !out.status.success() {
                return Err(failed(&args, &out));
            }
            Ok(Change {
                files: files
                    .split('\0')
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect(),
                diff: String::from_utf8_lossy(&out.stdout).into_owned(),
                left_out,
            })
        }
        .await;
        let _ = std::fs::remove_file(index);
        staged
    }

    /// A commit of what [`Git::change_since`] reads, on top of `HEAD` and on no branch, so the
    /// change as it stands can be checked out somewhere clean.
    pub async fn commit_of_change(
        &self,
        worktree: &Path,
        index: &Path,
        commits: &[String],
        claims: Option<&Claims>,
    ) -> Result<String, GitError> {
        let _ = std::fs::remove_file(index);
        let scratch = self.with_index(index);
        let made = async {
            scratch.run(worktree, &["read-tree", "HEAD"]).await?;
            scratch.unstage_left_out(worktree, commits, claims).await?;
            scratch.stage_all(worktree, claims).await?;
            let tree = scratch.run(worktree, &["write-tree"]).await?;
            let args = [
                "commit-tree",
                tree.trim(),
                "-p",
                "HEAD",
                "-m",
                "The change as it stands",
            ];
            let commit = scratch.run(worktree, &args).await?;
            Ok(commit.trim().to_owned())
        }
        .await;
        let _ = std::fs::remove_file(index);
        made
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

    async fn run_on_worktrees(&self, repo: &Path, args: &[&str]) -> Result<String, GitError> {
        let _one_at_a_time = WORKTREE_RECORDS.lock().await;
        self.run(repo, args).await
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
        let hidden = self
            .hidden
            .as_deref()
            .filter(|hidden| hidden.work_tree == dir);
        if let Some(bound) = self.bound.as_ref().or(hidden) {
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
        self.run_on_worktrees(repo, &args).await.map(drop)
    }

    /// Puts the worktree at `dir` back to exactly `rev`, detached: each tracked file as `rev`
    /// holds it and every untracked file gone. What git ignores stays, such as dependencies a
    /// setup command installed.
    pub async fn restore_clean(&self, dir: &Path, rev: &str) -> Result<(), GitError> {
        let checkout = ["checkout", "--quiet", "--detach", "--force", rev];
        self.run(dir, &checkout).await?;
        self.run(dir, &["clean", "-ffdq"]).await.map(drop)
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
        self.run_on_worktrees(repo, &args).await.map(drop)
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
    /// exist nor are tracked are skipped, and so are the ones git ignores: they stay on disk
    /// and out of the commit, never forced in.
    pub async fn commit_paths(
        &self,
        worktree: &Path,
        paths: &[&str],
        message: &str,
    ) -> Result<Option<String>, GitError> {
        let ignored = self.ignored(worktree, paths).await?;
        let mut present = Vec::new();
        for path in paths {
            if ignored.iter().any(|left| left == path) {
                continue;
            }
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

    /// Which of `paths` git ignores, as given. A tracked file is never among them, since
    /// ignore rules don't apply to what git already tracks.
    pub async fn ignored(&self, worktree: &Path, paths: &[&str]) -> Result<Vec<String>, GitError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["check-ignore", "--"];
        args.extend_from_slice(paths);
        let out = self.output(worktree, &args, &[]).await?;
        match out.status.code() {
            Some(0) => Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_owned)
                .collect()),
            // None of them is ignored.
            Some(1) => Ok(Vec::new()),
            _ => Err(failed(&args, &out)),
        }
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
        Ok(self.finish_merge_work(worktree, message, None).await?.0)
    }

    /// [`Git::finish_merge`], taking of the new files what [`Git::commit_claimed`] takes, and
    /// saying which it left out.
    pub async fn finish_merge_work(
        &self,
        worktree: &Path,
        message: &str,
        claims: Option<&Claims>,
    ) -> Result<(String, Vec<LeftOut>), GitError> {
        let left_out = self.stage_all(worktree, claims).await?;
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

    /// Puts every tracked file back as HEAD holds it, staged or not. Untracked files stay.
    pub async fn discard_tracked_changes(&self, dir: &Path) -> Result<(), GitError> {
        self.run(dir, &["reset", "--quiet", "--hard", "HEAD"])
            .await
            .map(drop)
    }

    /// Commits the merge in progress with the tracked files as they are now and `resolved`
    /// added, the files a conflict left. Anything else untracked stays out, such as what an
    /// earlier check left in the tree.
    pub async fn finish_merge_of(
        &self,
        worktree: &Path,
        resolved: &[String],
        message: &str,
    ) -> Result<String, GitError> {
        self.run(worktree, &["add", "--update"]).await?;
        for chunk in resolved.chunks(PATHS_PER_CALL) {
            let mut args = vec!["--literal-pathspecs", "add", "--all", "--"];
            args.extend(chunk.iter().map(String::as_str));
            self.run(worktree, &args).await?;
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
        self.resolve(worktree, "HEAD").await
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

    /// A folder with no git works through a history kept elsewhere: commits and worktrees
    /// come from it, a merge writes the folder, and the folder never gets a `.git`.
    #[tokio::test]
    async fn a_folder_without_git_works_through_its_hidden_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (folder, hidden) = (root.join("folder"), root.join("history.git"));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("notes.md"), "one\n").unwrap();
        let made = std::process::Command::new("git")
            .args(["init", "--quiet", "--bare", "--template="])
            .arg(&hidden)
            .status()
            .unwrap();
        assert!(made.success());
        std::fs::write(hidden.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        assert!(
            git().head_commit(&folder).await.is_err(),
            "no git in the folder"
        );
        let git = git().with_hidden(folder.clone(), hidden.clone());
        let base = git
            .commit_all(&folder, "the folder")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            git.current_branch(&folder).await.unwrap().as_deref(),
            Some("main")
        );

        let tree = root.join("worktrees/task");
        git.add_worktree(&folder, &tree, "argonauts/run/task", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("notes.md"), "two\n").unwrap();
        std::fs::write(tree.join("new.txt"), "new\n").unwrap();
        let work = git.commit_all(&tree, "work").await.unwrap().unwrap();
        // Bound to the worktree's own git folder, as the engine does before an agent runs.
        let own = git.absolute_git_dir(&tree).await.unwrap();
        assert!(own.starts_with(&hidden), "{}", own.display());
        assert!(
            git.bound_to(own, tree.clone())
                .is_clean(&tree)
                .await
                .unwrap()
        );

        std::fs::write(folder.join("mine.txt"), "yours stays\n").unwrap();
        assert!(
            git.in_the_way(&folder, &base, &work)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(git.fast_forward(&folder, &work).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(folder.join("notes.md")).unwrap(),
            "two\n"
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("new.txt")).unwrap(),
            "new\n"
        );
        assert!(folder.join("mine.txt").is_file());
        assert!(!folder.join(".git").exists());
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
        repo.git(&["checkout", "-qb", "merge"]);
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

    /// Before a first commit your index lists every file you staged as a change, and never
    /// held the snapshot a run started from. So what's in the way is found by comparing the
    /// files themselves with that snapshot.
    #[tokio::test]
    async fn before_a_first_commit_only_files_that_are_different_from_the_snapshot_are_in_the_way()
    {
        let repo = TestRepo::new();
        let git = git();
        repo.write("store.rs", "fn get() {}\n");
        repo.write("old.rs", "fn old() {}\n");
        repo.write("notes.md", "mine\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-qm", "base"]);
        repo.git(&["checkout", "-qb", "run"]);
        repo.write("store.rs", "fn get() {}\nfn put() {}\n");
        repo.git(&["mv", "old.rs", "older.rs"]);
        repo.git(&["commit", "-qam", "run"]);
        let run = git.head_commit(&repo.root).await.unwrap().unwrap();

        // Your files as the run found them, with nothing on your branch and all of it staged.
        repo.git(&["checkout", "-q", "main"]);
        let tree = git.tree_of(&repo.root, "HEAD").await.unwrap();
        let snapshot = repo.git(&["commit-tree", &tree, "-m", "your files"]);
        let snapshot = snapshot.trim();
        repo.git(&["update-ref", "refs/argonauts/run-01/base", snapshot]);
        repo.git(&["update-ref", "-d", "refs/heads/main"]);
        assert_eq!(git.head_commit(&repo.root).await.unwrap(), None);
        let kept = git.ref_commit(&repo.root, "refs/argonauts/run-01/base");
        assert_eq!(kept.await.unwrap().as_deref(), Some(snapshot));
        let none = git.ref_commit(&repo.root, "refs/argonauts/run-02/base");
        assert_eq!(none.await.unwrap(), None);

        let scratch = repo.root.join(".git/in-the-way.index");
        let in_the_way = || git.differing_in_the_way(&repo.root, &scratch, snapshot, &run);
        assert!(in_the_way().await.unwrap().is_empty());
        repo.write("notes.md", "mine, edited\n");
        assert!(
            in_the_way().await.unwrap().is_empty(),
            "an edit to a file the run leaves alone rides along"
        );
        repo.write("store.rs", "fn get() { todo!() }\n");
        std::fs::remove_file(repo.root.join("old.rs")).unwrap();
        assert_eq!(in_the_way().await.unwrap(), ["old.rs", "store.rs"]);
        assert!(!scratch.exists());

        let landing = repo.root.join(".git/landing.index");
        assert!(
            git.write_files(&repo.root, &landing, snapshot, &run, &[])
                .await
                .is_err()
        );
        repo.write("store.rs", "fn get() {}\n");
        repo.write("old.rs", "fn old() {}\n");
        let written = || git.holds_change(&repo.root, &scratch, snapshot, &run);
        assert!(!written().await.unwrap());
        git.write_files(&repo.root, &landing, snapshot, &run, &[])
            .await
            .unwrap();
        assert!(repo.root.join("older.rs").exists() && !repo.root.join("old.rs").exists());
        assert_eq!(git.head_commit(&repo.root).await.unwrap(), None);

        // Written once, the run's own work is told from an edit of yours in its way.
        assert!(written().await.unwrap());
        repo.write("old.rs", "fn old() {}\n");
        assert!(!written().await.unwrap(), "a file it removes is back");
        std::fs::remove_file(repo.root.join("old.rs")).unwrap();
        repo.write("store.rs", "fn get() { todo!() }\n");
        assert!(!written().await.unwrap(), "a file it wrote was edited");
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
                because: "textkit/__pycache__/".into(),
                why: LeftOutWhy::BuildOutput,
            }]
        );
    }

    /// The sample run's commit, without a list of file types: the agent's edit tool wrote one
    /// file and its report lists another, and its tests left bytecode and a cache beside
    /// them. Only what it claimed is committed, with every change to a tracked file.
    #[tokio::test]
    async fn a_commit_takes_tracked_changes_and_only_the_new_files_an_agent_claimed() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("textkit/__init__.py", "");
        git.commit_all(&repo.root, "start").await.unwrap();

        repo.write("README.md", "# Changed\n");
        repo.write("textkit/steps.py", "def steps(): ...\n");
        repo.write("textkit/gen/schema.py", "SCHEMA = {}\n");
        repo.write("textkit/__pycache__/steps.cpython-312.pyc", "bytecode");
        repo.write(".pytest_cache/v/cache/lastfailed", "{}");
        repo.write(".pytest_cache/README.md", "cache");
        repo.write("coverage.xml", "<coverage/>");

        let mut claims = Claims::default();
        // An edit tool names the file by its full path, and a report by its path in the repo.
        claims.add(
            &repo.root,
            &repo.root.join("textkit/steps.py").to_string_lossy(),
        );
        claims.add(&repo.root, "textkit/gen/");
        for nothing in [".", "/", "../elsewhere.py", "/etc/passwd", ""] {
            claims.add(&repo.root, nothing);
        }
        assert!(claims.covers("textkit/steps.py") && claims.covers("textkit/gen/schema.py"));
        assert!(!claims.covers("coverage.xml") && !claims.covers("README.md"));

        let committed = git
            .commit_claimed(&repo.root, "work", Some(&claims))
            .await
            .unwrap();
        let sha = committed.sha.unwrap();
        let mut files = git
            .changed_paths(&repo.root, &format!("{sha}~1"), &sha)
            .await
            .unwrap();
        files.sort();
        assert_eq!(
            files,
            ["README.md", "textkit/gen/schema.py", "textkit/steps.py"]
        );
        assert!(
            committed
                .left_out
                .iter()
                .all(|left| left.why == LeftOutWhy::Unclaimed)
        );
        let told: BTreeSet<&str> = committed
            .left_out
            .iter()
            .map(|left| left.because.as_str())
            .collect();
        assert_eq!(
            told.into_iter().collect::<Vec<_>>(),
            [
                // A new folder nothing claimed is named once, whatever it holds.
                ".pytest_cache/",
                "coverage.xml",
                // A folder the repo tracks names the file, never the whole folder.
                "textkit/__pycache__/steps.cpython-312.pyc",
            ]
        );
        // What stayed out is still there for the next session and the checks.
        assert!(repo.root.join("coverage.xml").exists());
        assert!(repo.root.join(".pytest_cache/README.md").exists());

        // Nothing that stayed out was hashed on the way, so a folder of thousands of files a
        // command left behind costs the repository nothing.
        let id = repo.git(&["hash-object", "coverage.xml"]);
        let stored = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo.root)
            .args(["cat-file", "-e", id.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!stored.success(), "coverage.xml reached the object store");

        // Without claims every new file goes in, as `commit.unclaimed = "keep"` asks.
        let kept = git.commit_claimed(&repo.root, "all", None).await.unwrap();
        assert!(kept.sha.is_some() && kept.left_out.is_empty());
    }

    /// A file a command moved keeps its place in the commit: no edit tool wrote the new
    /// name, but it holds the bytes of the tracked file that went. A file with other bytes
    /// beside it is still nobody's.
    /// `commit.always_keep` names files a tool outside the agent's view makes and the project
    /// needs. They go in whoever made them, and what sits beside them is still sorted.
    #[tokio::test]
    async fn files_the_settings_always_keep_are_committed_whoever_made_them() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("Assets/Player.cs", "class Player {}\n");
        repo.write("Assets/Player.cs.meta", "guid: 1\n");
        repo.write("Assets/Editor.log", "noise\n");
        repo.write("Library/cache.bin", "cache");

        let mut claims = Claims::default().always_keeping(&["*.META".to_owned()]);
        claims.add(&repo.root, "Assets/Player.cs");
        let committed = git
            .commit_claimed(&repo.root, "work", Some(&claims))
            .await
            .unwrap();
        let sha = committed.sha.unwrap();
        let mut files = git
            .changed_paths(&repo.root, &format!("{sha}~1"), &sha)
            .await
            .unwrap();
        files.sort();
        assert_eq!(files, ["Assets/Player.cs", "Assets/Player.cs.meta"]);
        let told: Vec<&str> = committed
            .left_out
            .iter()
            .map(|left| left.because.as_str())
            .collect();
        // `Assets/` holds kept files, so only the file is named there.
        assert_eq!(told, ["Assets/Editor.log", "Library/"]);
    }

    /// A size and never a type: a new file over the cap stays out even when an agent claimed
    /// it, and is named with its size. What `.gitattributes` gives to Git LFS is exempt.
    #[tokio::test]
    async fn a_new_file_over_the_size_cap_is_left_out_and_named_with_its_size() {
        let repo = TestRepo::new();
        let git = git().capping_new_files(1);
        repo.write(".gitattributes", "*.psd filter=lfs\n");
        git.commit_all(&repo.root, "attributes").await.unwrap();
        let big = "x".repeat(2 * 1024 * 1024);
        repo.write("model.bin", &big);
        repo.write("art/cover.psd", &big);
        repo.write("notes.md", "small\n");

        let mut claims = Claims::default();
        for claimed in ["model.bin", "art/cover.psd", "notes.md"] {
            claims.add(&repo.root, claimed);
        }
        let committed = git
            .commit_claimed(&repo.root, "work", Some(&claims))
            .await
            .unwrap();
        let sha = committed.sha.unwrap();
        let mut files = git
            .changed_paths(&repo.root, &format!("{sha}~1"), &sha)
            .await
            .unwrap();
        files.sort();
        assert_eq!(files, ["art/cover.psd", "notes.md"]);
        let [left] = committed.left_out.as_slice() else {
            panic!("{:?}", committed.left_out);
        };
        assert_eq!(left.why, LeftOutWhy::TooBig);
        assert_eq!(left.because, "model.bin (2 MB)");
        assert!(repo.root.join("model.bin").exists());

        // With no claims every new file goes in, and the cap still holds.
        let kept = git.commit_work(&repo.root, "all of it").await.unwrap();
        assert_eq!(kept.sha, None);
        assert_eq!(kept.left_out.len(), 1);
    }

    /// What reviewers read before the commit is what the commit then holds: the agent's
    /// files and edits, never the bytecode beside them, and reading it touches nothing.
    #[tokio::test]
    async fn the_change_read_before_the_commit_is_the_commit() {
        let repo = TestRepo::new();
        let git = listing();
        repo.write("money.py", "def total(): return 1\n");
        git.commit_all(&repo.root, "start").await.unwrap();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();

        repo.write("money.py", "def total(): return 2\n");
        repo.write("rounding.py", "def round_half(): ...\n");
        repo.write("stray.txt", "a command made this");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-qm", "the agent's own"]);
        let own = git.head_commit(&repo.root).await.unwrap().unwrap();
        repo.write("tests/test_money.py", "def test_total(): ...\n");
        repo.write("__pycache__/money.cpython-312.pyc", "bytecode");
        repo.write("tests/__pycache__/test_money.cpython-312.pyc", "bytecode");

        let mut claims = Claims::default();
        for path in ["rounding.py", "tests/test_money.py"] {
            claims.add(&repo.root, path);
        }
        let index = repo.root.join(".git").join("scratch-index");
        let read = git
            .change_since(
                &repo.root,
                &base,
                &index,
                std::slice::from_ref(&own),
                Some(&claims),
            )
            .await
            .unwrap();
        assert_eq!(
            read.files,
            ["money.py", "rounding.py", "tests/test_money.py"]
        );
        assert!(!read.diff.contains("pyc"), "{}", read.diff);
        assert!(!read.diff.contains("stray"), "{}", read.diff);
        assert!(!index.exists());
        assert_eq!(repo.git(&["diff", "--cached", "--name-only"]), "");

        git.unstage_left_out(&repo.root, std::slice::from_ref(&own), Some(&claims))
            .await
            .unwrap();
        let sha = git
            .commit_claimed(&repo.root, "the session's", Some(&claims))
            .await
            .unwrap()
            .sha
            .unwrap();
        let committed = git.review_diff(&repo.root, &base, &sha).await.unwrap();
        assert_eq!(read.diff, committed);
    }

    /// An agent ran `git add -A` and committed: its work, what a command generated, and the
    /// bytecode its tests left. The sort takes the unclaimed files back out of the index and
    /// the next commit out of the branch, and all of them stay in the tree.
    #[tokio::test]
    async fn what_a_commit_swept_in_unclaimed_is_taken_back_out_and_stays_on_disk() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("textkit/__init__.py", "");
        repo.write("textkit/old.py", "def old(): ...\n");
        git.commit_all(&repo.root, "start").await.unwrap();

        repo.write("textkit/steps.py", "def steps(): ...\n");
        repo.write("textkit/__pycache__/steps.cpython-312.pyc", "bytecode");
        repo.write(".coverage", "data");
        repo.git(&["mv", "textkit/old.py", "textkit/older.py"]);
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-qm", "the agent's own"]);
        let own = git.head_commit(&repo.root).await.unwrap().unwrap();
        // Changed since its commit, which a plain `git rm --cached` refuses.
        repo.write(".coverage", "data, again");

        let mut claims = Claims::default();
        claims.add(&repo.root, "textkit/steps.py");
        git.unstage_left_out(&repo.root, std::slice::from_ref(&own), Some(&claims))
            .await
            .unwrap();
        let committed = git
            .commit_claimed(&repo.root, "Argonauts Code's", Some(&claims))
            .await
            .unwrap();
        let sha = committed.sha.unwrap();

        let held = repo.git(&["ls-tree", "-r", "--name-only", &sha]);
        let held: Vec<&str> = held.lines().collect();
        assert_eq!(
            held,
            [
                "README.md",
                "textkit/__init__.py",
                // Moved from a tracked name, so it was never new.
                "textkit/older.py",
                "textkit/steps.py"
            ]
        );
        let told: BTreeSet<&str> = committed
            .left_out
            .iter()
            .map(|left| left.path.as_str())
            .collect();
        assert_eq!(
            told.into_iter().collect::<Vec<_>>(),
            [".coverage", "textkit/__pycache__/steps.cpython-312.pyc"]
        );
        for kept in [".coverage", "textkit/__pycache__/steps.cpython-312.pyc"] {
            assert!(repo.root.join(kept).exists(), "{kept} left the tree");
        }
        assert_eq!(
            std::fs::read_to_string(repo.root.join(".coverage")).unwrap(),
            "data, again"
        );
    }

    /// Whoever made a new file, a commit leaves it out when it's too big or the leave-out
    /// settings name it. An agent's own commit is sorted the same way, claims or none, and
    /// a file the settings name in a folder the repository already tracks goes back in.
    #[tokio::test]
    async fn what_a_commit_swept_in_too_big_or_left_out_is_taken_back_out() {
        let repo = TestRepo::new();
        let git = git()
            .capping_new_files(1)
            .leaving_out(false, &["dist/".to_owned()]);
        repo.write("web/dist/keep.txt", "the project's own\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-qm", "start"]);

        repo.write("model.bin", &"x".repeat(2 * 1024 * 1024));
        repo.write("dist/app.js", "built\n");
        repo.write("web/dist/more.txt", "also the project's\n");
        repo.write("src/app.rs", "fn app() {}\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-qm", "the agent's own"]);
        let own = git.head_commit(&repo.root).await.unwrap().unwrap();

        git.unstage_left_out(&repo.root, std::slice::from_ref(&own), None)
            .await
            .unwrap();
        let committed = git
            .commit_claimed(&repo.root, "Argonauts Code's", None)
            .await
            .unwrap();
        let sha = committed.sha.unwrap();
        let held = repo.git(&["ls-tree", "-r", "--name-only", &sha]);
        let held: Vec<&str> = held.lines().collect();
        assert_eq!(
            held,
            [
                "README.md",
                "src/app.rs",
                "web/dist/keep.txt",
                "web/dist/more.txt"
            ]
        );
        let mut told: Vec<(&str, LeftOutWhy)> = committed
            .left_out
            .iter()
            .map(|left| (left.path.as_str(), left.why))
            .collect();
        told.sort_by_key(|(path, _)| *path);
        assert_eq!(
            told,
            [
                ("dist/app.js", LeftOutWhy::BuildOutput),
                ("model.bin", LeftOutWhy::TooBig)
            ]
        );
        assert!(repo.root.join("model.bin").exists());
    }

    #[tokio::test]
    async fn a_file_moved_by_a_command_goes_in_under_its_new_name() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("old/steps.py", "def steps(): ...\n");
        repo.write("keep.py", "kept = True\n");
        git.commit_all(&repo.root, "start").await.unwrap();

        std::fs::create_dir_all(repo.root.join("lib")).unwrap();
        std::fs::rename(
            repo.root.join("old/steps.py"),
            repo.root.join("lib/steps.py"),
        )
        .unwrap();
        repo.write("lib/steps.pyc", "def steps(): !!!\n");

        let claims = Claims::default();
        let left = git.unclaimed_files(&repo.root, &claims).await.unwrap();
        let named: Vec<&str> = left.iter().map(|left| left.because.as_str()).collect();
        assert_eq!(named, ["lib/steps.pyc"], "read without staging anything");
        assert_eq!(repo.git(&["diff", "--cached", "--name-only"]).trim(), "");

        let committed = git
            .commit_claimed(&repo.root, "move", Some(&claims))
            .await
            .unwrap();
        let sha = committed.sha.unwrap();
        let changed = repo.git(&["show", "--name-status", "--no-renames", "--format=", &sha]);
        assert_eq!(changed, "A\tlib/steps.py\nD\told/steps.py");
        assert_eq!(committed.left_out.len(), 1);
    }

    /// Checks run on the run's branch with a merge staged, and what they leave there is
    /// nobody's work. The merge commit takes what the two sides committed.
    #[tokio::test]
    async fn a_merge_finished_with_no_claims_takes_no_file_a_check_left_behind() {
        let repo = TestRepo::new();
        let git = git();
        repo.write("main.py", "print('main')\n");
        git.commit_all(&repo.root, "start").await.unwrap();
        repo.git(&["checkout", "-q", "-b", "task"]);
        repo.write("side.py", "print('side')\n");
        git.commit_all(&repo.root, "task").await.unwrap();
        repo.git(&["checkout", "-q", "-"]);

        let staged = git.merge_staged(&repo.root, "task", false).await.unwrap();
        assert_eq!(staged, StagedMerge::Staged);
        repo.write("__pycache__/main.cpython-312.pyc", "bytecode");
        let (sha, left_out) = git
            .finish_merge_work(&repo.root, "merge task", Some(&Claims::default()))
            .await
            .unwrap();
        assert_eq!(
            git.changed_paths(&repo.root, &format!("{sha}~1"), &sha)
                .await
                .unwrap(),
            ["side.py"]
        );
        assert_eq!(left_out.len(), 1);
        assert!(repo.root.join("__pycache__/main.cpython-312.pyc").exists());
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
        let tree = repo.dir.path().join("trees/run/task");
        git.add_worktree(&repo.root, &tree, "argonauts/run/task", &base)
            .await
            .unwrap();
        assert_eq!(
            git.current_branch(&tree).await.unwrap().as_deref(),
            Some("argonauts/run/task")
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

        git.merge(&repo.root, &sha, "Merge the task").await.unwrap();
        assert_eq!(git.head_commit(&repo.root).await.unwrap().unwrap(), sha);
        assert!(repo.root.join("hello.txt").exists());

        git.remove_worktree(&repo.root, &tree).await.unwrap();
        assert!(!tree.exists());
        git.delete_branch(&repo.root, "argonauts/run/task")
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
        git.add_worktree(&repo.root, &tree, "task", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("README.md"), "theirs\n").unwrap();
        git.commit_all(&tree, "theirs").await.unwrap();
        repo.write("README.md", "ours\n");
        repo.git(&["commit", "--quiet", "-am", "ours"]);

        let err = git
            .merge(&repo.root, "task", "Merge task")
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
            "Argonauts Code <argonauts@localhost>"
        );
    }

    #[tokio::test]
    async fn a_rewritten_dot_git_file_is_ignored_when_bound() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let tree = repo.dir.path().join("tree");
        git.add_worktree(&repo.root, &tree, "task", &base)
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
        std::fs::write(fake.join("HEAD"), "ref: refs/heads/task\n").unwrap();
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
            Some("task")
        );
        let sha = bound.commit_all(&tree, "hello").await.unwrap().unwrap();
        assert!(!marker.exists(), "the fake repository's filter ran");
        assert_eq!(git.resolve(&repo.root, "task").await.unwrap(), sha);
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
            git.branch_head(&repo.root, "argonauts/r/merged")
                .await
                .unwrap(),
            None
        );
        git.create_branch(&repo.root, "argonauts/r/merged", &base)
            .await
            .unwrap();
        assert_eq!(
            git.branch_head(&repo.root, "argonauts/r/merged")
                .await
                .unwrap()
                .as_deref(),
            Some(base.as_str())
        );
        assert_eq!(
            git.current_branch(&repo.root).await.unwrap().as_deref(),
            Some("main")
        );

        let tree = repo.dir.path().join("trees/merged");
        git.add_worktree_on(&repo.root, &tree, "argonauts/r/merged")
            .await
            .unwrap();
        assert_eq!(
            git.current_branch(&tree).await.unwrap().as_deref(),
            Some("argonauts/r/merged")
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

    /// A repository that ignores `.argonauts/`: the run's own files stay on disk and out of
    /// the commit, and nothing fails.
    #[tokio::test]
    async fn an_ignored_argonauts_folder_stays_out_of_the_commit() {
        let repo = TestRepo::new();
        let git = git();
        repo.write(".gitignore", ".argonauts/\n");
        repo.git(&["add", ".gitignore"]);
        repo.git(&["commit", "-q", "-m", "ignore it"]);
        repo.write(".argonauts/goal/run-1.md", "goal\n");
        repo.write("notes.md", "notes\n");
        assert_eq!(
            git.ignored(&repo.root, &[".argonauts/goal/run-1.md", "notes.md"])
                .await
                .unwrap(),
            [".argonauts/goal/run-1.md"]
        );
        assert_eq!(
            git.commit_paths(&repo.root, &[".argonauts/goal/run-1.md"], "Goal")
                .await
                .unwrap(),
            None,
            "nothing to commit, and no error"
        );
        let sha = git
            .commit_paths(
                &repo.root,
                &[".argonauts/goal/run-1.md", "notes.md"],
                "Both",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            repo.git(&["show", "--name-only", "--format=", &sha]),
            "notes.md"
        );
        assert!(repo.root.join(".argonauts/goal/run-1.md").exists());
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
        git.add_worktree(&repo.root, &tree, "task", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("a.txt"), "a\n").unwrap();
        let task = git.commit_all(&tree, "task work").await.unwrap().unwrap();

        let MergeOutcome::Merged(merge) = git
            .merge_no_ff(&repo.root, "task", "Merge task-001")
            .await
            .unwrap()
        else {
            panic!("expected a merge");
        };
        assert_ne!(merge, task, "fast-forwarded");
        let parents = repo.git(&["log", "-1", "--format=%P", &merge]);
        assert_eq!(parents, format!("{base} {task}"));
        assert_eq!(
            repo.git(&["log", "-1", "--format=%s", &merge]),
            "Merge task-001"
        );
    }

    #[tokio::test]
    async fn merge_no_ff_reports_conflicts_and_aborts() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let tree = repo.dir.path().join("tree");
        git.add_worktree(&repo.root, &tree, "task", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("README.md"), "theirs\n").unwrap();
        git.commit_all(&tree, "theirs").await.unwrap();
        repo.write("README.md", "ours\n");
        repo.git(&["commit", "--quiet", "-am", "ours"]);
        let head = git.head_commit(&repo.root).await.unwrap().unwrap();

        let outcome = git
            .merge_no_ff(&repo.root, "task", "Merge task")
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
        git.add_worktree(&repo.root, &tree, "task", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("a.txt"), "a\n").unwrap();
        let task = git.commit_all(&tree, "task").await.unwrap().unwrap();

        let staged = git.merge_staged(&repo.root, "task", false).await.unwrap();
        assert_eq!(staged, StagedMerge::Staged);
        assert!(git.merge_in_progress(&repo.root).await.unwrap());
        assert_eq!(git.head_commit(&repo.root).await.unwrap().unwrap(), base);
        git.abort_merge(&repo.root).await.unwrap();
        assert!(!git.merge_in_progress(&repo.root).await.unwrap());
        assert!(!repo.root.join("a.txt").exists());

        git.merge_staged(&repo.root, "task", false).await.unwrap();
        let merge = git.finish_merge(&repo.root, "Merge task").await.unwrap();
        assert_eq!(git.parents(&repo.root, &merge).await.unwrap(), [base, task]);
        assert_eq!(
            git.merge_staged(&repo.root, "task", false).await.unwrap(),
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
        git.add_worktree(&repo.root, &tree, "task", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("README.md"), "theirs\n").unwrap();
        git.commit_all(&tree, "theirs").await.unwrap();
        repo.write("README.md", "ours\n");
        repo.git(&["commit", "--quiet", "-am", "ours"]);

        let outcome = git.merge_staged(&repo.root, "task", false).await.unwrap();
        assert_eq!(outcome, StagedMerge::Conflict(vec!["README.md".into()]));
        assert!(!git.merge_in_progress(&repo.root).await.unwrap());

        let outcome = git.merge_staged(&repo.root, "task", true).await.unwrap();
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
