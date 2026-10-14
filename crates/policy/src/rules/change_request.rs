//! Change requests reach an approval when no other task's worker can decide them: nobody owns
//! what they touch, the workers tied, or the change can't be undone. A reversible one that
//! touches no protected path may be decided here; an irreversible one is yours, and the hard
//! limits hold it to `limits.irreversible_allowlist` first. A request that changes the goal or
//! moves a task into a protected path never gets here: it goes to the goal or the plan.

use argonauts_core::policy::EvidenceFacts;

use super::{Conditions, RuleVerdict};

pub fn check(facts: &EvidenceFacts) -> RuleVerdict {
    let mut c = Conditions::default();
    match facts.irreversible_actions.as_deref() {
        Some([]) => c.hold("nothing it changes ends in an outside action that can't be undone"),
        Some(actions) => c.ask_you(format!(
            "it ends in outside actions that can't be undone, which are yours: {}",
            actions.join(", ")
        )),
        None => c.ask_you("no fact says whether it ends in an outside action"),
    }
    // A protected path sends the run back to the plan before it gets here, so one in the
    // evidence means something moved, and that's yours.
    if facts.protected_paths.is_empty() {
        c.hold("it touches no protected path");
    } else {
        c.ask_you(format!(
            "it touches protected paths: {}",
            facts.protected_paths.join(", ")
        ));
    }
    c.verdict()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::policy::Verdict;

    fn facts(actions: Option<&[&str]>, protected: &[&str]) -> EvidenceFacts {
        EvidenceFacts {
            irreversible_actions: actions
                .map(|actions| actions.iter().map(|a| (*a).to_owned()).collect()),
            protected_paths: protected.iter().map(|p| (*p).to_owned()).collect(),
            ..EvidenceFacts::default()
        }
    }

    #[test]
    fn a_reversible_request_away_from_protected_paths_passes() {
        assert_eq!(check(&facts(Some(&[]), &[])).verdict, Verdict::Approve);
    }

    #[test]
    fn an_irreversible_or_unmeasured_or_protected_request_asks() {
        let irreversible = check(&facts(Some(&["deploy"]), &[]));
        assert_eq!(irreversible.verdict, Verdict::AskYou);
        assert!(irreversible.reasons[0].contains("deploy"));
        assert_eq!(check(&facts(None, &[])).verdict, Verdict::AskYou);
        assert_eq!(
            check(&facts(Some(&[]), &["Cargo.lock"])).verdict,
            Verdict::AskYou
        );
    }
}
