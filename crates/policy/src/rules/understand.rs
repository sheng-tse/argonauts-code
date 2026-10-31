//! Understand: approve only when no question is open. Scope, security, data, and cost questions
//! always reach you, so an open one never auto-approves, whatever `decide_with` says.

use argonauts_core::safety::policy::EvidenceFacts;

use super::RuleVerdict;

pub fn check(facts: &EvidenceFacts) -> RuleVerdict {
    // An open question comes to you rather than rejects: `otherwise` could send the goal back
    // or stop the run, and neither answers a question that belongs to you.
    match facts.open_questions {
        Some(0) => RuleVerdict::approve("no question is open"),
        Some(1) => RuleVerdict::ask_you("1 question is open, and it's yours to answer"),
        Some(n) => RuleVerdict::ask_you(format!(
            "{n} questions are open, and they're yours to answer"
        )),
        None => RuleVerdict::ask_you("no count of open questions"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::safety::policy::Verdict;

    fn with(open_questions: Option<u32>) -> Verdict {
        let facts = EvidenceFacts {
            open_questions,
            ..EvidenceFacts::default()
        };
        check(&facts).verdict
    }

    #[test]
    fn only_no_open_question_approves() {
        assert_eq!(with(Some(0)), Verdict::Approve);
        assert_eq!(with(Some(1)), Verdict::AskYou);
        assert_eq!(with(Some(3)), Verdict::AskYou);
        assert_eq!(with(None), Verdict::AskYou);
    }
}
