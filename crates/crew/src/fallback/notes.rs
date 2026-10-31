//! Who made each commit, in the run's log and in `refs/notes/argonauts`, so commit messages stay
//! clean and the diff view can say "3 commits by codex/<model> since fallback-1".

use std::path::Path;

use argonauts_core::TaskId;
use argonauts_core::base::names::ModelRef;

use crate::engine::error::EngineResult;
use crate::engine::host::Host;

const PREFIX: &str = "author: ";

/// The note's text for a commit by `author`.
pub fn note_text(author: &ModelRef) -> String {
    format!("{PREFIX}{author}")
}

/// The author a note names, if it's one of ours.
pub fn parse_note(text: &str) -> Option<ModelRef> {
    text.lines()
        .find_map(|line| line.strip_prefix(PREFIX))
        .and_then(|author| author.trim().parse().ok())
}

pub async fn record_author(
    host: &Host,
    dir: &Path,
    commit: &str,
    author: &ModelRef,
) -> EngineResult<()> {
    let text = note_text(author);
    let git = host.git();
    // An unchanged note isn't written again, so catching up costs nothing once done.
    if git.note(dir, commit).await?.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    git.add_note(dir, commit, &text).await?;
    Ok(())
}

pub async fn author_of(host: &Host, dir: &Path, commit: &str) -> EngineResult<Option<ModelRef>> {
    let note = host.git().note(dir, commit).await?;
    Ok(note.as_deref().and_then(parse_note))
}

/// Notes the author of every commit the log has for this task. Your own commits (`by` unset)
/// get none.
pub async fn catch_up(host: &Host, task: &TaskId) -> EngineResult<()> {
    let commits: Vec<(String, ModelRef)> = host.read(|state| {
        state
            .tasks
            .get(task)
            .map(|record| {
                record
                    .commits
                    .iter()
                    .filter_map(|commit| Some((commit.sha.clone()?, commit.by.clone()?)))
                    .collect()
            })
            .unwrap_or_default()
    });
    let root = host.repo().root.clone();
    for (sha, author) in commits {
        record_author(host, &root, &sha, &author).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_round_trip_the_author() {
        let author = ModelRef::new("codex", Some("gpt-5.6".into()));
        let text = note_text(&author);
        assert_eq!(text, "author: codex/gpt-5.6");
        assert_eq!(parse_note(&text), Some(author));
        assert_eq!(parse_note("something else"), None);
        assert_eq!(parse_note("author: agy"), Some(ModelRef::new("agy", None)));
    }
}
