//! What planning measured, for the plan checkpoint and the pick card: the judging facts the
//! plan rules read, and the flags you see.

use argonauts_core::events::plan::JudgingResult;
use argonauts_core::names::PlanLabel;
use argonauts_core::policy::EvidenceFacts;

use crate::engine::host::Host;

/// A plan nobody contested, merged on the fast path or planned by the coordinator, has the
/// whole scale as its margin.
const UNCONTESTED_MARGIN: f64 = 10.0;

#[derive(Debug, Clone, PartialEq)]
pub struct PlanReview {
    pub judging: Option<JudgingResult>,
    /// Plans holding text addressed to judges.
    pub flagged: Vec<PlanLabel>,
    /// Topics with a quote code couldn't find in the plan.
    pub unverified: Vec<String>,
    pub disputes_settled: bool,
}

pub fn review(host: &Host) -> PlanReview {
    host.read(|state| PlanReview {
        judging: state.plan.judged.clone(),
        flagged: state.plan.flagged_plans(),
        unverified: state.plan.unverified_topics(),
        disputes_settled: state.plan.disputes_settled(),
    })
}

impl PlanReview {
    /// The judging facts. Every counted score comes from another family than the plan's
    /// author, so the judges were of other families when every plan has a counted score. With
    /// `judging.method: human` nobody scored, and the facts say nothing.
    pub fn facts(&self) -> EvidenceFacts {
        let (spread, margin, other_provider) = match &self.judging {
            Some(judged) if judged.plans.iter().all(|tally| tally.counted == 0) => {
                (None, None, None)
            }
            Some(judged) => (
                Some(
                    judged
                        .plans
                        .iter()
                        .map(|plan| plan.spread)
                        .fold(0.0, f64::max),
                ),
                Some(judged.margin),
                Some(judged.plans.iter().all(|tally| tally.counted > 0)),
            ),
            None => (Some(0.0), Some(UNCONTESTED_MARGIN), Some(true)),
        };
        EvidenceFacts {
            open_questions: Some(u32::try_from(self.unverified.len()).unwrap_or(u32::MAX)),
            max_score_spread: spread,
            winner_margin: margin,
            judges_other_provider: other_provider,
            disputes_settled: Some(self.disputes_settled),
            flagged_plans: Some(u32::try_from(self.flagged.len()).unwrap_or(u32::MAX)),
            ..EvidenceFacts::default()
        }
    }

    pub fn flags(&self) -> Vec<String> {
        let mut flags: Vec<String> = self
            .flagged
            .iter()
            .map(|label| {
                format!(
                    "Plan {label} holds text addressed to its judges, so it can't be auto-approved."
                )
            })
            .collect();
        if !self.unverified.is_empty() {
            flags.push(format!(
                "Quotes in these topics weren't found in the plans they came from: {}.",
                self.unverified.join(", ")
            ));
        }
        if !self.disputes_settled {
            flags.push("An experiment that fit its limit has no report yet.".to_owned());
        }
        flags
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::plan::PlanTally;
    use argonauts_core::settings::protocol::JudgingMethod;

    use super::*;

    fn tally(label: PlanLabel, counted: u32, spread: f64) -> PlanTally {
        PlanTally {
            label,
            author: "codex".parse().unwrap(),
            mean: 7.0,
            spread,
            counted,
            dropped: 0,
        }
    }

    fn judged(method: JudgingMethod, plans: Vec<PlanTally>) -> Option<JudgingResult> {
        Some(JudgingResult {
            method,
            plans,
            winner: None,
            margin: 0.4,
            near_tie: true,
        })
    }

    #[test]
    fn the_facts_the_plan_rules_read() {
        let review = PlanReview {
            judging: judged(
                JudgingMethod::Cross,
                vec![tally(1, 2, 0.5), tally(2, 2, 1.5)],
            ),
            flagged: vec![2],
            unverified: vec!["counters".to_owned()],
            disputes_settled: true,
        };
        let facts = review.facts();
        assert_eq!(facts.max_score_spread, Some(1.5));
        assert_eq!(facts.winner_margin, Some(0.4));
        assert_eq!(facts.judges_other_provider, Some(true));
        assert_eq!(facts.flagged_plans, Some(1));
        assert_eq!(facts.open_questions, Some(1));
        assert_eq!(facts.disputes_settled, Some(true));
        let flags = review.flags();
        assert!(flags[0].contains("Plan 2 holds text addressed to its judges"));
        assert!(flags[1].contains("counters"));

        // A plan scored only by its own family has no counted judge.
        let lopsided = PlanReview {
            judging: judged(
                JudgingMethod::Cross,
                vec![tally(1, 2, 0.5), tally(2, 0, 0.0)],
            ),
            ..review.clone()
        };
        assert_eq!(lopsided.facts().judges_other_provider, Some(false));

        let human = PlanReview {
            judging: judged(JudgingMethod::Human, vec![tally(1, 0, 0.0)]),
            ..review.clone()
        };
        assert_eq!(human.facts().winner_margin, None);

        let fast = PlanReview {
            judging: None,
            flagged: Vec::new(),
            unverified: Vec::new(),
            disputes_settled: true,
        };
        let facts = fast.facts();
        assert_eq!(facts.max_score_spread, Some(0.0));
        assert_eq!(facts.winner_margin, Some(UNCONTESTED_MARGIN));
        assert!(fast.flags().is_empty());
    }
}
