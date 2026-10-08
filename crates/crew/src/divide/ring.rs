//! The review ring: each card's reviewers (`review.per_task`) come from other model families
//! than its owner (`review.other_provider`). Chores (integration, docs) rotate.

use argonauts_core::names::WorkerRef;

/// `candidates` are `(worker, family)`, the owner already left out. Rotates by `offset` so the
/// load spreads. Fewer than `per_task` come back when too few fit.
pub fn reviewers(
    owner_family: &str,
    candidates: &[(WorkerRef, String)],
    per_task: usize,
    other_provider: bool,
    offset: usize,
) -> Vec<WorkerRef> {
    let mut pool: Vec<&WorkerRef> = Vec::new();
    for (worker, family) in candidates {
        if (!other_provider || family != owner_family) && !pool.contains(&worker) {
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

    fn crew() -> Vec<(WorkerRef, String)> {
        [
            ("claude-code/opus", "claude"),
            ("codex", "gpt"),
            ("agy/gemini-3.6-pro", "gemini"),
            ("claude-code/sonnet", "claude"),
        ]
        .into_iter()
        .map(|(worker, family)| (worker.parse().unwrap(), family.to_owned()))
        .collect()
    }

    fn names(workers: &[WorkerRef]) -> Vec<String> {
        workers.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn divide_review_ring_other_provider() {
        let crew = crew();
        for offset in 0..6 {
            for reviewer in reviewers("claude", &crew, 2, true, offset) {
                assert!(!reviewer.worker.starts_with("claude"), "offset {offset}");
            }
        }
        assert_eq!(names(&reviewers("claude", &crew, 1, true, 0)), ["codex"]);
        assert_eq!(
            names(&reviewers("claude", &crew, 1, true, 1)),
            ["agy/gemini-3.6-pro"]
        );
        assert_eq!(
            names(&reviewers("claude", &crew, 2, true, 1)),
            ["agy/gemini-3.6-pro", "codex"]
        );
        // Only one other family: one reviewer, however many are asked for.
        assert_eq!(
            names(&reviewers("gpt", &crew[..2], 2, true, 0)),
            ["claude-code/opus"]
        );
        // A one-family crew has no ring unless the setting allows the same family.
        let same: Vec<_> = crew
            .iter()
            .filter(|(_, f)| f == "claude")
            .cloned()
            .collect();
        assert!(reviewers("claude", &same, 1, true, 0).is_empty());
        assert_eq!(
            names(&reviewers("claude", &same, 1, false, 1)),
            ["claude-code/sonnet"]
        );
    }
}
