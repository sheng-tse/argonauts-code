//! Merge: gates pass, every reviewer approved and a reviewer of another company than each
//! card's owner gave the approval that stands, no protected path is touched, the diff is small
//! enough, and the suite passed on the latest main. Acceptance tests, test tampering, and
//! changed-line coverage count when they were measured, since not every repo can measure them.
//! Quality debt no strong enough model reviewed escalates unless `allow_debt` is set, since
//! merging it anyway is yours.

use argonauts_core::policy::EvidenceFacts;
use argonauts_core::settings::approvals::MergeRules;

use super::{Conditions, RuleVerdict, percent};

pub fn check(facts: &EvidenceFacts, rules: &MergeRules) -> RuleVerdict {
    let mut c = Conditions::default();
    c.flag(
        facts.gates_passed,
        "every check passed",
        "a check failed or didn't run",
        "no check results",
    );
    c.flag(
        facts.reviews_approved,
        "every reviewer approved",
        "a reviewer didn't approve",
        "no review results",
    );
    c.flag(
        facts.reviewed_by_other_provider,
        "each task's approval came from a reviewer of another provider than its worker",
        "a task's approval came from a reviewer of its worker's own provider",
        "no fact says which provider each task's reviewer comes from",
    );
    c.measure_if_known(facts.acceptance_tests_valid, |valid| {
        if valid {
            Ok(
                "every acceptance test fails on the stubs and passes on the integration branch"
                    .to_owned(),
            )
        } else {
            Err(
                "an acceptance test passes on the stubs or fails on the integration branch"
                    .to_owned(),
            )
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
        Err(why) => c.escalate(why),
    }
    c.verdict()
}

/// Why quality debt keeps auto from merging, given the cards whose debt no strong enough model
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
        Some(cards) => Err(format!(
            "quality debt no strong enough model reviewed, and merging it anyway is yours: {}",
            cards.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::Verdict;

    fn passing() -> EvidenceFacts {
        EvidenceFacts {
            gates_passed: Some(true),
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

    #[test]
    fn each_failed_condition_rejects() {
        let cases: Vec<(&str, EvidenceFacts)> = vec![
            (
                "gates",
                EvidenceFacts {
                    gates_passed: Some(false),
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
    fn each_missing_fact_escalates() {
        let cases: Vec<(&str, EvidenceFacts)> = vec![
            (
                "gates",
                EvidenceFacts {
                    gates_passed: None,
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
            assert_eq!(verdict(&facts), Verdict::Escalate, "{name}");
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
            gates_passed: Some(false),
            combined_diff_lines: Some(900),
            ..passing()
        };
        let verdict = check(&facts, &MergeRules::default());
        assert_eq!(verdict.reasons.len(), 2, "{:?}", verdict.reasons);
        assert!(verdict.reasons[1].contains("900"));
    }

    #[test]
    fn unreviewed_debt_escalates_unless_allowed() {
        let lenient = MergeRules {
            allow_debt: true,
            ..MergeRules::default()
        };
        let indebted = EvidenceFacts {
            unreviewed_debt: Some(vec!["task-03".to_owned()]),
            ..passing()
        };
        let strict = check(&indebted, &MergeRules::default());
        assert_eq!(strict.verdict, Verdict::Escalate);
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

        // A measured failure still sends the card back.
        let failed = EvidenceFacts {
            gates_passed: Some(false),
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
