//! Who decides in auto mode: your rules, your judge, both, or either.

use argonauts_core::base::names::ModelRef;
use argonauts_core::safety::policy::{DecisionPoint, EvidencePacket, Verdict};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::DecideWith;
use time::OffsetDateTime;

use crate::approvals::in_autopilot;
use crate::approvals::parts::{Approve, ModelVerdict};
use crate::rules::{self, RuleVerdict};

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub verdict: Verdict,
    pub by: DecideWith,
    /// Set only when the model was asked.
    pub judge: Option<ModelRef>,
    pub reasons: Vec<String>,
}

/// `rules_and_model` needs both to approve; `rules_or_model` asks the model only about what
/// the rules didn't approve. The rules no model may waive are checked before this.
pub async fn decide(
    judge: &dyn Approve,
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Settings,
    decide_with: DecideWith,
    now: OffsetDateTime,
) -> Decision {
    let by_rules = || rules::check(point, evidence, settings, now);
    let by_model = || judge.verdict(point, evidence, settings);
    // Autopilot never waits for you, so when no model can check, your rules decide alone.
    let unchecked = |model: &ModelVerdict| in_autopilot(point) && model.judge.is_none();
    match decide_with {
        DecideWith::Rules => from_rules(by_rules(), decide_with),
        DecideWith::Model => {
            let model = by_model().await;
            if unchecked(&model) {
                return rules_alone(by_rules(), model);
            }
            from_model(model, decide_with)
        }
        DecideWith::RulesAndModel => {
            let rules = by_rules();
            // A measured failure already settles it, so the model isn't worth the tokens.
            if rules.verdict == Verdict::Reject {
                return from_rules(rules, decide_with);
            }
            let model = by_model().await;
            if unchecked(&model) {
                return rules_alone(rules, model);
            }
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
            if unchecked(&model) {
                return rules_alone(rules, model);
            }
            combine(
                either(rules.verdict, model.verdict),
                rules,
                model,
                decide_with,
            )
        }
    }
}

/// Your rules' verdict, decided by them alone, with why no model could check it last, which
/// the engine reports in the transcript.
fn rules_alone(rules: RuleVerdict, model: ModelVerdict) -> Decision {
    let mut decision = from_rules(rules, DecideWith::Rules);
    decision.reasons.extend(model.reasons);
    decision
}

/// Approve only when both approve; otherwise a reject from either wins over an ask_you.
fn both(rules: Verdict, model: Verdict) -> Verdict {
    match (rules, model) {
        (Verdict::Approve, Verdict::Approve) => Verdict::Approve,
        (Verdict::Reject, _) | (_, Verdict::Reject) => Verdict::Reject,
        _ => Verdict::AskYou,
    }
}

/// The model decides what the rules didn't approve. When it can't, a measured failure stands.
fn either(rules: Verdict, model: Verdict) -> Verdict {
    match (rules, model) {
        (Verdict::Approve, _) | (_, Verdict::Approve) => Verdict::Approve,
        (Verdict::Reject, _) | (_, Verdict::Reject) => Verdict::Reject,
        _ => Verdict::AskYou,
    }
}

fn from_rules(rules: RuleVerdict, by: DecideWith) -> Decision {
    Decision {
        verdict: rules.verdict,
        by,
        judge: None,
        reasons: rules.reasons,
    }
}

fn from_model(model: ModelVerdict, by: DecideWith) -> Decision {
    Decision {
        verdict: model.verdict,
        by,
        judge: model.judge,
        reasons: model.reasons,
    }
}

/// What held the verdict back comes first, so an approval left to you starts with it.
fn combine(verdict: Verdict, rules: RuleVerdict, model: ModelVerdict, by: DecideWith) -> Decision {
    let model_first = verdict != Verdict::Approve && rules.verdict == Verdict::Approve;
    let reasons = if model_first {
        model.reasons.into_iter().chain(rules.reasons).collect()
    } else {
        rules.reasons.into_iter().chain(model.reasons).collect()
    };
    Decision {
        verdict,
        by,
        judge: model.judge,
        reasons,
    }
}
