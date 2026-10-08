//! An approver must not be an author or owner of what it approves, and with
//! `different_provider` it must not share a model family with any of them.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::WorkerRef;
use argonauts_core::worker::WorkerDirectory;

/// Why `candidate` can't approve, or `Ok` when it's independent. An author whose family is
/// unknown counts as the same family. The reason reaches the transcript, so it names agents as
/// people read them, `Antigravity CLI` rather than `antigravity-cli`.
pub fn check(
    candidate: &WorkerRef,
    authors: &[WorkerRef],
    different_provider: bool,
    workers: &dyn WorkerDirectory,
) -> Result<(), String> {
    let name = |worker: &WorkerRef| DisplayNames::default().worker(worker);
    if authors.iter().any(|author| same_model(candidate, author)) {
        return Err(format!(
            "{} worked on this, so it can't approve it",
            name(candidate)
        ));
    }
    if !different_provider {
        return Ok(());
    }
    let Some(family) = family(candidate, workers) else {
        return Err(format!(
            "the provider behind {} isn't known, so it can't be shown to come from another \
             provider",
            name(candidate)
        ));
    };
    for author in authors {
        match self::family(author, workers) {
            Some(theirs) if theirs != family => {}
            Some(_) => {
                return Err(format!(
                    "{} comes from the same provider as {}, so it can't approve this",
                    name(candidate),
                    name(author)
                ));
            }
            None => {
                return Err(format!(
                    "the provider behind {} isn't known, so {} can't be shown to come from \
                     another provider",
                    name(author),
                    name(candidate)
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

    /// The reasons reach the transcript, so they name agents as people read them.
    #[test]
    fn reasons_name_agents_by_their_display_names() {
        let workers = directory();
        let author = check(
            &r("antigravity-cli"),
            &[r("antigravity-cli")],
            false,
            &workers,
        );
        assert_eq!(
            author.unwrap_err(),
            "Antigravity CLI worked on this, so it can't approve it"
        );
        let same = check(&r("openrouter/gpt-oss"), &[r("codex")], true, &workers);
        let same = same.unwrap_err();
        assert!(same.contains("same provider as Codex"), "{same}");
        assert!(!same.contains("codex"), "{same}");
    }

    #[test]
    fn another_provider_is_independent() {
        let workers = directory();
        assert!(check(&r("claude-code"), &[r("codex")], true, &workers).is_ok());
    }

    #[test]
    fn an_author_is_refused_even_without_different_provider() {
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
