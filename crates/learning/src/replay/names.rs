//! Whether a task's tests lean on names or files that only its solution gave.
//!
//! Tests written against one solution can reject a different correct one when they call a
//! function the solution named and the request never did. A name counts as taken from the
//! solution when the tests use it, the change's other lines add it, the base tree doesn't have
//! it, and the request doesn't say it. A file counts the same way when the change added it and
//! the tests name it.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::git;

/// Larger files are data, not code a test could take a name from.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// How far into a file to look for a zero byte before calling it binary.
const BINARY_SNIFF: usize = 8000;

/// Test files the change added or edited, by the usual naming: a `test`, `tests`, `__tests__`,
/// or `spec` folder, a `test_` prefix, or `_test.`, `.test.`, `.spec.`, or `_spec.` in the name.
/// Used when the run recorded no acceptance tests of its own, as a solo run doesn't.
pub(super) fn changed_test_files(repo: &Path, base: &str, merged: &str) -> Result<Vec<String>> {
    let listed = git::run(
        repo,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=AM",
            base,
            merged,
        ],
    )?;
    Ok(listed
        .lines()
        .filter(|path| looks_like_test(path))
        .map(str::to_owned)
        .collect())
}

fn looks_like_test(path: &str) -> bool {
    let path = Path::new(path);
    let in_test_folder = path.parent().is_some_and(|dir| {
        dir.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("test" | "tests" | "__tests__" | "spec")
            )
        })
    });
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    in_test_folder
        || name.starts_with("test_")
        || ["_test.", ".test.", ".spec.", "_spec."]
            .iter()
            .any(|marker| name.contains(marker))
}

/// The names and files the tests take from the solution, sorted. `repo` holds the merged
/// commit with its history, `base_dir` a checkout of the base commit without the tests, and
/// `hidden` the copies of `tests`.
pub(super) fn unseen(
    request: &str,
    base_dir: &Path,
    repo: &Path,
    base: &str,
    merged: &str,
    tests: &[String],
    hidden: &[PathBuf],
) -> Result<Vec<String>> {
    let given = Given::new(request);
    let mut test_text = String::new();
    for path in hidden {
        test_text.push_str(&String::from_utf8_lossy(&fs::read(path)?));
        test_text.push('\n');
    }
    let test_names = identifiers(&test_text);

    let mut diff_args: Vec<String> = [
        "diff",
        "--unified=0",
        "--no-color",
        "--no-ext-diff",
        "--no-renames",
        base,
        merged,
        "--",
        ".",
    ]
    .map(str::to_owned)
    .to_vec();
    diff_args.extend(tests.iter().map(|test| format!(":(exclude,literal){test}")));
    let diff_args: Vec<&str> = diff_args.iter().map(String::as_str).collect();
    let diff = git::run(repo, &diff_args)?;
    let mut added_names = BTreeSet::new();
    for line in diff.lines() {
        if let Some(added) = line.strip_prefix('+')
            && !line.starts_with("+++")
        {
            for_each_identifier(added, |name| {
                added_names.insert(name.to_owned());
            });
        }
    }

    let candidates: HashSet<String> = test_names
        .intersection(&added_names)
        .filter(|name| !given.covers(name))
        .cloned()
        .collect();
    let in_base = present_in_tree(base_dir, &candidates)?;
    let mut unseen: BTreeSet<String> = candidates.difference(&in_base).cloned().collect();

    let added_files = git::run(
        repo,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            base,
            merged,
        ],
    )?;
    for file in added_files
        .lines()
        .filter(|file| !tests.iter().any(|test| test == file))
    {
        let path = Path::new(file);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(file);
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(name);
        let named = test_text.contains(file) || test_text.contains(name);
        if named && !given.covers(stem) && !given.covers(name) {
            unseen.insert(file.to_owned());
        }
    }
    Ok(unseen.into_iter().collect())
}

/// The words of the request, so a name counts as given when the request spells it or says
/// each of its words, as "retry after" gives `retryAfter` and `retry_after`.
struct Given {
    text: String,
    words: HashSet<String>,
}

impl Given {
    fn new(request: &str) -> Self {
        let words = request
            .split(|c: char| !c.is_alphanumeric())
            .flat_map(split_words)
            .collect();
        Self {
            text: request.to_lowercase(),
            words,
        }
    }

    fn covers(&self, name: &str) -> bool {
        if self.text.contains(&name.to_lowercase()) {
            return true;
        }
        let parts = split_words(name);
        !parts.is_empty() && parts.iter().all(|part| self.words.contains(part))
    }
}

/// `retryAfter`, `RetryAfter`, `retry_after`, and `retry-after` all give `retry` and `after`;
/// `HTTPServer` gives `http` and `server`.
fn split_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let previous = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1);
        let boundary = c.is_uppercase()
            && previous.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(|n| n.is_lowercase()))
            });
        if boundary && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn identifiers(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for_each_identifier(text, |name| {
        names.insert(name.to_owned());
    });
    names
}

/// Calls `f` with each identifier of two or more characters: a letter or `_`, then letters,
/// digits, or `_`. One-letter names say too little to count.
fn for_each_identifier(text: &str, mut f: impl FnMut(&str)) {
    let mut start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        let word = c.is_alphanumeric() || c == '_';
        match start {
            None if word && !c.is_numeric() => start = Some(i),
            Some(from) if !word => {
                if text[from..i].chars().nth(1).is_some() {
                    f(&text[from..i]);
                }
                start = None;
            }
            _ => {}
        }
    }
}

/// Which of `names` appear in the checkout at `root`, in a file's text or its path.
fn present_in_tree(root: &Path, names: &HashSet<String>) -> Result<HashSet<String>> {
    let mut found = HashSet::new();
    if names.is_empty() {
        return Ok(found);
    }
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for item in fs::read_dir(&folder)? {
            let path = item?.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            let meta = fs::symlink_metadata(&path)?;
            if let Ok(relative) = path.strip_prefix(root) {
                for_each_identifier(&relative.to_string_lossy(), |name| {
                    if names.contains(name) {
                        found.insert(name.to_owned());
                    }
                });
            }
            if meta.is_dir() {
                folders.push(path);
            } else if meta.is_file() && meta.len() <= MAX_FILE_BYTES {
                let bytes = fs::read(&path)?;
                if bytes.iter().take(BINARY_SNIFF).any(|&byte| byte == 0) {
                    continue;
                }
                for_each_identifier(&String::from_utf8_lossy(&bytes), |name| {
                    if names.contains(name) {
                        found.insert(name.to_owned());
                    }
                });
            }
            if found.len() == names.len() {
                return Ok(found);
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_into_words() {
        assert_eq!(split_words("retryAfter"), ["retry", "after"]);
        assert_eq!(split_words("Retry-After"), ["retry", "after"]);
        assert_eq!(split_words("retry_after"), ["retry", "after"]);
        assert_eq!(split_words("HTTPServer"), ["http", "server"]);
        assert_eq!(split_words("v2Parser"), ["v2", "parser"]);
    }

    #[test]
    fn the_request_gives_names_it_spells_or_says() {
        let given = Given::new("Send a Retry-After header on 429");
        assert!(given.covers("retry_after"));
        assert!(given.covers("RetryAfter"));
        assert!(!given.covers("backoff_seconds"));
    }

    #[test]
    fn identifiers_skip_numbers_and_single_letters() {
        let names = identifiers("let x = twice(21) + 2a; _ok");
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["_ok", "let", "twice"]
        );
    }

    #[test]
    fn test_files_go_by_their_usual_names() {
        assert!(looks_like_test("tests/limits.rs"));
        assert!(looks_like_test("src/__tests__/a.js"));
        assert!(looks_like_test("pkg/limit_test.go"));
        assert!(looks_like_test("web/limit.spec.ts"));
        assert!(looks_like_test("test_limits.py"));
        assert!(!looks_like_test("src/testing.rs"));
    }
}
