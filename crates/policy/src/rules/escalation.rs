//! Escalation: one more attempt, by a fallback agent from a family that hasn't tried the card,
//! within the reserve and `max_extra_attempts`, when the role's handoff mode is auto. These
//! conditions hold whatever `decide_with` says.

use argonauts_core::policy::EscalationFacts;
use argonauts_core::settings::approvals::EscalationRules;
use argonauts_core::settings::handoff::HandoffMode;

use super::{Conditions, RuleVerdict};

pub fn check(facts: Option<&EscalationFacts>, rules: &EscalationRules) -> RuleVerdict {
    if rules.max_extra_attempts == 0 {
        return RuleVerdict::escalate("max_extra_attempts is 0, so auto grants no extra attempt");
    }
    let Some(facts) = facts else {
        return RuleVerdict::escalate(
            "the evidence doesn't show the role's handoff mode, which families tried the card, \
             its extra attempts so far, or whether one more fits the reserve",
        );
    };
    // Nothing cancels a card automatically, so every condition that fails asks you rather than
    // rejects.
    let mut c = Conditions::default();
    if facts.handoff_mode == HandoffMode::Auto {
        c.hold("the role's handoff mode is auto");
    } else {
        c.escalate("the role's handoff mode isn't auto, so who takes the card is yours");
    }
    match &facts.next_family {
        Some(next) if facts.families_tried.contains(next) => c.escalate(format!(
            "the next fallback is of the {next} family, which already tried the card"
        )),
        Some(next) => c.hold(format!(
            "the next attempt goes to the {next} family, which hasn't tried the card"
        )),
        None => c.escalate("no fallback from a family that hasn't tried the card is left"),
    }
    let (made, most) = (facts.extra_attempts, rules.max_extra_attempts);
    if made < most {
        c.hold(format!("{made} of {most} extra attempts used"));
    } else {
        c.escalate(format!("all {most} extra attempts auto may grant are used"));
    }
    if facts.fits_reserve {
        c.hold("one more attempt fits what's left of the reserve");
    } else {
        c.escalate("one more attempt doesn't fit what's left of the reserve");
    }
    c.verdict()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::Verdict;

    fn granting() -> EscalationFacts {
        EscalationFacts {
            handoff_mode: HandoffMode::Auto,
            families_tried: vec!["claude".to_owned()],
            next_family: Some("gpt".to_owned()),
            extra_attempts: 0,
            fits_reserve: true,
        }
    }

    fn verdict(facts: Option<&EscalationFacts>, max_extra_attempts: u32) -> Verdict {
        check(facts, &EscalationRules { max_extra_attempts }).verdict
    }

    #[test]
    fn a_fresh_family_within_the_reserve_and_the_attempts_gets_one_more() {
        assert_eq!(verdict(Some(&granting()), 1), Verdict::Approve);
    }

    #[test]
    fn every_failed_condition_or_missing_fact_asks() {
        let cases = [
            (
                "ask mode",
                EscalationFacts {
                    handoff_mode: HandoffMode::Ask,
                    ..granting()
                },
            ),
            (
                "family tried",
                EscalationFacts {
                    next_family: Some("claude".to_owned()),
                    ..granting()
                },
            ),
            (
                "no fallback",
                EscalationFacts {
                    next_family: None,
                    ..granting()
                },
            ),
            (
                "attempts used",
                EscalationFacts {
                    extra_attempts: 1,
                    ..granting()
                },
            ),
            (
                "over the reserve",
                EscalationFacts {
                    fits_reserve: false,
                    ..granting()
                },
            ),
        ];
        for (name, facts) in cases {
            assert_eq!(verdict(Some(&facts), 1), Verdict::Escalate, "{name}");
        }
        assert_eq!(verdict(None, 1), Verdict::Escalate);
        assert_eq!(verdict(Some(&granting()), 0), Verdict::Escalate);
    }
}
