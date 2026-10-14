//! Learned workflow promotion: a change class you allow, and no other promotion of the learned
//! workflow within `min_days_between`, whatever `decide_with` says. Replay and holdout checks
//! always run first.

use argonauts_core::policy::EvidenceFacts;
use argonauts_core::settings::approvals::PromotionRules;
use time::{Duration, OffsetDateTime};

use super::RuleVerdict;

pub fn check(facts: &EvidenceFacts, rules: &PromotionRules, now: OffsetDateTime) -> RuleVerdict {
    let Some(promotion) = &facts.promotion else {
        return RuleVerdict::ask_you(
            "the evidence doesn't show the candidate's change class or when the workflow last \
             promoted",
        );
    };
    let class = promotion.change_class.as_str();
    if !rules.change_classes.iter().any(|allowed| allowed == class) {
        return RuleVerdict::ask_you(format!(
            "{class} isn't a change class your rules promote on their own"
        ));
    }
    let gap = Duration::days(i64::from(rules.min_days_between));
    match promotion.last_promoted {
        Some(last) if now - last < gap => RuleVerdict::ask_you(format!(
            "the workflow last promoted {} days ago, and your rules wait {} days between promotions",
            (now - last).whole_days().max(0),
            rules.min_days_between
        )),
        Some(last) => RuleVerdict::approve(format!(
            "{class} is a class you allow, and the workflow last promoted {} days ago",
            (now - last).whole_days()
        )),
        None => RuleVerdict::approve(format!(
            "{class} is a class you allow, and the workflow never promoted before"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::{PromotionFacts, Verdict};

    fn facts(class: &str, last_promoted: Option<OffsetDateTime>) -> EvidenceFacts {
        EvidenceFacts {
            promotion: Some(PromotionFacts {
                change_class: class.to_owned(),
                last_promoted,
            }),
            ..EvidenceFacts::default()
        }
    }

    #[test]
    fn promotion_rules_table() {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(400);
        let days_ago = |days: i64| Some(now - Duration::days(days));
        let rules = PromotionRules::default();
        let none = PromotionRules {
            change_classes: Vec::new(),
            ..PromotionRules::default()
        };
        let every_day = PromotionRules {
            min_days_between: 0,
            ..PromotionRules::default()
        };
        let cases = [
            (facts("checklist_item", None), &rules, Verdict::Approve),
            (
                facts("drafting_prompt", days_ago(14)),
                &rules,
                Verdict::Approve,
            ),
            (facts("task_split", days_ago(30)), &rules, Verdict::Approve),
            // Too soon after the last promotion, a long wait in the queue notwithstanding.
            (
                facts("checklist_item", days_ago(13)),
                &rules,
                Verdict::AskYou,
            ),
            (
                facts("checklist_item", days_ago(0)),
                &rules,
                Verdict::AskYou,
            ),
            (
                facts("checklist_item", days_ago(0)),
                &every_day,
                Verdict::Approve,
            ),
            // A class you don't allow, or none allowed at all.
            (facts("step_graph", None), &rules, Verdict::AskYou),
            (facts("crew_composition", None), &rules, Verdict::AskYou),
            (facts("checklist_item", None), &none, Verdict::AskYou),
            // No facts, no promotion.
            (EvidenceFacts::default(), &rules, Verdict::AskYou),
        ];
        for (facts, rules, expected) in cases {
            let verdict = check(&facts, rules, now);
            assert_eq!(
                verdict.verdict, expected,
                "{facts:?}: {:?}",
                verdict.reasons
            );
        }
    }
}
