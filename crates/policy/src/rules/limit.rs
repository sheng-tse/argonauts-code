//! Limit: release the run's reserve when the remaining tasks fit in it. Only you raise a
//! ceiling, so these conditions hold whatever `decide_with` says.

use argonauts_core::safety::policy::SpendFacts;
use argonauts_core::settings::approvals::LimitRules;

use super::RuleVerdict;

pub fn check(facts: Option<&SpendFacts>, rules: &LimitRules) -> RuleVerdict {
    if !rules.use_reserve {
        return RuleVerdict::ask_you("use_reserve is off, so only you release the reserve");
    }
    let Some(facts) = facts else {
        return RuleVerdict::ask_you(
            "the evidence doesn't show what the remaining tasks cost or how much reserve is left",
        );
    };
    let (needed, left) = (facts.remaining_tasks, facts.reserve_left);
    if needed.unit != left.unit {
        return RuleVerdict::ask_you(
            "the remaining tasks and the reserve are counted in different units",
        );
    }
    // Written so a NaN fails it, and a shortfall asks rather than stops the run.
    if needed.amount <= left.amount {
        RuleVerdict::approve(format!(
            "the remaining tasks need {:.2} and the reserve has {:.2} left",
            needed.amount, left.amount
        ))
    } else {
        RuleVerdict::ask_you(format!(
            "the remaining tasks need {:.2}, more than the {:.2} left in the reserve",
            needed.amount, left.amount
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::safety::policy::Verdict;
    use argonauts_core::settings::limit::{LimitAmount, LimitUnit};

    fn facts(needed: f64, left: f64) -> SpendFacts {
        SpendFacts {
            remaining_tasks: LimitAmount {
                unit: LimitUnit::Usd,
                amount: needed,
            },
            reserve_left: LimitAmount {
                unit: LimitUnit::Usd,
                amount: left,
            },
        }
    }

    fn verdict(facts: Option<&SpendFacts>, use_reserve: bool) -> Verdict {
        check(facts, &LimitRules { use_reserve }).verdict
    }

    #[test]
    fn the_reserve_is_released_when_the_remaining_tasks_fit() {
        assert_eq!(verdict(Some(&facts(2.0, 3.0)), true), Verdict::Approve);
        assert_eq!(verdict(Some(&facts(3.0, 3.0)), true), Verdict::Approve);
    }

    #[test]
    fn a_shortfall_a_missing_fact_or_use_reserve_off_asks() {
        assert_eq!(verdict(Some(&facts(4.0, 3.0)), true), Verdict::AskYou);
        assert_eq!(verdict(Some(&facts(f64::NAN, 3.0)), true), Verdict::AskYou);
        assert_eq!(verdict(None, true), Verdict::AskYou);
        assert_eq!(verdict(Some(&facts(1.0, 3.0)), false), Verdict::AskYou);
    }
}
