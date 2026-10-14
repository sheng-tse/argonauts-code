//! What a message cites: paths, tasks, tests, and earlier records, read from its text by code,
//! and whether a path falls under a task's glob.

use std::sync::LazyLock;

use globset::{Glob, GlobMatcher};
use regex::Regex;

use argonauts_core::TaskId;

static TASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(task|test|experiment)-\d{3,}$").expect("a valid pattern"));
static RECORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(m|D)-\d{3,}$").expect("a valid pattern"));
static TEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)+$").expect("a valid pattern")
});

/// Paths, task ids, test names, and message or decision ids cited in `text`, in order, once
/// each.
pub fn refs_in(text: &str) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| {
            matches!(
                c,
                '`' | '"' | '\'' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '?' | '!'
            )
        });
        let word = word.strip_suffix('.').unwrap_or(word);
        if cites(word) && !refs.iter().any(|known| known == word) {
            refs.push(word.to_owned());
        }
    }
    refs
}

fn cites(word: &str) -> bool {
    if word.is_empty() || word.contains("://") {
        return false;
    }
    TASK.is_match(word) || RECORD.is_match(word) || TEST.is_match(word) || is_path(word)
}

fn is_path(word: &str) -> bool {
    let plausible = word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '*'));
    if !plausible || word.starts_with('-') {
        return false;
    }
    if word.contains('/') {
        return word.chars().any(|c| c.is_ascii_alphanumeric());
    }
    // A bare file name needs an extension: `Cargo.toml`, `limiter.rs`.
    match word.rsplit_once('.') {
        Some((stem, extension)) => {
            stem.len() >= 2
                && (1..=5).contains(&extension.len())
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
                && extension.chars().any(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

pub fn task_in(reference: &str) -> Option<TaskId> {
    TASK.is_match(reference)
        .then(|| TaskId::new(reference).ok())
        .flatten()
}

/// Whether `path` (a path, or a glob itself) falls under `pattern`, a task's owned or read-only
/// glob.
pub fn under(pattern: &str, path: &str) -> bool {
    let path = path.trim_start_matches("./");
    let pattern = pattern.trim_start_matches("./");
    if pattern == path {
        return true;
    }
    if let Some(matcher) = matcher(pattern)
        && matcher.is_match(path)
    {
        return true;
    }
    // A directory named without a glob covers what's inside it.
    let fixed = literal(pattern);
    if !fixed.is_empty() && fixed == pattern && path.starts_with(&format!("{fixed}/")) {
        return true;
    }
    // A glob in the request overlaps the pattern when one's fixed part contains the other's.
    let asked = literal(path);
    asked != path
        && !asked.is_empty()
        && ((!fixed.is_empty() && asked.starts_with(fixed)) || fixed.starts_with(asked))
}

fn matcher(pattern: &str) -> Option<GlobMatcher> {
    Glob::new(pattern).ok().map(|glob| glob.compile_matcher())
}

/// A glob's fixed part before its first wildcard, without a trailing `/`.
fn literal(pattern: &str) -> &str {
    let end = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    pattern[..end].trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_are_paths_tasks_tests_and_records() {
        let refs = refs_in(
            "Should `src/limiter/mod.rs` use Redis? task-003 relies on it, and \
             limiter::tests::burst fails since m-004. See Cargo.toml, or https://example.com.",
        );
        assert_eq!(
            refs,
            [
                "src/limiter/mod.rs",
                "task-003",
                "limiter::tests::burst",
                "m-004",
                "Cargo.toml"
            ]
        );
        assert!(refs_in("Is it ok? e.g. yes, 2.5 times.").is_empty());
    }

    #[test]
    fn paths_fall_under_globs() {
        assert!(under("src/limiter/**", "src/limiter/redis.rs"));
        assert!(under("src/limiter", "src/limiter/redis.rs"));
        assert!(under("src/api/*.rs", "src/api/mod.rs"));
        assert!(!under("src/api/*.rs", "src/limiter/mod.rs"));
        assert!(under("src/limiter/**", "src/limiter/**"));
        assert!(under("src/**", "src/limiter/*.rs"));
        assert!(!under("src/limiter/**", "src/api/*.rs"));
        assert_eq!(task_in("task-004"), Some(TaskId::new("task-004").unwrap()));
        assert_eq!(task_in("src/task-004"), None);
    }
}
