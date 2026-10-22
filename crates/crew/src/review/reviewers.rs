//! Who reviews: the task's reviewers from the ring, never the worker, other providers than the
//! worker's while `review.other_provider` holds, and never the worker's notes. A task that names
//! no reviewers, as one a script made does, takes the crew's.

use argonauts_core::TaskId;
use argonauts_core::events::task::TaskKind;
use argonauts_core::names::ModelRef;

use crate::state::RunState;
use crate::state::tasks::TaskEntry;

/// Why a task has nobody to review it when it names none and the crew has none either.
pub const NONE_SET: &str = "No reviewer is set for this task";

/// The task's reviewers, skipping the worker and, under `other_provider`, any reviewer now of the
/// worker's provider (after a fallback, say). A task that names none takes `crew`, the crew's
/// reviewers, without the providers whose commits are on the task. `providers` gives a worker's
/// model provider from the registry.
pub fn for_task(
    state: &RunState,
    task: &TaskId,
    providers: &dyn Fn(&ModelRef) -> String,
    other_provider: bool,
    crew: &[ModelRef],
) -> Vec<ModelRef> {
    let Some(record) = state.tasks.get(task) else {
        return Vec::new();
    };
    if takes_the_crews(record) {
        return without_authors(crew, record, providers, other_provider);
    }
    let worker = record.worker.as_ref().or(record.spec.worker.as_ref());
    eligible(&record.spec.reviewers, worker, providers, other_provider)
}

/// Whether nobody could review the task whatever its worker: it names no reviewers and the
/// crew has none it could take.
pub fn none_set(state: &RunState, task: &TaskId, crew: &[ModelRef]) -> bool {
    state.tasks.get(task).is_some_and(|record| {
        record.spec.reviewers.is_empty() && (crew.is_empty() || !takes_the_crews(record))
    })
}

/// A task that names no reviewers takes the crew's, except a Captain's, whose review is its own.
fn takes_the_crews(record: &TaskEntry) -> bool {
    record.spec.reviewers.is_empty() && record.spec.kind != TaskKind::Captain
}

/// `crew` without the authors of the task's commits and, under `other_provider`, their
/// providers. A try that committed nothing isn't an author, so it doesn't keep its provider out.
fn without_authors(
    crew: &[ModelRef],
    record: &TaskEntry,
    providers: &dyn Fn(&ModelRef) -> String,
    other_provider: bool,
) -> Vec<ModelRef> {
    let authors: Vec<&ModelRef> = record
        .commits
        .iter()
        .filter(|commit| commit.sha.is_some())
        .filter_map(|commit| commit.by.as_ref())
        .collect();
    let authored: Vec<String> = authors.iter().map(|author| providers(author)).collect();
    let mut kept: Vec<ModelRef> = Vec::new();
    for reviewer in crew {
        let own = authors.contains(&reviewer);
        let same_provider = other_provider && authored.contains(&providers(reviewer));
        if !own && !same_provider && !kept.contains(reviewer) {
            kept.push(reviewer.clone());
        }
    }
    kept
}

/// `reviewers` without the worker, the worker's provider under `other_provider`, or repeats.
pub fn eligible(
    reviewers: &[ModelRef],
    worker: Option<&ModelRef>,
    providers: &dyn Fn(&ModelRef) -> String,
    other_provider: bool,
) -> Vec<ModelRef> {
    let worker_provider = worker.map(providers);
    let mut kept: Vec<ModelRef> = Vec::new();
    for reviewer in reviewers {
        let own = Some(reviewer) == worker;
        let same_provider =
            other_provider && worker_provider.as_deref() == Some(providers(reviewer).as_str());
        if !own && !same_provider && !kept.contains(reviewer) {
            kept.push(reviewer.clone());
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(worker: &ModelRef) -> String {
        match worker.agent.as_str() {
            "claude-code" => "claude",
            "codex" | "opencode" => "gpt",
            _ => "gemini",
        }
        .to_owned()
    }

    fn workers(names: &[&str]) -> Vec<ModelRef> {
        names.iter().map(|n| n.parse().unwrap()).collect()
    }

    #[test]
    fn reviewers_of_the_workers_provider_are_skipped() {
        // After a fallback to codex, the reviewer of Codex's own provider can't review.
        let reviewers = workers(&["opencode", "antigravity-cli", "codex", "antigravity-cli"]);
        let worker: ModelRef = "codex".parse().unwrap();
        let kept = eligible(&reviewers, Some(&worker), &provider, true);
        assert_eq!(kept, workers(&["antigravity-cli"]));
        // With review across providers off, only the worker itself is skipped.
        let kept = eligible(&reviewers, Some(&worker), &provider, false);
        assert_eq!(kept, workers(&["opencode", "antigravity-cli"]));
    }

    #[test]
    fn with_no_worker_every_distinct_reviewer_stays() {
        let reviewers = workers(&["codex", "codex", "claude-code"]);
        assert_eq!(
            eligible(&reviewers, None, &provider, true),
            workers(&["codex", "claude-code"])
        );
    }

    #[test]
    fn an_unknown_task_has_no_reviewers() {
        let other = TaskId::new("task-009").unwrap();
        let crew = workers(&["codex"]);
        assert!(for_task(&RunState::default(), &other, &provider, true, &crew).is_empty());
    }

    /// Claude Code built the task, then Codex tried again and committed nothing: Codex isn't an
    /// author, so it and agy may review, and Claude Code may not.
    #[test]
    fn a_task_that_names_no_reviewers_takes_the_crews_without_its_authors() {
        use argonauts_core::{CommitInfo, DiffStat};

        let task = TaskId::new("task-001").unwrap();
        let mut record = TaskEntry::new(crate::testutil::task_spec("task-001", &[], None));
        let commit = |by: &str, sha: Option<&str>| CommitInfo {
            branch: "argonauts/run/task-001".to_owned(),
            sha: sha.map(str::to_owned),
            message: String::new(),
            stat: DiffStat::default(),
            by: Some(by.parse().unwrap()),
        };
        record.commits = vec![
            commit("claude-code", Some("bee47e62")),
            commit("codex", None),
        ];
        record.worker = Some("codex".parse().unwrap());
        let mut state = RunState::default();
        state.tasks.by_id.insert(task.clone(), record);
        let crew = workers(&["claude-code", "codex", "antigravity-cli"]);
        assert_eq!(
            for_task(&state, &task, &provider, true, &crew),
            workers(&["codex", "antigravity-cli"])
        );
        assert!(!none_set(&state, &task, &crew));
        assert!(none_set(&state, &task, &[]));
    }
}
