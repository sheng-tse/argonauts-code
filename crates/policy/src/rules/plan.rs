//! Plan: close judges from other providers, a clear winner, settled disputes, no flagged plan,
//! an estimate within the limit, allowlisted dependencies, and no protected owned paths. They hold
//! whoever answers the crew's questions: a plan nobody drafted or judged, such as a workflow
//! the main agent picked, carries an uncontested plan's facts instead.

use argonauts_core::safety::policy::EvidenceFacts;
use argonauts_core::settings::approvals::{HardLimitSettings, PlanRules};

use super::{Conditions, RuleVerdict, usd};

/// `spend_per_run` is the run's spend limit, when it has one.
pub fn check(
    facts: &EvidenceFacts,
    rules: &PlanRules,
    limits: &HardLimitSettings,
    spend_per_run: Option<f64>,
) -> RuleVerdict {
    let mut c = Conditions::default();
    judging(&mut c, facts, rules);
    binding(&mut c, facts, limits, spend_per_run);
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
            Ok(format!("the winner is ahead by {margin}"))
        } else {
            Err(format!(
                "the winner is ahead by {margin}, under {min_margin}"
            ))
        }
    });

    c.flag(
        facts.judges_other_provider,
        "every counted score came from another provider",
        "a counted score came from the plan author's provider",
        "no fact says which providers judged",
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
    spend_per_run: Option<f64>,
) {
    let flagged = |flagged: u32| {
        if flagged == 0 {
            Ok("no plan was flagged".to_owned())
        } else {
            Err(format!("{flagged} flagged for text addressed to judges"))
        }
    };
    let fits = |estimate: f64| fits_limit(estimate, facts.spent.cost_usd, spend_per_run);
    c.measure(facts.flagged_plans, "no count of flagged plans", flagged);
    if facts.spent.cost_partial && spend_per_run.is_some() {
        // The spend is only a lower bound, so an estimate that seems to fit may not.
        c.ask_you("the run's spend isn't fully priced, so what's left of its limit is unknown");
    }
    // Tasks have limits only when you set them, so a plan often has no estimate. It isn't
    // held back for one: the meter holds the run to its limit either way.
    c.measure_if_known(facts.estimate_usd, fits);

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
        "no task owns a protected path",
        "tasks own protected paths",
    );
}

/// The estimate has to fit what's left of the run's limit after what it already spent.
fn fits_limit(estimate: f64, spent: f64, spend_per_run: Option<f64>) -> Result<String, String> {
    let Some(ceiling) = spend_per_run else {
        return Ok(format!(
            "the estimate is {}, and the run has no limit",
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
            "the estimate {} is over the {} left of the run's limit",
            usd(estimate),
            usd(left)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::safety::policy::Verdict;

    fn passing() -> EvidenceFacts {
        EvidenceFacts {
            max_score_spread: Some(0.5),
            winner_margin: Some(2.0),
            judges_other_provider: Some(true),
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
                "provider",
                EvidenceFacts {
                    judges_other_provider: Some(false),
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
    fn each_missing_fact_asks_you() {
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
                "provider",
                EvidenceFacts {
                    judges_other_provider: None,
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
        ];
        for (name, facts) in cases {
            assert_eq!(verdict(&facts), Verdict::AskYou, "{name}");
        }
    }

    /// Tasks have limits only when you set them, so a plan often has no estimate.
    #[test]
    fn a_plan_with_no_estimate_is_not_held_back_for_one() {
        let facts = EvidenceFacts {
            estimate_usd: None,
            ..passing()
        };
        assert_eq!(verdict(&facts), Verdict::Approve);
    }

    #[test]
    fn the_estimate_fits_what_is_left_of_the_limit() {
        let mut facts = passing();
        facts.spent.cost_usd = 15.0;
        assert_eq!(verdict(&facts), Verdict::Reject);

        facts.spent.cost_partial = true;
        assert_eq!(verdict(&facts), Verdict::Reject);

        facts.spent.cost_usd = 2.0;
        assert_eq!(verdict(&facts), Verdict::AskYou);

        let no_limit = check(&passing(), &PlanRules::default(), &limits(), None);
        assert_eq!(no_limit.verdict, Verdict::Approve);
    }
}
