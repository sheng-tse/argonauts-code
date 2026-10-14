//! Landing over files of yours that already hold what it brings. Git's fast-forward stops at
//! an untracked file where the run adds one, whatever its bytes, so such a file waits in the
//! run's folder for the moment git writes the same bytes there, and comes back if git
//! doesn't. A landing cut short meanwhile puts it back the next time the run lands or ends.

use std::io;
use std::path::{Path, PathBuf};

use crate::engine::host::Host;
use crate::git::GitError;
use crate::merge::in_the_way::{Holding, is_inside, move_path};

/// Moves your checkout to `rev` by git's fast-forward, taking your files in `holding` as they
/// are: an edit is staged, and an untracked file is set aside while git writes it. True when
/// your branch moved.
pub async fn fast_forward(
    host: &Host,
    root: &Path,
    rev: &str,
    holding: &Holding,
) -> Result<bool, GitError> {
    let git = host.git();
    git.stage_as_they_are(root, &holding.edited).await?;
    let aside = folder(host);
    set_aside(root, &aside, &holding.untracked);
    let moved = git.fast_forward(root, rev).await;
    if matches!(moved, Ok(true)) {
        // What was set aside is what merged, byte for byte.
        if let Err(err) = std::fs::remove_dir_all(&aside)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!("removing {}: {err}", aside.display());
        }
    } else {
        put_back(root, &aside);
    }
    moved
}

/// Puts back what a landing cut short left set aside.
pub fn put_back_leftovers(host: &Host) {
    put_back(&host.repo().root, &folder(host));
}

fn folder(host: &Host) -> PathBuf {
    host.repo().run_dir.join("landing-aside")
}

/// Moves `files` from `root` into `aside`, keeping their paths. A file that can't move stays,
/// and git's refusal then says what's there.
fn set_aside(root: &Path, aside: &Path, files: &[String]) {
    for file in files.iter().filter(|file| is_inside(file)) {
        let to = aside.join(file);
        let moved = to
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| move_path(&root.join(file), &to));
        if let Err(err) = moved {
            tracing::warn!("setting {file} aside: {err}");
        }
    }
}

/// Moves every file in `aside` back to its path under `root`. Where something is there by
/// now, a copy with the same bytes is dropped, and one that is different stays in `aside`, so
/// nothing of yours is ever lost.
fn put_back(root: &Path, aside: &Path) {
    let mut files = Vec::new();
    collect_files(aside, Path::new(""), &mut files);
    for rel in files {
        let from = aside.join(&rel);
        let to = root.join(&rel);
        let result = match std::fs::symlink_metadata(&to) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => to
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| move_path(&from, &to)),
            Ok(_) if same_bytes(&from, &to) => std::fs::remove_file(&from),
            Ok(_) => Ok(()),
            Err(err) => Err(err),
        };
        if let Err(err) = result {
            tracing::warn!("putting {} back: {err}", rel.display());
        }
    }
    remove_empty_folders(aside);
}

fn collect_files(dir: &Path, rel: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let rel = rel.join(entry.file_name());
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => collect_files(&entry.path(), &rel, files),
            Ok(_) => files.push(rel),
            Err(_) => {}
        }
    }
}

/// Removes `dir` and the folders under it that are empty, deepest first.
fn remove_empty_folders(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                remove_empty_folders(&entry.path());
            }
        }
    }
    // Fails while something is left, which is what keeps it.
    let _ = std::fs::remove_dir(dir);
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(one), Ok(other)) if one.is_file() && other.is_file() && one.len() == other.len() => {
            std::fs::read(a).ok() == std::fs::read(b).ok()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_set_aside_goes_back_and_never_over_something_else() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("checkout");
        let aside = dir.path().join("aside");
        for file in ["a.txt", "deep/b.txt", "deep/c.txt"] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file).unwrap();
        }
        let files: Vec<String> = ["a.txt", "deep/b.txt", "deep/c.txt", "../outside"]
            .iter()
            .map(|file| (*file).to_owned())
            .collect();
        set_aside(&root, &aside, &files);
        assert!(!root.join("a.txt").exists() && aside.join("deep/b.txt").exists());

        // Meanwhile the same bytes merged at a.txt, and something else at deep/c.txt.
        std::fs::write(root.join("a.txt"), "a.txt").unwrap();
        std::fs::write(root.join("deep/c.txt"), "not yours").unwrap();
        put_back(&root, &aside);
        assert_eq!(
            std::fs::read_to_string(root.join("deep/b.txt")).unwrap(),
            "deep/b.txt"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("deep/c.txt")).unwrap(),
            "not yours"
        );
        assert_eq!(
            std::fs::read_to_string(aside.join("deep/c.txt")).unwrap(),
            "deep/c.txt",
            "yours, kept where it waited"
        );
        assert!(!aside.join("a.txt").exists() && !aside.join("deep/b.txt").exists());

        std::fs::remove_file(root.join("deep/c.txt")).unwrap();
        put_back(&root, &aside);
        assert!(!aside.exists(), "the folder goes once it's empty");
    }
}
