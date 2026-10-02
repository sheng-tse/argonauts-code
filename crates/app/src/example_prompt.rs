//! The example in the empty input, `Try "…"`, made from the repo's recent commit subjects and
//! never by a model. A subject says what was done, so the example asks for the next step on
//! the same code: tests for what was added, a regression test for what was fixed.

use std::path::Path;

use crate::git::git;

/// How far back the subjects are read.
const RECENT: &str = "30";
/// Longer examples crowd the input line.
const MAX_CHARS: usize = 60;
/// Commit kinds whose subjects say nothing about code worth asking after.
const SKIPPED_KINDS: &[&str] = &["build", "chore", "ci", "release", "revert", "style"];

/// The example from the newest subject that makes one. `None` outside a repo, before its first
/// commit, or when no subject fits; the TUI then shows its own.
pub(crate) fn example_prompt(repo_root: &Path) -> Option<String> {
    let subjects = git(
        repo_root,
        &["log", "-n", RECENT, "--no-merges", "--format=%s"],
    )
    .ok()?;
    subjects.lines().find_map(from_subject)
}

/// One subject as an example, such as `fix(export): the flaky export test` as `add a
/// regression test for the flaky export test`.
fn from_subject(subject: &str) -> Option<String> {
    // Quotes would end the TUI's `Try "…"` early, and control characters have no place in it.
    if subject
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '`')
    {
        return None;
    }
    let (kind, scope, description) = parse(subject.trim());
    if kind.is_some_and(|kind| SKIPPED_KINDS.contains(&kind.as_str())) {
        return None;
    }
    let description = description.trim().trim_end_matches('.').trim();
    let (verb, rest) = description.split_once(' ').unwrap_or((description, ""));
    let rest = rest.trim();
    let verb = verb.to_lowercase();
    let made = match verb.as_str() {
        "add" | "adds" | "added" | "implement" | "implements" | "implemented" | "introduce"
        | "support" | "create" => Some(format!("write tests for {rest}")),
        "fix" | "fixes" | "fixed" => Some(format!("add a regression test for {rest}")),
        "remove" | "removes" | "removed" | "delete" | "drop" => {
            Some(format!("check nothing still uses {rest}"))
        }
        "refactor" | "simplify" | "split" | "extract" => {
            Some(format!("review the refactor of {rest}"))
        }
        _ => None,
    };
    let made = made
        .filter(|_| !rest.is_empty() && !starts_like_a_version(rest))
        .filter(|made| made.chars().count() <= MAX_CHARS);
    made.or_else(|| scope.map(|scope| format!("explain how the {scope} code works")))
        .filter(|made| made.chars().count() <= MAX_CHARS)
}

/// A conventional commit's kind and scope, and its description; a plain subject is all
/// description.
fn parse(subject: &str) -> (Option<String>, Option<String>, &str) {
    let Some((head, description)) = subject.split_once(": ") else {
        return (None, None, subject);
    };
    let head = head.trim_end_matches('!');
    let (kind, scope) = match head.split_once('(') {
        Some((kind, scope)) => (kind, scope.strip_suffix(')')),
        None => (head, None),
    };
    let word = |text: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
    };
    if !word(kind) || scope.is_some_and(|scope| !word(scope)) {
        return (None, None, subject);
    }
    (
        Some(kind.to_ascii_lowercase()),
        scope.map(str::to_owned),
        description,
    )
}

/// `v1.2.0` or `1.2`: a release, not code.
fn starts_like_a_version(text: &str) -> bool {
    let text = text.strip_prefix('v').unwrap_or(text);
    text.chars().next().is_some_and(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::TempRepo;

    use super::*;

    #[test]
    fn a_subject_becomes_the_next_step_on_the_same_code() {
        let cases = [
            (
                "feat(cli): add the app entry point",
                Some("write tests for the app entry point"),
            ),
            ("fix: the flaky export test", None),
            (
                "Fix the flaky export test",
                Some("add a regression test for the flaky export test"),
            ),
            (
                "refactor(tui)!: split the composer",
                Some("review the refactor of the composer"),
            ),
            (
                "Remove the legacy TUI exports.",
                Some("check nothing still uses the legacy TUI exports"),
            ),
            (
                "docs(registry): describe the layers",
                Some("explain how the registry code works"),
            ),
            ("chore(deps): bump serde", None),
            ("Update README", None),
            ("add v2.0.0", None),
            ("add \"quoted\" names", None),
            (
                "feat(crew): add a very long description that goes on and on past the input",
                Some("explain how the crew code works"),
            ),
        ];
        for (subject, expected) in cases {
            assert_eq!(
                from_subject(subject).as_deref(),
                expected,
                "from {subject:?}"
            );
        }
    }

    #[test]
    fn the_newest_subject_that_fits_is_used() {
        let repo = TempRepo::new().unwrap();
        repo.write("a.txt", "a").unwrap();
        repo.commit_all("feat(export): add a --json flag").unwrap();
        repo.write("b.txt", "b").unwrap();
        repo.commit_all("chore: tidy").unwrap();
        assert_eq!(
            example_prompt(repo.root()).as_deref(),
            Some("write tests for a --json flag")
        );
    }

    #[test]
    fn no_fitting_subject_or_no_repo_means_none() {
        let repo = TempRepo::new().unwrap();
        assert_eq!(example_prompt(repo.root()), None, "only \"initial commit\"");
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(example_prompt(plain.path()), None);
    }
}
