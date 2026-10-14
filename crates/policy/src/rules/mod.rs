//! Your auto-approval rules, one module per decision point. They read only the facts code
//! measured, never agent text, and any fact they need but don't have means it comes to you.

pub mod change_request;
pub mod goal_change;
pub mod limit;
pub mod merge;
pub mod plan;
pub mod promotion;
pub mod stuck;
pub mod understand;

use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::policy::{DecisionPoint, EvidencePacket, Verdict};
use argonauts_core::settings::Settings;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq)]
pub struct RuleVerdict {
    pub verdict: Verdict,
    /// Every condition that failed, or what the approval rests on.
    pub reasons: Vec<String>,
}

impl RuleVerdict {
    pub fn approve(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Approve,
            reasons: vec![reason.into()],
        }
    }

    pub fn ask_you(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::AskYou,
            reasons: vec![reason.into()],
        }
    }

    pub fn reject(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Reject,
            reasons: vec![reason.into()],
        }
    }
}

/// Your rules' verdict on one decision point at `now`. A kind without rules comes to you.
pub fn check(
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Settings,
    now: OffsetDateTime,
) -> RuleVerdict {
    let facts = &evidence.facts;
    let approvals = &settings.approvals;
    match point.kind {
        ApprovalKind::Understand => understand::check(facts),
        ApprovalKind::Plan => plan::check(
            facts,
            &approvals.rules.plan,
            &settings.limits,
            settings.limits.spend.spend_per_run,
        ),
        ApprovalKind::Merge => merge::check(facts, &approvals.rules.merge),
        ApprovalKind::Stuck => stuck::check(facts.stuck.as_ref(), &approvals.rules.stuck),
        ApprovalKind::Limit => limit::check(facts.spend.as_ref(), &approvals.rules.limit),
        ApprovalKind::ChangeRequest => change_request::check(facts),
        ApprovalKind::GoalChange => goal_change::check(approvals.goal_changes),
        ApprovalKind::WorkflowPromotion => {
            promotion::check(facts, &approvals.rules.workflow_promotion, now)
        }
        ApprovalKind::Fallback
        | ApprovalKind::AgentDown
        | ApprovalKind::TakeOverReturn
        | ApprovalKind::Question
        | ApprovalKind::OriginalReturned => {
            RuleVerdict::ask_you("no rules decide this kind of approval")
        }
    }
}

/// The outcome of every condition of one rule. Any failed condition rejects, any fact that
/// wasn't measured or call that is yours comes to you, and only a clean sweep approves.
#[derive(Debug, Default)]
pub(crate) struct Conditions {
    held: Vec<String>,
    failed: Vec<String>,
    needs_you: Vec<String>,
}

impl Conditions {
    /// `judge` returns what held, or what failed.
    pub fn measure<T>(
        &mut self,
        fact: Option<T>,
        missing: &str,
        judge: impl FnOnce(T) -> Result<String, String>,
    ) {
        match fact.map(judge) {
            Some(Ok(held)) => self.held.push(held),
            Some(Err(failed)) => self.failed.push(failed),
            None => self.needs_you.push(missing.to_owned()),
        }
    }

    /// Like [`Conditions::measure`] for a fact some repos can't measure: unmeasured, it
    /// neither holds nor holds the approval back.
    pub fn measure_if_known<T>(
        &mut self,
        fact: Option<T>,
        judge: impl FnOnce(T) -> Result<String, String>,
    ) {
        match fact.map(judge) {
            Some(Ok(held)) => self.held.push(held),
            Some(Err(failed)) => self.failed.push(failed),
            None => {}
        }
    }

    pub fn flag(&mut self, fact: Option<bool>, held: &str, failed: &str, missing: &str) {
        self.measure(fact, missing, |ok| {
            if ok {
                Ok(held.to_owned())
            } else {
                Err(failed.to_owned())
            }
        });
    }

    pub fn hold(&mut self, what: impl Into<String>) {
        self.held.push(what.into());
    }

    /// Holds the approval back without failing: a fact wasn't measured, or the call is yours.
    pub fn ask_you(&mut self, why: impl Into<String>) {
        self.needs_you.push(why.into());
    }

    /// Holds when `found` is empty; otherwise fails naming each item.
    pub fn none_of(&mut self, found: &[String], held: &str, failed: &str) {
        if found.is_empty() {
            self.held.push(held.to_owned());
        } else {
            self.failed.push(format!("{failed}: {}", found.join(", ")));
        }
    }

    pub fn verdict(self) -> RuleVerdict {
        let Self {
            held,
            mut failed,
            needs_you,
        } = self;
        if !failed.is_empty() {
            failed.extend(needs_you);
            return RuleVerdict {
                verdict: Verdict::Reject,
                reasons: failed,
            };
        }
        if !needs_you.is_empty() {
            return RuleVerdict {
                verdict: Verdict::AskYou,
                reasons: needs_you,
            };
        }
        RuleVerdict {
            verdict: Verdict::Approve,
            reasons: held,
        }
    }
}

/// A share such as 0.8, as a percentage for reasons you read.
pub(crate) fn percent(share: f64) -> String {
    format!("{:.0}%", share * 100.0)
}

/// `$1.50`, for reasons you read.
pub(crate) fn usd(amount: f64) -> String {
    format!("${amount:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_condition_rejects_over_a_missing_fact() {
        let mut c = Conditions::default();
        c.flag(
            Some(false),
            "checks passed",
            "a check failed",
            "no check result",
        );
        c.flag(
            None,
            "reviews approved",
            "a review failed",
            "no review result",
        );
        let verdict = c.verdict();
        assert_eq!(verdict.verdict, Verdict::Reject);
        assert_eq!(verdict.reasons, ["a check failed", "no review result"]);
    }

    #[test]
    fn a_missing_fact_asks_you_and_a_clean_sweep_approves() {
        let mut c = Conditions::default();
        c.flag(None, "checks passed", "a check failed", "no check result");
        assert_eq!(c.verdict(), RuleVerdict::ask_you("no check result"));

        let mut c = Conditions::default();
        c.flag(
            Some(true),
            "checks passed",
            "a check failed",
            "no check result",
        );
        c.none_of(&[], "no flag", "flags");
        assert_eq!(c.verdict().verdict, Verdict::Approve);
    }

    #[test]
    fn none_of_names_what_it_found() {
        let mut c = Conditions::default();
        c.none_of(&["a".to_owned(), "b".to_owned()], "none", "found");
        assert_eq!(c.verdict(), RuleVerdict::reject("found: a, b"));
    }
}
