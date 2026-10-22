//! Git, always run as an argument vector. These block, and git answers these in milliseconds.
//!
//! A folder with no git of its own is told apart here: its history, when it keeps one, is a
//! git folder under Argonauts Code's home, named on every call, so nothing in your folder ever
//! points at it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{LazyLock, Mutex, PoisonError};

use anyhow::{Context as _, bail};

use crate::hidden_git;

/// The folders this process opened, each with where its hidden history is kept when it has
/// one. What a folder has is read on every call, since `/git`, a run in another process, or
/// your own `git init` can change it at any time.
static FOLDERS: LazyLock<Mutex<HashMap<PathBuf, PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// From now on git calls for `folder` look at what it has: its own `.git`, else the hidden
/// history at `hidden` when that's on, else nothing.
pub(crate) fn know_folder(folder: &Path, hidden: &Path) {
    FOLDERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(folder.to_owned(), hidden.to_owned());
}

fn hidden_dir_of(folder: &Path) -> Option<PathBuf> {
    FOLDERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(folder)
        .cloned()
}

/// `git`, set to work in `dir`.
pub(crate) fn command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir);
    let Some(hidden) = hidden_dir_of(dir).filter(|_| !dir.join(".git").exists()) else {
        return command;
    };
    if hidden_git::is_on(&hidden) {
        command
            .arg(flag_with_path("--git-dir=", &hidden))
            .arg(flag_with_path("--work-tree=", dir));
    } else if let Some(parent) = dir.parent() {
        // No git here at all, so it must not walk up into a repository that only happens to
        // hold the folder.
        command.env("GIT_CEILING_DIRECTORIES", parent);
    }
    command
}

fn flag_with_path(flag: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(flag);
    arg.push(path);
    arg
}

fn run(repo: &Path, args: &[&str]) -> anyhow::Result<Output> {
    command(repo)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))
}

/// Stdout without its final newline. Fails with git's own message.
pub(crate) fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = run(repo, args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}

/// Whether git exited 0, for commands that answer with their status.
pub(crate) fn git_ok(repo: &Path, args: &[&str]) -> anyhow::Result<bool> {
    Ok(run(repo, args)?.status.success())
}

/// Whether git has anything for `dir`: a repository of its own or above it, or a hidden
/// history that's on.
pub(crate) fn has_git(dir: &Path) -> bool {
    git_ok(dir, &["rev-parse", "--git-dir"]).unwrap_or(false)
}

pub(crate) fn toplevel(dir: &Path) -> anyhow::Result<PathBuf> {
    let root = git(dir, &["rev-parse", "--show-toplevel"])
        .with_context(|| format!("{} isn't inside a git repository", dir.display()))?;
    Ok(PathBuf::from(root))
}

/// The project `dir` belongs to: the top of the git repository holding it, or `dir` itself
/// when no repository holds it, or when the one that does ignores it and so would show a run
/// none of its files.
pub(crate) fn project_root(dir: &Path) -> PathBuf {
    match toplevel(dir) {
        Ok(top) if !git_ok(dir, &["check-ignore", "--quiet", "."]).unwrap_or(false) => top,
        _ => dir.to_owned(),
    }
}

/// Whether the checked-out branch has a commit. A repository where `git init` just ran has
/// none.
pub(crate) fn has_commits(repo: &Path) -> anyhow::Result<bool> {
    git_ok(repo, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
}

pub(crate) fn head(repo: &Path) -> anyhow::Result<String> {
    git(repo, &["rev-parse", "HEAD"])
}

/// Paths with uncommitted changes, untracked files included, as `git status` lists them.
pub(crate) fn uncommitted_paths(repo: &Path) -> anyhow::Result<Vec<String>> {
    let output = git(repo, &["status", "--porcelain=v1", "-z"])?;
    let mut paths = Vec::new();
    let mut fields = output.split('\0').filter(|field| !field.is_empty());
    while let Some(entry) = fields.next() {
        let Some(path) = entry.get(3..) else {
            continue;
        };
        paths.push(path.to_owned());
        // A rename or copy is followed by its old path, which isn't a change of its own.
        if matches!(entry.as_bytes().first(), Some(b'R' | b'C')) {
            fields.next();
        }
    }
    Ok(paths)
}

/// Whether the repository tracks `path`, which `.git/info/exclude` can't hide.
pub(crate) fn tracks(repo: &Path, path: &Path) -> anyhow::Result<bool> {
    let path = path.to_string_lossy();
    git_ok(repo, &["ls-files", "--error-unmatch", "--", &path])
}

/// Copies the index at `from` to `to`, dated as `from` is. git reads a file again where its
/// entry is as new as the index, since it may have changed within the same tick at the same
/// size. A copy dated now would have git trust those stats and miss the change.
pub(crate) fn copy_index(from: &Path, to: &Path) -> std::io::Result<()> {
    // Read first: should yours be rewritten meanwhile, the copy only reads more files again.
    let modified = std::fs::metadata(from)?.modified()?;
    std::fs::copy(from, to)?;
    std::fs::File::options()
        .write(true)
        .open(to)?
        .set_modified(modified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_testkit::TempRepo;

    /// A file rewritten at its size in the tick its entry and the index were written, as an
    /// agent's edit right after a commit can be, still shows as changed in a copy made seconds
    /// later.
    #[test]
    fn a_copied_index_still_sees_a_change_made_in_its_own_tick() {
        let repo = TempRepo::new().unwrap();
        let file = repo.root().join("app.py");
        let yours = repo.root().join(".git/index");
        let tick = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let date = |path: &Path| {
            let opened = std::fs::File::options().write(true).open(path).unwrap();
            opened.set_modified(tick).unwrap();
        };
        repo.write("app.py", "print(1)\n").unwrap();
        date(&file);
        repo.commit_all("app").unwrap();
        date(&yours);
        repo.write("app.py", "print(2)\n").unwrap();
        date(&file);

        let copy = repo.scratch().join("index-copy");
        copy_index(&yours, &copy).unwrap();
        let changed = command(repo.root())
            .args(["-c", "core.trustctime=false", "diff-files", "--name-only"])
            .env("GIT_INDEX_FILE", &copy)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&changed.stdout), "app.py\n");
    }

    #[test]
    fn lists_uncommitted_paths_and_tracked_files() {
        let repo = TempRepo::new().unwrap();
        repo.write("notes.txt", "draft\n").unwrap();
        let paths = uncommitted_paths(repo.root()).unwrap();
        assert_eq!(paths, vec!["notes.txt".to_owned()]);
        assert!(!tracks(repo.root(), Path::new("notes.txt")).unwrap());
        repo.commit_all("notes").unwrap();
        assert!(tracks(repo.root(), Path::new("notes.txt")).unwrap());
        assert_eq!(head(repo.root()).unwrap(), repo.head().unwrap());
    }
}
