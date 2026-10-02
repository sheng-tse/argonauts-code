//! An approver must not be an author or owner of what it approves, and with
//! `different_family` it must not share a model family with any of them.

use argonauts_core::names::WorkerRef;
use argonauts_core::worker::WorkerDirectory;

/// Why `candidate` can't approve, or `Ok` when it's independent. An author whose family is
/// unknown counts as the same family.
pub fn check(
    candidate: &WorkerRef,
    authors: &[WorkerRef],
    different_family: bool,
    workers: &dyn WorkerDirectory,
) -> Result<(), String> {
    if let Some(author) = authors.iter().find(|author| same_model(candidate, author)) {
        return Err(format!(
            "{candidate} can't approve work {author} wrote or owns"
        ));
    }
    if !different_family {
        return Ok(());
    }
    let Some(family) = family(candidate, workers) else {
        return Err(format!(
            "{candidate}'s model family is unknown, so it can't be shown to differ from the authors'"
        ));
    };
    for author in authors {
        match self::family(author, workers) {
            Some(theirs) if theirs != family => {}
            Some(_) => {
                return Err(format!(
                    "{candidate} is in the same model family ({family}) as {author}"
                ));
            }
            None => {
                return Err(format!(
                    "{author}'s model family is unknown, so {candidate} counts as the same family"
                ));
            }
        }
    }
    Ok(())
}

/// The same worker, and the same model or one side left to the worker's default, which could
/// be the other's.
fn same_model(a: &WorkerRef, b: &WorkerRef) -> bool {
    a.worker == b.worker && (a.model.is_none() || b.model.is_none() || a.model == b.model)
}

fn family(worker: &WorkerRef, workers: &dyn WorkerDirectory) -> Option<String> {
    let family = workers.get(worker).ok()?.family();
    let family = family.trim().to_lowercase();
    (!family.is_empty()).then_some(family)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_testkit::{FakeDirectory, FakeWorker};

    fn r(text: &str) -> WorkerRef {
        text.parse().unwrap()
    }

    fn directory() -> FakeDirectory {
        let workers = FakeDirectory::new();
        workers.add(FakeWorker::harness("codex", "gpt"));
        workers.add(FakeWorker::harness("claude-code", "claude"));
        workers.add(FakeWorker::endpoint("openrouter/glm", "glm"));
        workers.add(FakeWorker::endpoint("openrouter/gpt-oss", "GPT"));
        workers.add(FakeWorker::endpoint("openrouter/mystery", ""));
        workers
    }

    #[test]
    fn another_family_is_independent() {
        let workers = directory();
        assert!(check(&r("claude-code"), &[r("codex")], true, &workers).is_ok());
    }

    #[test]
    fn an_author_is_refused_even_without_different_family() {
        let workers = directory();
        assert!(check(&r("codex"), &[r("codex")], false, &workers).is_err());
        // An unset model could be the author's.
        assert!(check(&r("codex"), &[r("codex/gpt-x")], false, &workers).is_err());
        assert!(check(&r("codex/gpt-y"), &[r("codex/gpt-x")], false, &workers).is_ok());
    }

    #[test]
    fn a_shared_or_unknown_family_is_refused() {
        let workers = directory();
        assert!(check(&r("openrouter/gpt-oss"), &[r("codex")], true, &workers).is_err());
        assert!(
            check(
                &r("claude-code"),
                &[r("openrouter/mystery")],
                true,
                &workers
            )
            .is_err()
        );
        assert!(check(&r("claude-code"), &[r("nobody")], true, &workers).is_err());
        assert!(check(&r("openrouter/mystery"), &[r("codex")], true, &workers).is_err());
        assert!(check(&r("openrouter/gpt-oss"), &[r("codex")], false, &workers).is_ok());
    }
}
