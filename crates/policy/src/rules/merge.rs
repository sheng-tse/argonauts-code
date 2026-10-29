//! Merge: checks pass, every reviewer approved and a reviewer of another provider than each
//! task's worker, or one you chose, gave the approval that stands, no protected path is touched,
//! the diff is small enough, and the suite passed on the latest main. Acceptance tests, test
//! tampering, and changed-line coverage count when they were measured, since not every repo can
//! measure them. Quality debt no strong enough model reviewed comes to you unless `allow_debt`
//! is set, since merging it anyway is yours.

use argonauts_core::policy::{EvidenceFacts, WithinProvider};
use argonauts_core::settings::approvals::MergeRules;

use super::{Conditions, RuleVerdict, percent};

pub fn check(facts: &EvidenceFacts, rules: &MergeRules) -> RuleVerdict {
    let mut c = Conditions::default();
    c.flag(
        facts.checks_passed,
        "every check passed",
        "a check failed or didn't run",
        "no check results",
    );
    // A task nobody could review fails here, named, rather than as a reviewer who didn't
    // approve.
    let unreviewed = (!facts.unreviewed.is_empty()).then(|| facts.unreviewed.join("; "));
    c.flag(
        facts.reviews_approved,
        "every reviewer approved",
        unreviewed.as_deref().unwrap_or("a reviewer didn't approve"),
        "no review results",
    );
    let within = within_provider(&facts.approved_within_provider);
    // With no approval to name, the unreviewed tasks are the reason, which failed above.
    let said_above = unreviewed.is_some()
        && within.is_none()
        && facts.reviews_approved == Some(false)
        && facts.reviewed_by_other_provider == Some(false);
    if !said_above {
        c.flag(
            facts.reviewed_by_other_provider,
            "each task's approval came from a reviewer of another provider than its worker, or \
             one you chose",
            within.as_deref().unwrap_or(
                "a task's approval came from a reviewer of its worker's own provider that you \
                 didn't choose",
            ),
            "no fact says which provider each task's reviewer comes from",
        );
    }
    c.measure_if_known(facts.acceptance_tests_valid, |valid| {
        if valid {
            Ok("every acceptance test fails on the stubs and passes on the run branch".to_owned())
        } else {
            Err("an acceptance test passes on the stubs or fails on the run branch".to_owned())
        }
    });
    c.none_of(
        &facts.untested_criteria,
        "every success criterion has an acceptance test",
        "success criteria without an acceptance test",
    );

    // Comparisons are written so a NaN fails them.
    let min_coverage = rules.min_test_coverage_of_changed_lines;
    c.measure_if_known(facts.changed_line_coverage, |coverage| {
        if coverage >= min_coverage {
            Ok(format!(
                "{} of changed lines are covered",
                percent(coverage)
            ))
        } else {
            Err(format!(
                "{} of changed lines are covered, under {}",
                percent(coverage),
                percent(min_coverage)
            ))
        }
    });

    c.measure_if_known(facts.tamper_flags, |flags| {
        if flags == 0 {
            Ok("no test-tampering flag is open".to_owned())
        } else {
            Err(format!("open test-tampering flags: {flags}"))
        }
    });
    c.none_of(
        &facts.protected_paths,
        "no protected path is touched",
        "protected paths touched",
    );

    let max_lines = rules.max_changed_lines;
    c.measure(
        facts.combined_diff_lines,
        "no combined diff size",
        |lines| {
            if lines <= max_lines {
                Ok(format!("the combined diff is {lines} lines"))
            } else {
                Err(format!(
                    "the combined diff is {lines} lines, over {max_lines}"
                ))
            }
        },
    );
    c.flag(
        facts.suite_on_latest_main,
        "the full suite passed on the latest main",
        "the full suite didn't pass on the latest main",
        "no suite result on the latest main",
    );
    match debt(facts.unreviewed_debt.as_deref(), rules) {
        Ok(held) => c.hold(held),
        Err(why) => c.ask_you(why),
    }
    c.verdict()
}

/// Who reviewed which task from its builder's own provider, as a reason that names each:
/// "Sonnet 5 on Claude Code, a fallback from its builder's own provider, reviewed task-001".
fn within_provider(tasks: &[WithinProvider]) -> Option<String> {
    let each: Vec<String> = tasks
        .iter()
        .map(|task| {
            let how = if task.fallback {
                "a fallback from its builder's own provider"
            } else {
                "picked from its builder's own provider"
            };
            format!("{}, {how}, reviewed {}", task.reviewer, task.task)
        })
        .collect();
    (!each.is_empty()).then(|| each.join("; "))
}

/// Why quality debt keeps auto from merging, given the tasks whose debt no strong enough model
/// reviewed. `None` when auto may merge: no debt, or `allow_debt` is set.
pub fn debt_blocks(unreviewed_debt: Option<&[String]>, rules: &MergeRules) -> Option<String> {
    debt(unreviewed_debt, rules).err()
}

/// What lets auto merge past quality debt, or why that call is yours. Debt nobody measured
/// counts as debt.
fn debt(unreviewed_debt: Option<&[String]>, rules: &MergeRules) -> Result<String, String> {
    match unreviewed_debt {
        Some([]) => {
            Ok("no task carries quality debt a strong enough model didn't review".to_owned())
        }
        _ if rules.allow_debt => Ok(
            "allow_debt lets auto merge quality debt no strong enough model reviewed".to_owned(),
        ),
        None => Err("no fact says whether a task carries quality debt".to_owned()),
        Some(tasks) => Err(format!(
            "quality debt no strong enough model reviewed, and merging it anyway is yours: {}",
            tasks.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::Verdict;

    fn passing() -> EvidenceFacts {
        EvidenceFacts {
            checks_passed: Some(true),
            reviews_approved: Some(true),
            reviewed_by_other_provider: Some(true),
            acceptance_tests_valid: Some(true),
            changed_line_coverage: Some(0.9),
            tamper_flags: Some(0),
            combined_diff_lines: Some(120),
            suite_on_latest_main: Some(true),
            unreviewed_debt: Some(Vec::new()),
            ..EvidenceFacts::default()
        }
    }

    fn verdict(facts: &EvidenceFacts) -> Verdict {
        check(facts, &MergeRules::default()).verdict
    }

    #[test]
    fn every_condition_holding_approves() {
        assert_eq!(verdict(&passing()), Verdict::Approve);
    }

    /// An approval code picked from the builder's own provider keeps the merge back, and the
    /// reason names who reviewed which task, and whether it was a fallback.
    #[test]
    fn an_approval_from_the_builders_own_provider_names_its_task() {
        let facts = EvidenceFacts {
            reviewed_by_other_provider: Some(false),
            approved_within_provider: vec![
                WithinProvider {
                    task: "task-001".to_owned(),
                    reviewer: "Sonnet 5 on Claude Code".to_owned(),
                    fallback: true,
                },
                WithinProvider {
                    task: "task-002".to_owned(),
                    reviewer: "Haiku 5.5 on Claude Code".to_owned(),
                    fallback: false,
                },
            ],
            ..passing()
        };
        let ruled = check(&facts, &MergeRules::default());
        assert_eq!(ruled.verdict, Verdict::Reject);
        assert_eq!(
            ruled.reasons,
            [
                "Sonnet 5 on Claude Code, a fallback from its builder's own provider, reviewed \
              task-001; Haiku 5.5 on Claude Code, picked from its builder's own provider, \
              reviewed task-002"
            ]
        );
        let held = check(&passing(), &MergeRules::default()).reasons;
        assert!(
            held.iter()
                .any(|reason| reason.ends_with("or one you chose")),
            "{held:?}"
        );
    }

    /// A task nobody but its builder could review keeps the merge back with one reason that
    /// names it, not as a reviewer who didn't approve.
    #[test]
    fn a_task_nobody_could_review_names_it_once() {
        let facts = EvidenceFacts {
            reviews_approved: Some(false),
            reviewed_by_other_provider: Some(false),
            unreviewed: vec!["Nobody but its builder is left to review task-001".to_owned()],
            ..passing()
        };
        let ruled = check(&facts, &MergeRules::default());
        assert_eq!(ruled.verdict, Verdict::Reject);
        assert_eq!(
            ruled.reasons,
            ["Nobody but its builder is left to review task-001"]
        );
        // Beside another task's approval from its builder's own provider, each says its own.
        let both = EvidenceFacts {
            approved_within_provider: vec![WithinProvider {
                task: "task-002".to_owned(),
                reviewer: "Haiku 5.5 on Claude Code".to_owned(),
                fallback: false,
            }],
            ..facts
        };
        assert_eq!(check(&both, &MergeRules::default()).reasons.len(), 2);
    }

    #[test]
    fn each_failed_condition_rejects() {
        let cases: Vec<(&str, EvidenceFacts)> = vec![
            (
                "checks",
                EvidenceFacts {
                    checks_passed: Some(false),
                    ..passing()
                },
            ),
            (
                "reviews",
                EvidenceFacts {
                    reviews_approved: Some(false),
                    ..passing()
                },
            ),
            (
                "same provider",
                EvidenceFacts {
                    reviewed_by_other_provider: Some(false),
                    ..passing()
                },
            ),
            (
                "acceptance",
                EvidenceFacts {
                    acceptance_tests_valid: Some(false),
                    ..passing()
                },
            ),
            (
                "criteria",
                EvidenceFacts {
                    untested_criteria: vec!["exports CSV".to_owned()],
                    ..passing()
                },
            ),
            (
                "coverage",
                EvidenceFacts {
                    changed_line_coverage: Some(0.5),
                    ..passing()
                },
            ),
            (
                "nan coverage",
                EvidenceFacts {
                    changed_line_coverage: Some(f64::NAN),
                    ..passing()
                },
            ),
            (
                "tamper",
                EvidenceFacts {
                    tamper_flags: Some(1),
                    ..passing()
                },
            ),
            (
                "protected",
                EvidenceFacts {
                    protected_paths: vec!["Cargo.lock".to_owned()],
                    ..passing()
                },
            ),
            (
                "diff",
                EvidenceFacts {
                    combined_diff_lines: Some(401),
                    ..passing()
                },
            ),
            (
                "suite",
                EvidenceFacts {
                    suite_on_latest_main: Some(false),
                    ..passing()
                },
            ),
        ];
        for (name, facts) in cases {
            assert_eq!(verdict(&facts), Verdict::Reject, "{name}");
        }
    }

    #[test]
    fn each_missing_fact_asks_you() {
        let cases: Vec<(&str, EvidenceFacts)> = vec![
            (
                "checks",
                EvidenceFacts {
                    checks_passed: None,
                    ..passing()
                },
            ),
            (
                "reviews",
                EvidenceFacts {
                    reviews_approved: None,
                    ..passing()
                },
            ),
            (
                "other provider",
                EvidenceFacts {
                    reviewed_by_other_provider: None,
                    ..passing()
                },
            ),
            (
                "diff",
                EvidenceFacts {
                    combined_diff_lines: None,
                    ..passing()
                },
            ),
            (
                "suite",
                EvidenceFacts {
                    suite_on_latest_main: None,
                    ..passing()
                },
            ),
            (
                "debt",
                EvidenceFacts {
                    unreviewed_debt: None,
                    ..passing()
                },
            ),
        ];
        for (name, facts) in cases {
            assert_eq!(verdict(&facts), Verdict::AskYou, "{name}");
        }
    }

    /// Not every repo can measure these, so auto merges without them, and a measured failure
    /// still rejects.
    #[test]
    fn unmeasured_tests_tampering_and_coverage_neither_hold_nor_block() {
        let unmeasured = EvidenceFacts {
            acceptance_tests_valid: None,
            changed_line_coverage: None,
            tamper_flags: None,
            ..passing()
        };
        let verdict = check(&unmeasured, &MergeRules::default());
        assert_eq!(verdict.verdict, Verdict::Approve);
        assert!(!verdict.reasons.iter().any(|r| r.contains("covered")));
    }

    #[test]
    fn a_rejection_names_every_failed_condition() {
        let facts = EvidenceFacts {
            checks_passed: Some(false),
            combined_diff_lines: Some(900),
            ..passing()
        };
        let verdict = check(&facts, &MergeRules::default());
        assert_eq!(verdict.reasons.len(), 2, "{:?}", verdict.reasons);
        assert!(verdict.reasons[1].contains("900"));
    }

    #[test]
    fn unreviewed_debt_asks_you_unless_allowed() {
        let lenient = MergeRules {
            allow_debt: true,
            ..MergeRules::default()
        };
        let indebted = EvidenceFacts {
            unreviewed_debt: Some(vec!["task-03".to_owned()]),
            ..passing()
        };
        let strict = check(&indebted, &MergeRules::default());
        assert_eq!(strict.verdict, Verdict::AskYou);
        assert!(
            strict.reasons[0].contains("task-03"),
            "{:?}",
            strict.reasons
        );
        assert_eq!(check(&indebted, &lenient).verdict, Verdict::Approve);

        // Debt nobody measured counts as debt, which `allow_debt` also lets through.
        let unmeasured = EvidenceFacts {
            unreviewed_debt: None,
            ..passing()
        };
        assert_eq!(check(&unmeasured, &lenient).verdict, Verdict::Approve);

        // A measured failure still sends the task back.
        let failed = EvidenceFacts {
            checks_passed: Some(false),
            ..indebted
        };
        assert_eq!(verdict(&failed), Verdict::Reject);
    }

    #[test]
    fn debt_blocks_unless_allowed() {
        let debt = ["task-03".to_owned()];
        let strict = MergeRules::default();
        let lenient = MergeRules {
            allow_debt: true,
            ..MergeRules::default()
        };
        assert!(debt_blocks(Some(&debt), &strict).is_some_and(|why| why.contains("task-03")));
        assert!(debt_blocks(None, &strict).is_some());
        assert_eq!(debt_blocks(Some(&debt), &lenient), None);
        assert_eq!(debt_blocks(None, &lenient), None);
        assert_eq!(debt_blocks(Some(&[]), &strict), None);
    }
}
