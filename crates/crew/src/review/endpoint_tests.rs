//! An endpoint reviewer writes acceptance tests as `AcceptanceTests` output. Files outside the
//! test card's `owned_paths` are rejected; the rest are written into the test card's worktree,
//! committed, and gated. A test that doesn't compile or passes against the stubs gets one more
//! request carrying the gate output.

use std::path::{Component, Path};

use globset::Glob;

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, ReviewEvent};
use argonauts_core::events::session::InputPartKind;
use argonauts_core::names::RoleName;
use argonauts_core::steps::card::{AcceptanceTests, AcceptanceTestsInput, TestFile};
use argonauts_core::worker::Access;
use argonauts_core::{Note, NoteLevel};

use crate::card::{gates, worktree};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::plan::part;
use crate::prompts::review::ACCEPTANCE_TESTS;

/// Splits files into the ones inside `owned_paths` and the rejected ones. A path that is
/// absolute, climbs out with `..`, or reaches into `.git` is always rejected.
pub fn split(files: &[TestFile], owned_paths: &[String]) -> (Vec<TestFile>, Vec<TestFile>) {
    files
        .iter()
        .cloned()
        .partition(|file| owned(&file.path, owned_paths))
}

fn owned(path: &str, owned_paths: &[String]) -> bool {
    let path = path.strip_prefix("./").unwrap_or(path);
    let safe = !path.is_empty()
        && Path::new(path).components().all(|c| match c {
            Component::Normal(name) => name != ".git",
            _ => false,
        });
    safe && owned_paths.iter().any(|pattern| matches(path, pattern))
}

fn matches(path: &str, pattern: &str) -> bool {
    let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
    if pattern.contains(['*', '?', '[', '{']) {
        return Glob::new(pattern).is_ok_and(|glob| glob.compile_matcher().is_match(path));
    }
    match pattern.strip_suffix('/') {
        Some(dir) => path.starts_with(dir) && path[dir.len()..].starts_with('/'),
        None => {
            path == pattern || (path.starts_with(pattern) && path[pattern.len()..].starts_with('/'))
        }
    }
}

/// Asks the test card's endpoint reviewer for its tests, writes the owned ones into the test
/// card's worktree, commits, and gates them. Returns the files written. When the gates fail,
/// the reviewer gets one more request with their output; failing again is an error, and the
/// card escalates.
pub async fn write_tests(host: &Host, test_card: &CardId) -> EngineResult<Vec<String>> {
    let (spec, reviewer, target) = host
        .read(|state| {
            let record = state.cards.get(test_card)?;
            let CardKind::AcceptanceTests { for_card } = &record.spec.kind else {
                return None;
            };
            let reviewer = record.owner.clone().or_else(|| record.spec.owner.clone())?;
            let target = state.cards.get(for_card).map(|r| r.spec.clone())?;
            Some((record.spec.clone(), reviewer, target))
        })
        .ok_or_else(|| {
            EngineError::Refused(format!(
                "{test_card} is not an acceptance-test card with a reviewer"
            ))
        })?;
    let tree = worktree::ensure(host, test_card).await?;
    let interfaces = spec
        .read_only_paths
        .iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(tree.path.join(path)).ok()?;
            Some(format!("{path}:\n{text}"))
        })
        .collect();
    let input = AcceptanceTestsInput {
        card: format!("{}\n\n{}", target.title, target.task),
        criteria: target.criteria.clone(),
        interfaces,
        test_dir: spec.owned_paths.first().cloned().unwrap_or_default(),
    };
    let mut gate_output: Option<String> = None;
    for attempt in 0..2u32 {
        let mut ask = Ask::new(
            None,
            RoleName::Reviewer,
            reviewer.clone(),
            ACCEPTANCE_TESTS,
            input.clone(),
        );
        ask.card = Some(test_card.clone());
        ask.access = Access::NoTools;
        ask.blind = true;
        ask.slot = attempt;
        if let Some(output) = &gate_output {
            ask.parts.push(part(
                InputPartKind::GateOutput,
                "gate output",
                output.clone(),
            ));
        }
        let answered = host.ask::<AcceptanceTests>(ask).await?;
        let (kept, rejected) = split(&answered.output.files, &spec.owned_paths);
        if !rejected.is_empty() {
            let paths: Vec<&str> = rejected.iter().map(|f| f.path.as_str()).collect();
            host.record(
                Some(test_card),
                Some(&answered.session),
                RunEventKind::Note(Note {
                    level: NoteLevel::Warn,
                    text: format!(
                        "Rejected acceptance tests outside the test card's paths: {}",
                        paths.join(", ")
                    ),
                }),
            )?;
        }
        if kept.is_empty() {
            return Err(EngineError::Refused(format!(
                "{reviewer} wrote no acceptance tests inside {}",
                spec.owned_paths.join(", ")
            )));
        }
        for file in &kept {
            let path = tree
                .path
                .join(file.path.strip_prefix("./").unwrap_or(&file.path));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &file.content)?;
        }
        worktree::commit_after_end(host, test_card, &answered.session).await?;
        let written: Vec<String> = kept.iter().map(|f| f.path.clone()).collect();
        match gates::run(host, test_card).await? {
            gates::GateVerdict::Passed => {
                host.record(
                    Some(test_card),
                    Some(&answered.session),
                    RunEventKind::Review(ReviewEvent::TestsWritten {
                        reviewer: reviewer.clone(),
                        for_card: target.id.clone(),
                        files: written.clone(),
                    }),
                )?;
                return Ok(written);
            }
            gates::GateVerdict::Failed { output } => gate_output = Some(output),
            gates::GateVerdict::Stopped { reason } => return Err(EngineError::Refused(reason)),
        }
    }
    Err(EngineError::Refused(format!(
        "the acceptance tests for {} failed their gates twice",
        target.id
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> TestFile {
        TestFile {
            path: path.to_owned(),
            content: "test".to_owned(),
        }
    }

    fn paths(files: &[TestFile]) -> Vec<&str> {
        files.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn files_inside_owned_paths_are_kept() {
        let owned = vec![
            "tests/limit/".to_owned(),
            "tests/limit_api.rs".to_owned(),
            "spec/**/*.test.ts".to_owned(),
        ];
        let files = vec![
            file("tests/limit/bucket.rs"),
            file("./tests/limit/deep/more.rs"),
            file("tests/limit_api.rs"),
            file("spec/a/b.test.ts"),
            file("tests/limiter.rs"),
            file("tests/limit_api.rs.bak"),
            file("src/lib.rs"),
        ];
        let (kept, rejected) = split(&files, &owned);
        assert_eq!(
            paths(&kept),
            [
                "tests/limit/bucket.rs",
                "./tests/limit/deep/more.rs",
                "tests/limit_api.rs",
                "spec/a/b.test.ts"
            ]
        );
        assert_eq!(
            paths(&rejected),
            ["tests/limiter.rs", "tests/limit_api.rs.bak", "src/lib.rs"]
        );
    }

    #[test]
    fn escapes_are_always_rejected() {
        let owned = vec!["tests/".to_owned(), "**".to_owned()];
        let files = vec![
            file("tests/../src/lib.rs"),
            file("/etc/passwd"),
            file("tests/.git/hooks/pre-commit"),
            file(""),
        ];
        let (kept, rejected) = split(&files, &owned);
        assert!(kept.is_empty());
        assert_eq!(rejected.len(), 4);
    }

    #[test]
    fn no_owned_paths_means_nothing_is_owned() {
        let (kept, rejected) = split(&[file("tests/a.rs")], &[]);
        assert!(kept.is_empty());
        assert_eq!(rejected.len(), 1);
    }
}
