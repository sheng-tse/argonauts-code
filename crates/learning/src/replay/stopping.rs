//! When a replay's result is clear enough to stop spending on it.

use serde::{Deserialize, Serialize};

use crate::types::{LearnedWorkflowRef, ReplayResult};

/// The one-sided p-value a sign test must beat for the replay to stop at an interim look.
const INTERIM_P: f64 = 0.001;

/// What the tuning set says about the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Its gain reaches the margin.
    Passes,
    Fails,
}

/// The stopping rule, fixed before a replay's first run and stored with its progress, so
/// nothing seen during the replay, and no settings change after it starts, can move it.
///
/// The child passes when its gain, the mean over tuning tasks of its pass rate less the
/// parent's, reaches `margin`. A task dropped after its first pair, because both versions
/// passed it or both failed it, counts as no gain. With `sequential` on, the replay stops
/// after any pair once either of these holds:
/// - **Curtailment.** Whether every run still planned went the child's way or every one went
///   the parent's, the verdict would be the same. Stopping then never changes the answer.
/// - **Haybittle-Peto.** On the pairs where exactly one version passed, an exact one-sided sign
///   test gives p < 0.001 in one direction, and the gain so far agrees. A bound that strict at
///   every look keeps the chance of a wrong early call close to a single test's.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StoppingRule {
    /// The gain needed, as a fraction: 0.10 is 10 points. A not-worse class has -0.05.
    pub margin: f64,
    pub runs_per_task: u32,
    pub sequential: bool,
}

/// One task's paired runs so far.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Pairs {
    done: u32,
    parent_passed: u32,
    child_passed: u32,
    /// Only one passed.
    child_only: u32,
    parent_only: u32,
    planned: u32,
}

impl StoppingRule {
    /// The verdict on what has run.
    pub fn verdict(
        &self,
        tasks: &[String],
        results: &[ReplayResult],
        parent: &LearnedWorkflowRef,
        child: &LearnedWorkflowRef,
    ) -> Verdict {
        let pairs = self.pairs(tasks, results, parent, child);
        if self.gain(&pairs, Outlook::SoFar) >= self.margin {
            Verdict::Passes
        } else {
            Verdict::Fails
        }
    }

    /// The verdict, if the rule says it's clear before every planned run is done.
    pub fn clear(
        &self,
        tasks: &[String],
        results: &[ReplayResult],
        parent: &LearnedWorkflowRef,
        child: &LearnedWorkflowRef,
    ) -> Option<Verdict> {
        let pairs = self.pairs(tasks, results, parent, child);
        let best = self.gain(&pairs, Outlook::Best);
        let worst = self.gain(&pairs, Outlook::Worst);
        if worst >= self.margin {
            return Some(Verdict::Passes);
        }
        if best < self.margin {
            return Some(Verdict::Fails);
        }
        let child_only: u32 = pairs.iter().map(|task| task.child_only).sum();
        let parent_only: u32 = pairs.iter().map(|task| task.parent_only).sum();
        let so_far = self.gain(&pairs, Outlook::SoFar);
        if sign_test(child_only, parent_only) < INTERIM_P && so_far >= self.margin {
            return Some(Verdict::Passes);
        }
        if sign_test(parent_only, child_only) < INTERIM_P && so_far < self.margin {
            return Some(Verdict::Fails);
        }
        None
    }

    /// Whether a task's first pair went the same way for both versions, so it's dropped.
    pub fn dropped(
        task: &str,
        results: &[ReplayResult],
        parent: &LearnedWorkflowRef,
        child: &LearnedWorkflowRef,
    ) -> bool {
        let first = |learned_workflow: &LearnedWorkflowRef| {
            results
                .iter()
                .find(|result| {
                    !result.holdout
                        && result.task == task
                        && result.try_number == 1
                        && &result.learned_workflow == learned_workflow
                })
                .map(|result| result.passed)
        };
        matches!((first(parent), first(child)), (Some(a), Some(b)) if a == b)
    }

    fn pairs(
        &self,
        tasks: &[String],
        results: &[ReplayResult],
        parent: &LearnedWorkflowRef,
        child: &LearnedWorkflowRef,
    ) -> Vec<Pairs> {
        let runs = self.runs_per_task.max(1);
        tasks
            .iter()
            .map(|task| {
                let mut pairs = Pairs {
                    planned: if Self::dropped(task, results, parent, child) {
                        1
                    } else {
                        runs
                    },
                    ..Pairs::default()
                };
                for try_number in 1..=runs {
                    let passed = |learned_workflow: &LearnedWorkflowRef| {
                        results
                            .iter()
                            .find(|result| {
                                !result.holdout
                                    && &result.task == task
                                    && result.try_number == try_number
                                    && &result.learned_workflow == learned_workflow
                            })
                            .map(|result| result.passed)
                    };
                    if let (Some(p), Some(c)) = (passed(parent), passed(child)) {
                        pairs.done += 1;
                        pairs.parent_passed += u32::from(p);
                        pairs.child_passed += u32::from(c);
                        pairs.child_only += u32::from(c && !p);
                        pairs.parent_only += u32::from(p && !c);
                    }
                }
                pairs
            })
            .collect()
    }

    fn gain(&self, tasks: &[Pairs], outlook: Outlook) -> f64 {
        let mut total = 0.0;
        let mut counted = 0u32;
        for task in tasks {
            let left = task.planned.saturating_sub(task.done);
            let (child, parent, runs) = match outlook {
                Outlook::SoFar if task.done == 0 => continue,
                Outlook::SoFar => (task.child_passed, task.parent_passed, task.done),
                Outlook::Best => (task.child_passed + left, task.parent_passed, task.planned),
                Outlook::Worst => (task.child_passed, task.parent_passed + left, task.planned),
            };
            total += (f64::from(child) - f64::from(parent)) / f64::from(runs.max(1));
            counted += 1;
        }
        if counted == 0 {
            0.0
        } else {
            total / f64::from(counted)
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Outlook {
    SoFar,
    /// Every planned run left goes the child's way.
    Best,
    /// Every one goes the parent's way.
    Worst,
}

/// The chance of `wins` or more out of `wins + losses` fair coin flips.
fn sign_test(wins: u32, losses: u32) -> f64 {
    let n = wins + losses;
    if n == 0 {
        return 1.0;
    }
    // Log space, so 2^-n doesn't underflow for a long replay.
    let mut log_pmf = -f64::from(n) * std::f64::consts::LN_2;
    let mut tail = 0.0;
    for i in 0..=n {
        if i >= wins {
            tail += log_pmf.exp();
        }
        if i < n {
            log_pmf += (f64::from(n - i) / f64::from(i + 1)).ln();
        }
    }
    tail.min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_test_matches_the_binomial_tail() {
        assert!((sign_test(0, 0) - 1.0).abs() < 1e-12);
        assert!((sign_test(1, 0) - 0.5).abs() < 1e-12);
        assert!((sign_test(10, 0) - 1.0 / 1024.0).abs() < 1e-12);
        assert!((sign_test(2, 1) - 0.5).abs() < 1e-12);
        assert!(sign_test(1500, 1500) > 0.4);
    }
}
