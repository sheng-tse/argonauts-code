//! The review ring: each task's reviewers (`review.per_task`) come from other model providers
//! than its worker (`review.other_provider`). Chores (merge, docs) rotate.

use argonauts_core::names::ModelRef;

/// `candidates` are `(worker, provider)`, the worker already left out. Rotates by `offset` so the
/// load spreads. Fewer than `per_task` come back when too few fit.
pub fn reviewers(
    worker_provider: &str,
    candidates: &[(ModelRef, String)],
    per_task: usize,
    other_provider: bool,
    offset: usize,
) -> Vec<ModelRef> {
    let mut pool: Vec<&ModelRef> = Vec::new();
    for (worker, provider) in candidates {
        if (!other_provider || provider != worker_provider) && !pool.contains(&worker) {
            pool.push(worker);
        }
    }
    if pool.is_empty() {
        return Vec::new();
    }
    let start = offset % pool.len();
    pool.iter()
        .cycle()
        .skip(start)
        .take(per_task.min(pool.len()))
        .map(|worker| (*worker).clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crew() -> Vec<(ModelRef, String)> {
        [
            ("claude-code/opus", "anthropic"),
            ("codex", "openai"),
            ("agy/gemini-3.6-pro", "google"),
            ("claude-code/sonnet", "anthropic"),
        ]
        .into_iter()
        .map(|(worker, provider)| (worker.parse().unwrap(), provider.to_owned()))
        .collect()
    }

    fn names(workers: &[ModelRef]) -> Vec<String> {
        workers.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn divide_review_ring_other_provider() {
        let crew = crew();
        for offset in 0..6 {
            for reviewer in reviewers("anthropic", &crew, 2, true, offset) {
                assert!(!reviewer.agent.starts_with("claude"), "offset {offset}");
            }
        }
        assert_eq!(names(&reviewers("anthropic", &crew, 1, true, 0)), ["codex"]);
        assert_eq!(
            names(&reviewers("anthropic", &crew, 1, true, 1)),
            ["agy/gemini-3.6-pro"]
        );
        assert_eq!(
            names(&reviewers("anthropic", &crew, 2, true, 1)),
            ["agy/gemini-3.6-pro", "codex"]
        );
        // Only one other provider: one reviewer, however many are asked for.
        assert_eq!(
            names(&reviewers("openai", &crew[..2], 2, true, 0)),
            ["claude-code/opus"]
        );
        // A one-provider crew has no ring unless the setting allows the same provider.
        let same: Vec<_> = crew
            .iter()
            .filter(|(_, provider)| provider == "anthropic")
            .cloned()
            .collect();
        assert!(reviewers("anthropic", &same, 1, true, 0).is_empty());
        assert_eq!(
            names(&reviewers("anthropic", &same, 1, false, 1)),
            ["claude-code/sonnet"]
        );
    }
}
