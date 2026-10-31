//! What the readers read beside your request: the notes and earlier decisions the crew kept
//! in the repo's state directory, and any your repository keeps in `.argonauts/`, the ones
//! sharing the most words with the request first.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use argonauts_core::base::layout::{project_files, repo_state};

use super::materiality::one_line;

/// At most this many notes and this many decisions go to each reader.
const MOST: usize = 8;

/// `(notes, decisions)`: one line each, such as `redis.md: Redis runs on 6380 in CI` and
/// `D-003: Use Redis for sessions`.
pub fn read(base_worktree: &Path, state_dir: &Path, request: &str) -> (Vec<String>, Vec<String>) {
    let project = base_worktree.join(".argonauts");
    let words = words(request);
    let notes = [
        project.join(project_files::NOTES),
        state_dir.join(repo_state::NOTES),
    ];
    let notes = relevant(&notes, &words, |name, line| format!("{name}: {line}"));
    let decisions = [
        project.join(project_files::DECISIONS),
        state_dir.join(repo_state::DECISIONS),
    ];
    let decisions = relevant(&decisions, &words, |name, line| {
        let number = name
            .strip_prefix("D-")
            .and_then(|rest| rest.split(['-', '.']).next())
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        match number {
            Some(number) => format!("D-{number}: {line}"),
            None => format!("{name}: {line}"),
        }
    });
    (notes, decisions)
}

fn relevant(
    dirs: &[PathBuf],
    request: &BTreeSet<String>,
    entry: impl Fn(&str, &str) -> String,
) -> Vec<String> {
    let mut found: Vec<(usize, String)> = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(Result::ok)
        .filter(|item| item.path().extension().is_some_and(|ext| ext == "md"))
        .filter_map(|item| {
            let name = item.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(item.path()).ok()?;
            let line = first_line(&text)?;
            let shared = words(&format!("{name} {text}"))
                .intersection(request)
                .count();
            Some((shared, entry(&name, &line)))
        })
        .collect();
    // Most shared words first, then by name, so the same folder always reads the same way.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().take(MOST).map(|(_, line)| line).collect()
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(|line| one_line(line.trim_start_matches('#')))
        .find(|line| !line.is_empty())
}

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() > 3)
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_decisions_come_back_most_relevant_first() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join(".argonauts");
        std::fs::create_dir_all(project.join("notes")).unwrap();
        std::fs::create_dir_all(project.join("decisions")).unwrap();
        std::fs::write(
            project.join("notes/redis.md"),
            "# Redis\nRedis runs on 6380.\n",
        )
        .unwrap();
        std::fs::write(project.join("notes/style.md"), "\n\nTabs, never spaces.\n").unwrap();
        std::fs::write(project.join("notes/skip.txt"), "not a note").unwrap();
        std::fs::write(
            project.join("decisions/D-003-sessions.md"),
            "# Use Redis for sessions\n",
        )
        .unwrap();

        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(state.path().join("notes")).unwrap();
        std::fs::write(
            state.path().join("notes/ports.md"),
            "# Ports
The API is on 8080.
",
        )
        .unwrap();

        let (notes, decisions) = read(
            dir.path(),
            state.path(),
            "Store the rate limit counters in Redis",
        );
        assert_eq!(
            notes,
            [
                "redis.md: Redis",
                "ports.md: Ports",
                "style.md: Tabs, never spaces."
            ]
        );
        assert_eq!(decisions, ["D-003: Use Redis for sessions"]);
    }

    #[test]
    fn a_missing_folder_reads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        assert_eq!(
            read(dir.path(), &state, "anything"),
            (Vec::new(), Vec::new())
        );
    }
}
