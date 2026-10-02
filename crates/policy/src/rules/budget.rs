//! Budget: release the run's reserve when the remaining cards fit in it. Only you raise a
//! ceiling, so these conditions hold whatever `decide_with` says.

use argonauts_core::policy::BudgetFacts;
use argonauts_core::settings::approvals::BudgetRules;

use super::RuleVerdict;

pub fn check(facts: Option<&BudgetFacts>, rules: &BudgetRules) -> RuleVerdict {
    if !rules.use_reserve {
        return RuleVerdict::escalate("use_reserve is off, so only you release the reserve");
    }
    let Some(facts) = facts else {
        return RuleVerdict::escalate(
            "the evidence doesn't show what the remaining cards cost or how much reserve is left",
        );
    };
    let (needed, left) = (facts.remaining_cards, facts.reserve_left);
    if needed.unit != left.unit {
        return RuleVerdict::escalate(
            "the remaining cards and the reserve are counted in different units",
        );
    }
    // Written so a NaN fails it, and a shortfall asks rather than stops the run.
    if needed.amount <= left.amount {
        RuleVerdict::approve(format!(
            "the remaining cards need {:.2} and the reserve has {:.2} left",
            needed.amount, left.amount
        ))
    } else {
        RuleVerdict::escalate(format!(
            "the remaining cards need {:.2}, more than the {:.2} left in the reserve",
            needed.amount, left.amount
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::Verdict;
    use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};

    fn facts(needed: f64, left: f64) -> BudgetFacts {
        BudgetFacts {
            remaining_cards: BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: needed,
            },
            reserve_left: BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: left,
            },
        }
    }

    fn verdict(facts: Option<&BudgetFacts>, use_reserve: bool) -> Verdict {
        check(facts, &BudgetRules { use_reserve }).verdict
    }

    #[test]
    fn the_reserve_is_released_when_the_remaining_cards_fit() {
        assert_eq!(verdict(Some(&facts(2.0, 3.0)), true), Verdict::Approve);
        assert_eq!(verdict(Some(&facts(3.0, 3.0)), true), Verdict::Approve);
    }

    #[test]
    fn a_shortfall_a_missing_fact_or_use_reserve_off_asks() {
        assert_eq!(verdict(Some(&facts(4.0, 3.0)), true), Verdict::Escalate);
        assert_eq!(
            verdict(Some(&facts(f64::NAN, 3.0)), true),
            Verdict::Escalate
        );
        assert_eq!(verdict(None, true), Verdict::Escalate);
        assert_eq!(verdict(Some(&facts(1.0, 3.0)), false), Verdict::Escalate);
    }
}
