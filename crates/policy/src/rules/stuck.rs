//! A stuck task: one more try, by a fallback agent from a provider that hasn't tried the task,
//! within the reserve and `max_extra_tries`, when the role's fallback mode is auto. These
//! conditions hold whatever `decide_with` says.

use argonauts_core::policy::StuckFacts;
use argonauts_core::settings::approvals::StuckRules;
use argonauts_core::settings::fallback::FallbackMode;

use super::{Conditions, RuleVerdict};

pub fn check(facts: Option<&StuckFacts>, rules: &StuckRules) -> RuleVerdict {
    if rules.max_extra_tries == 0 {
        return RuleVerdict::ask_you("max_extra_tries is 0, so auto grants no extra try");
    }
    let Some(facts) = facts else {
        return RuleVerdict::ask_you(
            "the evidence doesn't show the role's fallback mode, which providers tried the task, \
             its extra tries so far, or whether one more fits the reserve",
        );
    };
    // Nothing cancels a question automatically, so every condition that fails asks you rather than
    // rejects.
    let mut c = Conditions::default();
    if facts.fallback_mode == FallbackMode::Auto {
        c.hold("the role's fallback mode is auto");
    } else {
        c.ask_you("the role's fallback mode isn't auto, so who takes the task is yours");
    }
    match &facts.next_provider {
        Some(next) if facts.providers_tried.contains(next) => c.ask_you(format!(
            "the next fallback is of the {next} provider, which already tried the task"
        )),
        Some(next) => c.hold(format!(
            "the next try goes to the {next} provider, which hasn't tried the task"
        )),
        None => c.ask_you("no fallback from a provider that hasn't tried the task is left"),
    }
    let (made, most) = (facts.extra_tries, rules.max_extra_tries);
    if made < most {
        c.hold(format!("{made} of {most} extra tries used"));
    } else {
        c.ask_you(format!("all {most} extra tries auto may grant are used"));
    }
    match facts.fits_reserve {
        Some(true) => c.hold("one more try fits what's left of the reserve"),
        Some(false) => c.ask_you("one more try doesn't fit what's left of the reserve"),
        None => c.ask_you(
            "whether one more try fits what's left of the reserve can't be told, since the \
             task has no limit of its own",
        ),
    }
    c.verdict()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::Verdict;

    fn granting() -> StuckFacts {
        StuckFacts {
            fallback_mode: FallbackMode::Auto,
            providers_tried: vec!["anthropic".to_owned()],
            next_provider: Some("openai".to_owned()),
            extra_tries: 0,
            fits_reserve: Some(true),
        }
    }

    fn verdict(facts: Option<&StuckFacts>, max_extra_tries: u32) -> Verdict {
        check(facts, &StuckRules { max_extra_tries }).verdict
    }

    #[test]
    fn a_fresh_provider_within_the_reserve_and_the_tries_gets_one_more() {
        assert_eq!(verdict(Some(&granting()), 1), Verdict::Approve);
    }

    #[test]
    fn every_failed_condition_or_missing_fact_asks() {
        let cases = [
            (
                "ask mode",
                StuckFacts {
                    fallback_mode: FallbackMode::Ask,
                    ..granting()
                },
            ),
            (
                "provider tried",
                StuckFacts {
                    next_provider: Some("anthropic".to_owned()),
                    ..granting()
                },
            ),
            (
                "no fallback",
                StuckFacts {
                    next_provider: None,
                    ..granting()
                },
            ),
            (
                "tries used",
                StuckFacts {
                    extra_tries: 1,
                    ..granting()
                },
            ),
            (
                "over the reserve",
                StuckFacts {
                    fits_reserve: Some(false),
                    ..granting()
                },
            ),
            (
                "a try of unknown cost",
                StuckFacts {
                    fits_reserve: None,
                    ..granting()
                },
            ),
        ];
        for (name, facts) in cases {
            assert_eq!(verdict(Some(&facts), 1), Verdict::AskYou, "{name}");
        }
        assert_eq!(verdict(None, 1), Verdict::AskYou);
        assert_eq!(verdict(Some(&granting()), 0), Verdict::AskYou);
    }
}
