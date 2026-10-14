//! Keeping a goal: what follows each check. There's no fixed number of tries. A goal ends when
//! it holds, when you pause or clear it, or when it's stuck: the same failure
//! `goal.stuck_after` rounds running makes it change approach once, and stuck again it stops
//! and says why. `max_rounds` is a cap only when you set one.
//!
//! Nothing here runs anything, so the engine keeps a run's goals with it and the conversation keeps
//! the main agent's.

use crate::events::goal::{CheckedBy, Criterion, CriterionState, GoalChecked, GoalVerdict};

use super::report::{Failing, FailingWhat};

/// When a goal stops being tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// `goal.stuck_after`.
    pub stuck_after: u32,
    /// `goal.max_rounds`, or the goal's own.
    pub max_rounds: Option<u32>,
}

/// What follows a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Every line holds.
    Met,
    /// Not yet: the next try starts from what still fails.
    Again,
    /// The same failure came back `stuck_after` rounds running. Once, the next try is told to
    /// take another approach.
    ChangeApproach { why: String },
    /// It stops short: stuck after it changed approach, out of the rounds you set, or the check
    /// says it can't be met.
    Stop { why: String },
}

/// What follows the newest of `checks`, one for each try so far, oldest first.
pub fn next(checks: &[&GoalChecked], limits: Limits, approach_changed: bool) -> Next {
    let Some(last) = checks.last() else {
        return Next::Again;
    };
    match last.overall {
        GoalVerdict::Met => return Next::Met,
        GoalVerdict::Impossible => {
            return Next::Stop {
                why: format!("the check says it can't be met: {}", last.reason),
            };
        }
        GoalVerdict::NotYet => {}
    }
    let tries = u32::try_from(checks.len()).unwrap_or(u32::MAX);
    if let Some(max) = limits.max_rounds
        && tries >= max.max(1)
    {
        return Next::Stop {
            why: match max {
                1 => "it didn't hold after the 1 round you allow".to_owned(),
                max => format!("it didn't hold after the {max} rounds you allow"),
            },
        };
    }
    let same = limits.stuck_after.max(1) as usize;
    let stuck = checks.len() >= same && {
        let window = &checks[checks.len() - same..];
        let first = signature(window[0]);
        !first.is_empty() && window.iter().all(|checked| signature(checked) == first)
    };
    if !stuck {
        return Next::Again;
    }
    let why = match same {
        1 => "it failed the same way again".to_owned(),
        same => format!("the same failure {same} rounds running"),
    };
    if approach_changed {
        Next::Stop {
            why: format!("{why}, after a change of approach"),
        }
    } else {
        Next::ChangeApproach { why }
    }
}

/// What a check failed on: each line not met, with how many of its command's tests failed when
/// that was counted. Two checks with the same signature failed the same way; fewer failing
/// tests is progress, and so is one line more met.
pub fn signature(checked: &GoalChecked) -> Vec<(String, Option<u32>)> {
    checked
        .verdicts
        .iter()
        .filter(|verdict| verdict.verdict != CriterionState::Met)
        .map(|verdict| {
            let failing = checked
                .checks
                .iter()
                .find(|run| {
                    run.by == CheckedBy::Command && !run.passed && run.lines.contains(&verdict.id)
                })
                .and_then(|run| run.failed);
            (verdict.id.clone(), failing)
        })
        .collect()
}

/// What still failed after a try, for the bar beside it: failing tests where a command counted
/// them, else the lines not met.
pub fn failing(checked: &GoalChecked) -> Failing {
    let tests: Vec<u32> = checked
        .checks
        .iter()
        .filter(|run| run.by == CheckedBy::Command && !run.passed)
        .filter_map(|run| run.failed)
        .collect();
    if !tests.is_empty() {
        return Failing {
            count: tests.iter().sum(),
            what: FailingWhat::Tests,
        };
    }
    let unmet = checked
        .verdicts
        .iter()
        .filter(|verdict| verdict.verdict != CriterionState::Met)
        .count();
    Failing {
        count: u32::try_from(unmet).unwrap_or(u32::MAX),
        what: FailingWhat::Lines,
    }
}

/// What goes back to the agent as its next try: each line that doesn't hold with what still
/// fails, then the end of each failed command's output.
pub fn send_back(
    lines: &[Criterion],
    checked: &GoalChecked,
    change_approach: Option<&str>,
) -> String {
    let mut text = String::from(
        "The goal doesn't hold yet. Argonauts Code ran the check itself, and this still fails:\n",
    );
    for verdict in checked
        .verdicts
        .iter()
        .filter(|verdict| verdict.verdict != CriterionState::Met)
    {
        let line = lines
            .iter()
            .find(|line| line.id == verdict.id)
            .map_or(verdict.id.as_str(), |line| line.text.as_str());
        match verdict.missing.trim() {
            "" => text.push_str(&format!("- {line}\n")),
            missing => text.push_str(&format!("- {line}: {missing}\n")),
        }
    }
    for run in checked
        .checks
        .iter()
        .filter(|run| run.by == CheckedBy::Command && !run.passed && !run.output.trim().is_empty())
    {
        text.push_str(&format!(
            "\n{} printed:\n{}\n",
            run.name,
            run.output.trim_end()
        ));
    }
    if let Some(why) = change_approach {
        text.push_str(&format!(
            "\nThis is {why}. Take a different approach this time: say in one line what you'll \
             do differently, then do it.\n"
        ));
    }
    text.push_str("\nMake every line hold, then end with your report again.");
    text
}

/// `3 still fail`, `1 still fails`, for a line or a round.
pub fn still_fail_words(count: u32) -> String {
    match count {
        1 => "1 still fails".to_owned(),
        count => format!("{count} still fail"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::goal::{CheckRun, CriterionVerdict};

    const LIMITS: Limits = Limits {
        stuck_after: 3,
        max_rounds: None,
    };

    fn verdict(id: &str, met: bool) -> CriterionVerdict {
        CriterionVerdict {
            id: id.to_owned(),
            verdict: if met {
                CriterionState::Met
            } else {
                CriterionState::NotYet
            },
            evidence: String::new(),
            missing: if met {
                String::new()
            } else {
                "1 of 12 fails".to_owned()
            },
        }
    }

    /// A check of two lines: G1 met or not, G2 failing `failed` tests (met at 0).
    fn checked(try_number: u32, first_met: bool, failed: u32) -> GoalChecked {
        let met = first_met && failed == 0;
        GoalChecked {
            try_number,
            verdicts: vec![verdict("G1", first_met), verdict("G2", failed == 0)],
            overall: if met {
                GoalVerdict::Met
            } else {
                GoalVerdict::NotYet
            },
            reason: String::new(),
            task: None,
            checks: vec![CheckRun {
                name: "cargo test overdue".to_owned(),
                by: CheckedBy::Command,
                lines: vec!["G2".to_owned()],
                passed: failed == 0,
                summary: String::new(),
                failed: Some(failed),
                total: Some(12),
                ms: 0,
                output: if failed == 0 {
                    String::new()
                } else {
                    "overdue::skips_tasks_without_a_date".to_owned()
                },
                agent: None,
            }],
        }
    }

    fn after(checks: &[GoalChecked], limits: Limits, changed: bool) -> Next {
        let checks: Vec<&GoalChecked> = checks.iter().collect();
        next(&checks, limits, changed)
    }

    #[test]
    fn a_goal_goes_on_while_it_makes_progress_with_no_fixed_number_of_tries() {
        // Fewer failing tests each round is progress, however many rounds it takes.
        let mut checks = Vec::new();
        for (n, failed) in (1..).zip([9, 8, 7, 6, 5, 4, 3, 2, 1]) {
            checks.push(checked(n, true, failed));
            assert_eq!(after(&checks, LIMITS, false), Next::Again, "round {n}");
        }
        checks.push(checked(10, true, 0));
        assert_eq!(after(&checks, LIMITS, false), Next::Met);
    }

    #[test]
    fn the_same_failure_three_rounds_running_changes_approach_once_then_stops() {
        let stuck = [
            checked(1, true, 2),
            checked(2, true, 2),
            checked(3, true, 2),
        ];
        assert_eq!(after(&stuck[..2], LIMITS, false), Next::Again);
        assert_eq!(
            after(&stuck, LIMITS, false),
            Next::ChangeApproach {
                why: "the same failure 3 rounds running".to_owned()
            }
        );
        // The new approach fails the same way: it stops and says why.
        let mut again = stuck.to_vec();
        again.push(checked(4, true, 2));
        assert_eq!(
            after(&again, LIMITS, true),
            Next::Stop {
                why: "the same failure 3 rounds running, after a change of approach".to_owned()
            }
        );
        // The new approach moved it, so it goes on.
        let mut moved = stuck.to_vec();
        moved.push(checked(4, true, 1));
        assert_eq!(after(&moved, LIMITS, true), Next::Again);
    }

    #[test]
    fn a_cap_is_yours_to_set() {
        let capped = Limits {
            stuck_after: 3,
            max_rounds: Some(2),
        };
        let checks = [checked(1, false, 3), checked(2, true, 1)];
        assert_eq!(after(&checks[..1], capped, false), Next::Again);
        assert_eq!(
            after(&checks, capped, false),
            Next::Stop {
                why: "it didn't hold after the 2 rounds you allow".to_owned()
            }
        );
        // Without one, the same two checks go on.
        assert_eq!(after(&checks, LIMITS, false), Next::Again);
    }

    #[test]
    fn a_goal_the_check_says_cant_be_met_stops() {
        let mut impossible = checked(1, false, 1);
        impossible.overall = GoalVerdict::Impossible;
        impossible.reason = "the API it needs was removed".to_owned();
        assert_eq!(
            after(&[impossible], LIMITS, false),
            Next::Stop {
                why: "the check says it can't be met: the API it needs was removed".to_owned()
            }
        );
    }

    #[test]
    fn what_still_failed_counts_tests_where_a_command_counted_them() {
        assert_eq!(
            failing(&checked(1, false, 4)),
            Failing {
                count: 4,
                what: FailingWhat::Tests
            }
        );
        let mut judged = checked(1, false, 1);
        judged.checks.clear();
        assert_eq!(
            failing(&judged),
            Failing {
                count: 2,
                what: FailingWhat::Lines
            }
        );
        assert_eq!(still_fail_words(1), "1 still fails");
        assert_eq!(still_fail_words(3), "3 still fail");
    }

    #[test]
    fn what_goes_back_names_each_failing_line_and_the_commands_output() {
        let lines = vec![
            Criterion::new("G1", "tasks overdue lists tasks due before today"),
            Criterion::new("G2", "every test in tests/overdue.rs passes"),
        ];
        let text = send_back(&lines, &checked(3, true, 1), None);
        assert!(text.contains("- every test in tests/overdue.rs passes: 1 of 12 fails"));
        assert!(!text.contains("tasks overdue lists"), "{text}");
        assert!(text.contains("cargo test overdue printed:\noverdue::skips_tasks_without_a_date"));
        assert!(!text.contains("different approach"));
        let changed = send_back(
            &lines,
            &checked(3, true, 1),
            Some("the same failure 3 rounds running"),
        );
        assert!(
            changed.contains("Take a different approach this time"),
            "{changed}"
        );
    }
}
