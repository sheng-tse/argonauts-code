//! Git, always run as an argument vector. These block, and git answers these in milliseconds.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context as _, bail};

fn run(repo: &Path, args: &[&str]) -> anyhow::Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
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

pub(crate) fn toplevel(dir: &Path) -> anyhow::Result<PathBuf> {
    let root = git(dir, &["rev-parse", "--show-toplevel"])
        .with_context(|| format!("{} isn't inside a git repository", dir.display()))?;
    Ok(PathBuf::from(root))
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

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_testkit::TempRepo;

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
