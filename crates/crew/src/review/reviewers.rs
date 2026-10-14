//! Who reviews: the task's reviewers from the ring, never the worker, other providers than the
//! worker's while `review.other_provider` holds, and never the worker's notes.

use argonauts_core::TaskId;
use argonauts_core::names::ModelRef;

use crate::state::RunState;

/// The task's reviewers, skipping the worker and, under `other_provider`, any reviewer now of the
/// worker's provider (after a fallback, say). `providers` gives a worker's model provider from
/// the registry.
pub fn for_task(
    state: &RunState,
    task: &TaskId,
    providers: &dyn Fn(&ModelRef) -> String,
    other_provider: bool,
) -> Vec<ModelRef> {
    let Some(record) = state.tasks.get(task) else {
        return Vec::new();
    };
    let worker = record.worker.as_ref().or(record.spec.worker.as_ref());
    eligible(&record.spec.reviewers, worker, providers, other_provider)
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
        assert!(for_task(&RunState::default(), &other, &provider, true).is_empty());
    }
}
