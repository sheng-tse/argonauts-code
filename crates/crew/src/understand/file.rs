//! The goal file (`layout::goal_file`), kept in the repo's state directory under `~/.argonauts`. It
//! is Argonauts Code's own, so it never goes into your repository: nothing commits it, no check or
//! reviewer sees it as part of a change, and a merge never carries it.

use std::path::PathBuf;

use argonauts_core::RunId;
use argonauts_core::events::goal::{Criterion, GoalFile, GoalSource};
use argonauts_core::hash::sha256_hex;
use argonauts_core::layout;
use argonauts_core::paths::{create_private_dir_all, write_private_file};

use crate::divide::repo_state_dir;
use crate::engine::host::Host;
use crate::error::EngineResult;

use super::materiality::one_line;

/// The Markdown the goal file holds. Its SHA-256 is the goal's hash, so it never includes the
/// hash itself and says the same thing for the same goal every time.
pub fn render(goal: &GoalFile) -> String {
    let source = match goal.source {
        GoalSource::Prompt => "your prompt, as you wrote it",
        GoalSource::Readings => "the crew's readings",
    };
    let mut out = format!(
        "# Goal for {}\n\nVersion {}, from {source}.\n\n## Summary\n\n{}\n",
        goal.run,
        goal.version,
        block(&goal.summary)
    );
    let criteria: Vec<String> = goal
        .criteria
        .iter()
        .map(|criterion| format!("{}: {}", one_line(&criterion.id), one_line(&criterion.text)))
        .collect();
    section(&mut out, "Success criteria", &criteria);
    section(&mut out, "In scope", &lines(&goal.in_scope));
    section(&mut out, "Out of scope", &lines(&goal.out_of_scope));
    let assumptions: Vec<String> = goal
        .assumptions
        .iter()
        .map(|assumption| {
            let basis = one_line(&assumption.basis);
            if basis.is_empty() {
                one_line(&assumption.text)
            } else {
                format!("{} ({basis})", one_line(&assumption.text))
            }
        })
        .collect();
    section(&mut out, "Assumptions", &assumptions);
    section(&mut out, "Answers", &lines(&goal.answers));
    out
}

/// The goal file's SHA-256, as `render` writes it.
pub fn sha256(goal: &GoalFile) -> String {
    sha256_hex(render(goal).as_bytes())
}

/// `goals/<run>.md`, from the repo's state directory.
pub fn relative_path(run: &RunId) -> PathBuf {
    layout::goal_file(run)
}

/// Writes the goal file; returns the goal with its path and SHA-256 filled in. Writing the
/// same goal again changes nothing, so a re-entered step is safe.
pub async fn write(host: &Host, goal: GoalFile) -> EngineResult<GoalFile> {
    let goal = hashed(goal);
    let text = render(&goal);
    let absolute = repo_state_dir(host).join(&goal.path);
    if tokio::fs::read(&absolute).await.ok().as_deref() == Some(text.as_bytes()) {
        return Ok(goal);
    }
    if let Some(parent) = absolute.parent() {
        create_private_dir_all(parent)?;
    }
    write_private_file(&absolute, text.as_bytes())?;
    Ok(goal)
}

/// The goal with its relative path and hash filled in, as `write` leaves it.
pub fn hashed(mut goal: GoalFile) -> GoalFile {
    goal.path = relative_path(&goal.run);
    goal.sha256 = sha256(&goal);
    goal
}

/// Approve with edits: your note becomes an answer. It may add or narrow criteria, never drop
/// one silently. A line `+ text` or `add: text` adds a criterion; `G2: text` rewrites G2; every
/// other line is kept in the answer as written. Applying the same note twice changes nothing.
pub fn with_edits(goal: &GoalFile, note: &str) -> GoalFile {
    let note_lines: Vec<String> = note
        .lines()
        .map(one_line)
        .filter(|line| !line.is_empty())
        .collect();
    if note_lines.is_empty() {
        return goal.clone();
    }
    let answer = format!("Your edits: {}", note_lines.join("; "));
    if goal.answers.contains(&answer) {
        return goal.clone();
    }

    let mut edited = goal.clone();
    for line in &note_lines {
        if let Some(added) = addition(line) {
            let id = next_criterion_id(&edited.criteria);
            edited.criteria.push(Criterion::new(id, added));
        } else if let Some((id, text)) = rewrite(line)
            && let Some(criterion) = edited
                .criteria
                .iter_mut()
                .find(|criterion| criterion.id.eq_ignore_ascii_case(id))
        {
            criterion.text = text.to_owned();
        }
    }
    edited.answers.push(answer);
    hashed(edited)
}

fn addition(line: &str) -> Option<&str> {
    let text = line
        .strip_prefix("+ ")
        .or_else(|| {
            line.get(..4)
                .filter(|head| head.eq_ignore_ascii_case("add:"))
                .map(|_| &line[4..])
        })?
        .trim();
    (!text.is_empty()).then_some(text)
}

/// `G2: text`. An empty text never drops the criterion.
fn rewrite(line: &str) -> Option<(&str, &str)> {
    let (id, text) = line.split_once(':')?;
    let id = id.trim();
    let text = text.trim();
    let numbered =
        id.len() > 1 && id.starts_with(['G', 'g']) && id[1..].chars().all(|c| c.is_ascii_digit());
    (numbered && !text.is_empty()).then_some((id, text))
}

pub(crate) fn next_criterion_id(criteria: &[Criterion]) -> String {
    let highest = criteria
        .iter()
        .filter_map(|criterion| criterion.id.get(1..)?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("G{}", highest + 1)
}

fn section(out: &mut String, title: &str, items: &[String]) {
    out.push_str(&format!("\n## {title}\n\n"));
    if items.is_empty() {
        out.push_str("None.\n");
    }
    for item in items {
        out.push_str(&format!("- {item}\n"));
    }
}

fn lines(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|item| one_line(item))
        .filter(|item| !item.is_empty())
        .collect()
}

/// Multi-line text such as your prompt, with control characters removed and line endings
/// made plain.
fn block(text: &str) -> String {
    let cleaned: String = text
        .replace("\r\n", "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "None.".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::goal::Assumption;

    use super::*;

    fn goal() -> GoalFile {
        GoalFile {
            run: RunId::new("run-01").unwrap(),
            version: 1,
            summary: "Add a rate limit to the API.".to_owned(),
            criteria: vec![
                Criterion::new("G1", "Requests over the limit get 429"),
                Criterion::new("G2", "The limit is configurable"),
            ],
            in_scope: vec!["the HTTP API".to_owned()],
            out_of_scope: Vec::new(),
            assumptions: vec![Assumption {
                text: "limit: per user".to_owned(),
                basis: "a detail, and 2 of 3 read it that way".to_owned(),
                by: None,
            }],
            answers: Vec::new(),
            source: GoalSource::Readings,
            path: PathBuf::new(),
            sha256: String::new(),
        }
    }

    #[test]
    fn render_is_stable_markdown() {
        let text = render(&goal());
        assert_eq!(
            text,
            "# Goal for run-01\n\nVersion 1, from the crew's readings.\n\n## Summary\n\n\
             Add a rate limit to the API.\n\n## Success criteria\n\n\
             - G1: Requests over the limit get 429\n- G2: The limit is configurable\n\n\
             ## In scope\n\n- the HTTP API\n\n## Out of scope\n\nNone.\n\n## Assumptions\n\n\
             - limit: per user (a detail, and 2 of 3 read it that way)\n\n## Answers\n\nNone.\n"
        );
        assert_eq!(render(&goal()), text);
    }

    #[test]
    fn render_strips_control_sequences() {
        let mut dirty = goal();
        dirty.summary = "line one\r\nline \u{1b}[31mtwo".to_owned();
        dirty.in_scope = vec!["a\nb".to_owned()];
        let text = render(&dirty);
        assert!(!text.contains('\u{1b}') && !text.contains('\r'));
        assert!(text.contains("line one\nline [31mtwo"));
        assert!(text.contains("- a b\n"));
    }

    #[test]
    fn the_hash_follows_the_content() {
        let first = hashed(goal());
        assert_eq!(first.path, PathBuf::from("goals/run-01.md"));
        assert_eq!(first.sha256.len(), 64);
        assert_eq!(hashed(goal()).sha256, first.sha256);
        let mut other = goal();
        other.answers.push("Per API key".to_owned());
        assert_ne!(hashed(other).sha256, first.sha256);
    }

    #[test]
    fn edits_add_and_narrow_criteria_and_become_an_answer() {
        let note = "+ Limits reset every minute\nG2: The limit is set per route\nkeep it simple";
        let edited = with_edits(&goal(), note);
        let criteria: Vec<(&str, &str)> = edited
            .criteria
            .iter()
            .map(|c| (c.id.as_str(), c.text.as_str()))
            .collect();
        assert_eq!(
            criteria,
            [
                ("G1", "Requests over the limit get 429"),
                ("G2", "The limit is set per route"),
                ("G3", "Limits reset every minute"),
            ]
        );
        assert_eq!(
            edited.answers,
            [
                "Your edits: + Limits reset every minute; G2: The limit is set per route; keep it simple"
            ]
        );
        assert_eq!(with_edits(&edited, note), edited);
    }

    #[test]
    fn edits_never_drop_a_criterion() {
        let edited = with_edits(&goal(), "G1:\nG9: nothing to narrow\nadd:");
        assert_eq!(edited.criteria, goal().criteria);
        assert_eq!(edited.answers.len(), 1);
        assert_eq!(with_edits(&goal(), "  \n "), goal());
    }
}
