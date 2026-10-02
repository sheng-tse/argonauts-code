//! Goal changes follow `goal_changes`: ask, refuse, or record a clarification that leaves the
//! success criteria unchanged.

use argonauts_core::settings::approvals::GoalChanges;

use super::RuleVerdict;

pub fn check(goal_changes: GoalChanges) -> RuleVerdict {
    match goal_changes {
        GoalChanges::Ask => RuleVerdict::escalate("goal_changes is ask, so you decide"),
        GoalChanges::Deny => {
            RuleVerdict::reject("goal_changes is deny; the card goes on against the approved goal")
        }
        GoalChanges::AllowClarifications => RuleVerdict::escalate(
            "the evidence doesn't show whether the change leaves the success criteria unchanged",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::Verdict;

    #[test]
    fn goal_changes_follow_the_setting() {
        assert_eq!(check(GoalChanges::Ask).verdict, Verdict::Escalate);
        assert_eq!(check(GoalChanges::Deny).verdict, Verdict::Reject);
        assert_eq!(
            check(GoalChanges::AllowClarifications).verdict,
            Verdict::Escalate
        );
    }
}
