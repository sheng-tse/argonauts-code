//! The git the learning crate runs: through the git CLI as argument vectors, with hooks,
//! fsmonitor, and signing off so nothing in a repository runs code as a side effect.
//!
//! Proposals (a promoted learned workflow, an exported or imported skill, a conventions change) are
//! commits on their own branch, built with plumbing and a private index. Your checkout, its
//! index, and your current branch are never touched.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use argonauts_core::base::clock;

use crate::store::error::{LearningError, Result};

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

/// Identity for Argonauts Code's commits when you have none configured.
const FALLBACK_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Argonauts Code"),
    ("GIT_AUTHOR_EMAIL", "argonauts@localhost"),
    ("GIT_COMMITTER_NAME", "Argonauts Code"),
    ("GIT_COMMITTER_EMAIL", "argonauts@localhost"),
];

/// What a proposal commit puts at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    File(String),
    /// A symbolic link to this target, such as `../../.agents/skills/api-feature`.
    Link(String),
    Remove,
}

/// Runs `git args` in `dir` and returns its trimmed stdout.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    run_with(dir, args, &[], None)
}

/// `git` with extra environment and optional stdin.
pub fn run_with(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: Option<&[u8]>,
) -> Result<String> {
    let mut command = Command::new("git");
    command.args(SAFE_CONFIG).args(args).current_dir(dir);
    for name in REPOSITORY_ENV {
        command.env_remove(name);
    }
    for (name, value) in env {
        command.env(name, value);
    }
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let failed = |message: String| LearningError::Git {
        args: args.join(" "),
        message,
    };
    let mut child = command.spawn().map_err(|err| failed(err.to_string()))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(bytes)
            .map_err(|err| failed(err.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| failed(err.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(failed(
            stderr
                .trim()
                .lines()
                .last()
                .unwrap_or("no output")
                .to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Commits `entries` on top of `base` as `refs/heads/<branch>`, creating or moving that branch,
/// and returns the new commit. Works from the object store alone, so it never changes a
/// checkout or its index.
pub fn commit_on_branch(
    repo: &Path,
    base: &str,
    branch: &str,
    entries: &[(String, Entry)],
    message: &str,
) -> Result<String> {
    let base = run(
        repo,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )?;
    let index = private_index(repo)?;
    let index_text = index.to_string_lossy().into_owned();
    let mut env: Vec<(&str, &str)> = vec![("GIT_INDEX_FILE", index_text.as_str())];
    if run(repo, &["var", "GIT_COMMITTER_IDENT"]).is_err() {
        env.extend(FALLBACK_IDENTITY);
    }
    let result = (|| {
        run_with(repo, &["read-tree", &base], &env, None)?;
        for (path, entry) in entries {
            match entry {
                Entry::Remove => {
                    run_with(
                        repo,
                        &["update-index", "--force-remove", "--", path],
                        &env,
                        None,
                    )?;
                }
                Entry::File(text) | Entry::Link(text) => {
                    let blob = run_with(
                        repo,
                        &["hash-object", "-w", "--stdin"],
                        &env,
                        Some(text.as_bytes()),
                    )?;
                    let mode = if matches!(entry, Entry::Link(_)) {
                        "120000"
                    } else {
                        "100644"
                    };
                    let info = format!("{mode},{blob},{path}");
                    run_with(
                        repo,
                        &["update-index", "--add", "--cacheinfo", &info],
                        &env,
                        None,
                    )?;
                }
            }
        }
        let tree = run_with(repo, &["write-tree"], &env, None)?;
        let commit = run_with(
            repo,
            &["commit-tree", &tree, "-p", &base, "-m", message],
            &env,
            None,
        )?;
        run(
            repo,
            &["update-ref", &format!("refs/heads/{branch}"), &commit],
        )?;
        Ok(commit)
    })();
    let _ = std::fs::remove_file(&index);
    result
}

/// A fresh index file inside the repository's git directory, so it lives on the same disk and
/// nobody else's index is touched.
fn private_index(repo: &Path) -> Result<PathBuf> {
    let git_dir = run(repo, &["rev-parse", "--absolute-git-dir"])?;
    let nanos = clock::now_utc().unix_timestamp_nanos();
    Ok(PathBuf::from(git_dir).join(format!("argonauts-index-{}-{nanos}", std::process::id())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        run(path, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(path.join("README"), "hello\n").unwrap();
        run(path, &["add", "README"]).unwrap();
        let env = FALLBACK_IDENTITY;
        run_with(path, &["commit", "-q", "-m", "first"], &env, None).unwrap();
        dir
    }

    #[test]
    fn proposal_commits_leave_the_checkout_alone() {
        let dir = repo();
        let path = dir.path();
        let head = run(path, &["rev-parse", "HEAD"]).unwrap();
        let entries = vec![
            (
                ".agents/skills/api-feature/SKILL.md".to_owned(),
                Entry::File("---\nname: api-feature\n---\n".to_owned()),
            ),
            (
                ".claude/skills/api-feature".to_owned(),
                Entry::Link("../../.agents/skills/api-feature".to_owned()),
            ),
            ("README".to_owned(), Entry::Remove),
        ];
        let commit = commit_on_branch(
            path,
            "main",
            "argonauts/skills/api-feature",
            &entries,
            "Export api-feature",
        )
        .unwrap();

        assert_eq!(run(path, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(run(path, &["status", "--porcelain"]).unwrap(), "");
        assert!(path.join("README").exists());
        let listed = run(path, &["ls-tree", "-r", "--full-tree", &commit]).unwrap();
        assert!(listed.contains("100644 blob"), "{listed}");
        assert!(listed.contains("120000 blob"), "{listed}");
        assert!(!listed.contains("README"), "{listed}");
        assert_eq!(
            run(path, &["rev-parse", "argonauts/skills/api-feature"]).unwrap(),
            commit
        );
        assert_eq!(
            run(path, &["rev-parse", &format!("{commit}^")]).unwrap(),
            head
        );
    }
}
