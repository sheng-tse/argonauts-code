//! Goal changes follow `goal_changes`: ask, refuse, or record a clarification that leaves the
//! success criteria unchanged.

use argonauts_core::settings::approvals::GoalChanges;

use super::RuleVerdict;

pub fn check(goal_changes: GoalChanges) -> RuleVerdict {
    match goal_changes {
        GoalChanges::Ask => RuleVerdict::ask_you("goal_changes is ask, so you decide"),
        GoalChanges::Deny => {
            RuleVerdict::reject("goal_changes is deny; the task goes on against the approved goal")
        }
        GoalChanges::AllowClarifications => RuleVerdict::ask_you(
            "the evidence doesn't show whether the change leaves the success criteria unchanged",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::safety::policy::Verdict;

    #[test]
    fn goal_changes_follow_the_setting() {
        assert_eq!(check(GoalChanges::Ask).verdict, Verdict::AskYou);
        assert_eq!(check(GoalChanges::Deny).verdict, Verdict::Reject);
        assert_eq!(
            check(GoalChanges::AllowClarifications).verdict,
            Verdict::AskYou
        );
    }
}
