//! Who decides in auto mode: your rules, your approver model, both, or either.

use argonauts_core::names::WorkerRef;
use argonauts_core::policy::{DecisionPoint, EvidencePacket, Verdict};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::DecideWith;
use time::OffsetDateTime;

use crate::parts::{Approve, ModelVerdict};
use crate::rules::{self, RuleVerdict};

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub verdict: Verdict,
    pub by: DecideWith,
    /// Set only when the model was asked.
    pub approver: Option<WorkerRef>,
    pub reasons: Vec<String>,
}

/// `rules_and_model` needs both to approve; `rules_or_model` asks the model only about what
/// the rules didn't approve. The rules no model may waive are checked before this.
pub async fn decide(
    approver: &dyn Approve,
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Settings,
    decide_with: DecideWith,
    now: OffsetDateTime,
) -> Decision {
    let by_rules = || rules::check(point, evidence, settings, now);
    let by_model = || approver.verdict(point, evidence, settings);
    match decide_with {
        DecideWith::Rules => from_rules(by_rules(), decide_with),
        DecideWith::Model => from_model(by_model().await, decide_with),
        DecideWith::RulesAndModel => {
            let rules = by_rules();
            // A measured failure already settles it, so the model isn't worth the tokens.
            if rules.verdict == Verdict::Reject {
                return from_rules(rules, decide_with);
            }
            let model = by_model().await;
            combine(
                both(rules.verdict, model.verdict),
                rules,
                model,
                decide_with,
            )
        }
        DecideWith::RulesOrModel => {
            let rules = by_rules();
            if rules.verdict == Verdict::Approve {
                return from_rules(rules, decide_with);
            }
            let model = by_model().await;
            combine(
                either(rules.verdict, model.verdict),
                rules,
                model,
                decide_with,
            )
        }
    }
}

/// Approve only when both approve; otherwise a reject from either wins over an escalate.
fn both(rules: Verdict, model: Verdict) -> Verdict {
    match (rules, model) {
        (Verdict::Approve, Verdict::Approve) => Verdict::Approve,
        (Verdict::Reject, _) | (_, Verdict::Reject) => Verdict::Reject,
        _ => Verdict::Escalate,
    }
}

/// The model decides what the rules didn't approve. When it can't, a measured failure stands.
fn either(rules: Verdict, model: Verdict) -> Verdict {
    match (rules, model) {
        (Verdict::Approve, _) | (_, Verdict::Approve) => Verdict::Approve,
        (Verdict::Reject, _) | (_, Verdict::Reject) => Verdict::Reject,
        _ => Verdict::Escalate,
    }
}

fn from_rules(rules: RuleVerdict, by: DecideWith) -> Decision {
    Decision {
        verdict: rules.verdict,
        by,
        approver: None,
        reasons: rules.reasons,
    }
}

fn from_model(model: ModelVerdict, by: DecideWith) -> Decision {
    Decision {
        verdict: model.verdict,
        by,
        approver: model.approver,
        reasons: model.reasons,
    }
}

fn combine(verdict: Verdict, rules: RuleVerdict, model: ModelVerdict, by: DecideWith) -> Decision {
    let mut reasons = rules.reasons;
    reasons.extend(model.reasons);
    Decision {
        verdict,
        by,
        approver: model.approver,
        reasons,
    }
}
