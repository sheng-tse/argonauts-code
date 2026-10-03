//! Plan: close judges from other families, a clear winner, settled disputes, no flagged plan,
//! an estimate within budget, allowlisted dependencies, and no protected owned paths. They hold
//! whoever answers the crew's questions: a plan nobody drafted or judged, such as a workflow
//! the lead picked, carries an uncontested plan's facts instead.

use argonauts_core::policy::EvidenceFacts;
use argonauts_core::settings::approvals::{HardLimitSettings, PlanRules};

use super::{Conditions, RuleVerdict, usd};

/// `run_usd` is the run's budget ceiling, when it has one.
pub fn check(
    facts: &EvidenceFacts,
    rules: &PlanRules,
    limits: &HardLimitSettings,
    run_usd: Option<f64>,
) -> RuleVerdict {
    let mut c = Conditions::default();
    judging(&mut c, facts, rules);
    binding(&mut c, facts, limits, run_usd);
    c.verdict()
}

fn judging(c: &mut Conditions, facts: &EvidenceFacts, rules: &PlanRules) {
    // Comparisons are written so a NaN fails them.
    let max_spread = rules.max_score_spread;
    c.measure(facts.max_score_spread, "no judge score spread", |spread| {
        if spread <= max_spread {
            Ok(format!("judges' scores are within {max_spread} points"))
        } else {
            Err(format!(
                "a judge's two scores are {spread} points apart, over {max_spread}"
            ))
        }
    });

    let min_margin = rules.min_winner_margin;
    c.measure(facts.winner_margin, "no winner margin", |margin| {
        if margin >= min_margin {
            Ok(format!("the winner leads by {margin}"))
        } else {
            Err(format!("the winner leads by {margin}, under {min_margin}"))
        }
    });

    c.flag(
        facts.judges_other_family,
        "every counted score came from another family",
        "a counted score came from the plan author's family",
        "no fact says which families judged",
    );
    c.flag(
        facts.disputes_settled,
        "every dispute was settled by its experiment",
        "a dispute with a proposed experiment wasn't settled",
        "no fact says whether disputes were settled",
    );
}

fn binding(
    c: &mut Conditions,
    facts: &EvidenceFacts,
    limits: &HardLimitSettings,
    run_usd: Option<f64>,
) {
    let flagged = |flagged: u32| {
        if flagged == 0 {
            Ok("no plan was flagged".to_owned())
        } else {
            Err(format!("{flagged} flagged for text addressed to judges"))
        }
    };
    let fits = |estimate: f64| fits_budget(estimate, facts.spent.cost_usd, run_usd);
    c.measure(facts.flagged_plans, "no count of flagged plans", flagged);
    if facts.spent.cost_partial && run_usd.is_some() {
        // The spend is only a lower bound, so an estimate that seems to fit may not.
        c.escalate("the run's spend isn't fully priced, so what's left of its budget is unknown");
    }
    c.measure(facts.estimate_usd, "no cost estimate", fits);

    let unlisted: Vec<String> = facts
        .new_dependencies
        .iter()
        .filter(|dep| !limits.dependency_allowlist.contains(dep))
        .cloned()
        .collect();
    c.none_of(
        &unlisted,
        "every new dependency is on your allowlist",
        "new dependencies not on your allowlist",
    );
    c.none_of(
        &facts.protected_paths,
        "no card owns a protected path",
        "cards own protected paths",
    );
}

/// The estimate has to fit what's left of the run's budget after what it already spent.
fn fits_budget(estimate: f64, spent: f64, run_usd: Option<f64>) -> Result<String, String> {
    let Some(ceiling) = run_usd else {
        return Ok(format!(
            "the estimate is {}, and the run has no budget",
            usd(estimate)
        ));
    };
    let left = ceiling - spent;
    if estimate <= left {
        Ok(format!(
            "the estimate {} fits the {} left",
            usd(estimate),
            usd(left)
        ))
    } else {
        Err(format!(
            "the estimate {} is over the {} left of the run's budget",
            usd(estimate),
            usd(left)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::Verdict;

    fn passing() -> EvidenceFacts {
        EvidenceFacts {
            max_score_spread: Some(0.5),
            winner_margin: Some(2.0),
            judges_other_family: Some(true),
            disputes_settled: Some(true),
            flagged_plans: Some(0),
            estimate_usd: Some(8.0),
            new_dependencies: vec!["serde".to_owned()],
            ..EvidenceFacts::default()
        }
    }

    fn limits() -> HardLimitSettings {
        HardLimitSettings {
            dependency_allowlist: vec!["serde".to_owned()],
            ..HardLimitSettings::default()
        }
    }

    fn verdict(facts: &EvidenceFacts) -> Verdict {
        check(facts, &PlanRules::default(), &limits(), Some(20.0)).verdict
    }

    #[test]
    fn every_condition_holding_approves() {
        assert_eq!(verdict(&passing()), Verdict::Approve);
    }

    #[test]
    fn each_failed_condition_rejects() {
        let cases: Vec<(&str, EvidenceFacts)> = vec![
            (
                "spread",
                EvidenceFacts {
                    max_score_spread: Some(1.5),
                    ..passing()
                },
            ),
            (
                "nan spread",
                EvidenceFacts {
                    max_score_spread: Some(f64::NAN),
                    ..passing()
                },
            ),
            (
                "margin",
                EvidenceFacts {
                    winner_margin: Some(1.0),
                    ..passing()
                },
            ),
            (
                "family",
                EvidenceFacts {
                    judges_other_family: Some(false),
                    ..passing()
                },
            ),
            (
                "disputes",
                EvidenceFacts {
                    disputes_settled: Some(false),
                    ..passing()
                },
            ),
            (
                "flagged",
                EvidenceFacts {
                    flagged_plans: Some(1),
                    ..passing()
                },
            ),
            (
                "estimate",
                EvidenceFacts {
                    estimate_usd: Some(25.0),
                    ..passing()
                },
            ),
            (
                "dependency",
                EvidenceFacts {
                    new_dependencies: vec!["leftpad".to_owned()],
                    ..passing()
                },
            ),
            (
                "protected",
                EvidenceFacts {
                    protected_paths: vec![".github/ci.yml".to_owned()],
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
                "spread",
                EvidenceFacts {
                    max_score_spread: None,
                    ..passing()
                },
            ),
            (
                "margin",
                EvidenceFacts {
                    winner_margin: None,
                    ..passing()
                },
            ),
            (
                "family",
                EvidenceFacts {
                    judges_other_family: None,
                    ..passing()
                },
            ),
            (
                "disputes",
                EvidenceFacts {
                    disputes_settled: None,
                    ..passing()
                },
            ),
            (
                "flagged",
                EvidenceFacts {
                    flagged_plans: None,
                    ..passing()
                },
            ),
            (
                "estimate",
                EvidenceFacts {
                    estimate_usd: None,
                    ..passing()
                },
            ),
        ];
        for (name, facts) in cases {
            assert_eq!(verdict(&facts), Verdict::Escalate, "{name}");
        }
    }

    #[test]
    fn the_estimate_fits_what_is_left_of_the_budget() {
        let mut facts = passing();
        facts.spent.cost_usd = 15.0;
        assert_eq!(verdict(&facts), Verdict::Reject);

        facts.spent.cost_partial = true;
        assert_eq!(verdict(&facts), Verdict::Reject);

        facts.spent.cost_usd = 2.0;
        assert_eq!(verdict(&facts), Verdict::Escalate);

        let no_budget = check(&passing(), &PlanRules::default(), &limits(), None);
        assert_eq!(no_budget.verdict, Verdict::Approve);
    }
}
