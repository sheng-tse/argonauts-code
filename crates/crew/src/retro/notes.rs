//! Accepted notes, one file per topic in the repo's state directory under `~/.argonauts`
//! (`notes/`), one bullet per note with its source: the card, the worker, the run, and when.
//! They're Argonauts' own, so they never go into your repository. Notes your repository keeps
//! itself in `.argonauts/notes/` are read beside them. Writing is idempotent, so a re-entered
//! retro never repeats a note. Sessions get notes back as quoted reference data, never as
//! instructions, and a flagged note waits in the state folder for you.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use argonauts_core::layout::repo_state;
use argonauts_core::names::WorkerRef;
use argonauts_core::steps::card::MemoryProposal;
use argonauts_core::{CardId, RunId};
use argonauts_learning::memory::{self, Source};
use argonauts_learning::scan::Finding;

/// Notes a repository keeps itself, from its root.
pub const NOTES_DIR: &str = ".argonauts/notes";
/// Notes the scan flagged, waiting for you under `state/<repo>/`. Never applied on their own.
pub const FLAGGED_DIR: &str = "flagged-notes";

/// A note the scan flagged, kept for you to accept or drop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flagged {
    pub note: MemoryProposal,
    pub source: Source,
    pub findings: Vec<Finding>,
}

/// Keeps one sourced note in its topic's file in the repo's state directory `state_dir`.
/// Returns the file, relative to `state_dir`, when it changed; a note already there from the
/// same run is left alone.
pub fn keep_note(
    state_dir: &Path,
    note: &MemoryProposal,
    source: &Source,
) -> std::io::Result<Option<String>> {
    let relative = format!("{}/{}.md", repo_state::NOTES, slug(&note.topic));
    let changed = append_note(&state_dir.join(&relative), note, source)?;
    Ok(changed.then_some(relative))
}

/// Appends one sourced note to its topic's file in `worktree`'s `.argonauts/notes/`, for a
/// note you chose to keep in a repository. Returns the file, relative to `worktree`, when it
/// changed; a note already there from the same run is left alone.
pub fn write_note(
    worktree: &Path,
    note: &MemoryProposal,
    source: &Source,
) -> std::io::Result<Option<String>> {
    let relative = format!("{NOTES_DIR}/{}.md", slug(&note.topic));
    let changed = append_note(&worktree.join(&relative), note, source)?;
    Ok(changed.then_some(relative))
}

/// Whether the note went into the topic file at `path`.
fn append_note(path: &Path, note: &MemoryProposal, source: &Source) -> std::io::Result<bool> {
    let mut text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            format!("# {}\n\n", note.topic.trim())
        }
        Err(error) => return Err(error),
    };
    let start = format!("- {} (", one_line(&note.text));
    let run = source.run.as_str();
    if text
        .lines()
        .any(|line| line.starts_with(&start) && line.contains(run))
    {
        return Ok(false);
    }
    let date = source
        .date
        .format(&Rfc3339)
        .map_err(std::io::Error::other)?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "{start}{}, {}, {run}, {date})\n",
        source.card, source.worker
    ));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(true)
}

/// The sourced notes in one topic file. Bullets without a full source, such as ones written by
/// hand, aren't attributable, so they never count as anyone's note.
pub fn parse(text: &str) -> Vec<(MemoryProposal, Source)> {
    let mut topic = "general".to_owned();
    let mut notes = Vec::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("# ") {
            heading.trim().clone_into(&mut topic);
            continue;
        }
        let Some(bullet) = line.strip_prefix("- ") else {
            continue;
        };
        let Some((body, source)) = bullet
            .strip_suffix(')')
            .and_then(|bullet| bullet.rsplit_once(" ("))
        else {
            continue;
        };
        let fields: Vec<&str> = source.split(", ").collect();
        let [card, worker, run, date] = fields[..] else {
            continue;
        };
        let (Ok(card), Ok(worker), Ok(run), Ok(date)) = (
            CardId::new(card),
            worker.parse::<WorkerRef>(),
            RunId::new(run),
            OffsetDateTime::parse(date, &Rfc3339),
        ) else {
            continue;
        };
        notes.push((
            MemoryProposal {
                topic: topic.clone(),
                text: body.to_owned(),
            },
            Source {
                run,
                card,
                worker,
                date,
                evidence: Vec::new(),
            },
        ));
    }
    notes
}

/// One topic file under `tree`: where it is, its text, and its sourced notes.
pub struct NoteFile {
    pub relative: String,
    pub text: String,
    pub notes: Vec<(MemoryProposal, Source)>,
}

/// Every topic file the crew kept in `state_dir`, then every one in `tree`'s
/// `.argonauts/notes/`, each in name order.
pub fn read_all(tree: &Path, state_dir: &Path) -> Vec<NoteFile> {
    let mut files = read_in(&state_dir.join(repo_state::NOTES), repo_state::NOTES);
    files.extend(read_files(tree));
    files
}

/// Every topic file in `tree`'s `.argonauts/notes/`, in name order.
pub fn read_files(tree: &Path) -> Vec<NoteFile> {
    read_in(&tree.join(NOTES_DIR), NOTES_DIR)
}

/// Every topic file in `dir`, named from `shown`.
fn read_in(dir: &Path, shown: &str) -> Vec<NoteFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md") && path.is_file())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some(NoteFile {
                relative: format!("{shown}/{name}"),
                notes: parse(&text),
                text,
            })
        })
        .collect()
}

/// Every sourced note for `tree`, as the quoted block sessions get, leaving out notes written
/// by `judged`: a reviewer or judge never reads what the agent it judges wrote down.
pub fn quoted(tree: &Path, state_dir: &Path, judged: Option<&WorkerRef>) -> String {
    let notes: Vec<(MemoryProposal, Source)> = read_all(tree, state_dir)
        .into_iter()
        .flat_map(|file| file.notes)
        .filter(|(_, source)| judged.is_none_or(|judged| source.worker != *judged))
        .collect();
    memory::quoted(&notes)
}

/// Keeps a flagged note for you. One file per note, so taking one never touches another.
pub fn flag(state_dir: &Path, flagged: &Flagged) -> std::io::Result<PathBuf> {
    let dir = state_dir.join(FLAGGED_DIR);
    std::fs::create_dir_all(&dir)?;
    let digest = argonauts_core::sha256_hex(
        format!("{}\n{}", flagged.note.topic, flagged.note.text).as_bytes(),
    );
    let path = dir.join(format!(
        "{}-{}-{}.json",
        flagged.source.run,
        flagged.source.card,
        &digest[..12]
    ));
    let text = serde_json::to_string_pretty(flagged).map_err(std::io::Error::other)?;
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Flagged notes waiting for you, oldest file name first.
pub fn flagged(state_dir: &Path) -> Vec<(PathBuf, Flagged)> {
    let Ok(entries) = std::fs::read_dir(state_dir.join(FLAGGED_DIR)) else {
        return Vec::new();
    };
    let mut found: Vec<(PathBuf, Flagged)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let flagged = serde_json::from_str(&text).ok()?;
            Some((path, flagged))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Returns the files it changed, relative to `worktree`.
pub fn write(
    worktree: &Path,
    run: &RunId,
    notes: &[MemoryProposal],
) -> std::io::Result<Vec<String>> {
    let mut changed: Vec<String> = Vec::new();
    for note in notes {
        let relative = format!("{NOTES_DIR}/{}.md", slug(&note.topic));
        let path = worktree.join(&relative);
        let mut text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                format!("# {}\n\n", note.topic.trim())
            }
            Err(error) => return Err(error),
        };
        let bullet = format!("- {} ({run})", one_line(&note.text));
        if text.lines().any(|line| line == bullet) {
            continue;
        }
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&bullet);
        text.push('\n');
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, text)?;
        if !changed.contains(&relative) {
            changed.push(relative);
        }
    }
    Ok(changed)
}

/// Lowercase words joined by dashes, at most 40 characters; `general` when nothing is left.
pub fn slug(topic: &str) -> String {
    let mut slug = String::new();
    for c in topic.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(40).collect();
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        "general".to_owned()
    } else {
        slug
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_go_one_file_per_topic_once() {
        let dir = tempfile::tempdir().unwrap();
        let run = RunId::new("run-01").unwrap();
        let note = |topic: &str, text: &str| MemoryProposal {
            topic: topic.to_owned(),
            text: text.to_owned(),
        };
        let notes = [
            note("Redis", "Counters expire\nwith the window."),
            note("redis", "Use one pool per process."),
            note("Rate limits", "Keys are per API token."),
        ];
        let changed = write(dir.path(), &run, &notes).unwrap();
        assert_eq!(
            changed,
            [
                ".argonauts/notes/redis.md",
                ".argonauts/notes/rate-limits.md"
            ]
        );
        assert!(write(dir.path(), &run, &notes).unwrap().is_empty());
        let redis = std::fs::read_to_string(dir.path().join(".argonauts/notes/redis.md")).unwrap();
        assert_eq!(
            redis,
            "# Redis\n\n- Counters expire with the window. (run-01)\n- Use one pool per process. (run-01)\n"
        );
        assert_eq!(slug("  Ünïcode / ***  "), "n-code");
        assert_eq!(slug("***"), "general");
    }
}
