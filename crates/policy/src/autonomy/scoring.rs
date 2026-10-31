//! Earning and losing autonomy from the records: matches in a row, misses, restarts when a
//! point's settings or your rules document change, merge matches that count only after 7 days
//! without a revert, and demotions after a wrong spot check or a revert, which last until you
//! reinstate the kind.

use std::collections::BTreeSet;

use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::safety::policy::{EvidencePacket, Verdict};
use argonauts_core::settings::approvals::{
    ApprovalMode, ApprovalPointSettings, ApprovalSettings, DecideWith, Decides, JudgeSettings,
};
use argonauts_core::{canonical_json, sha256_hex};
use serde::Serialize;
use time::{Duration, OffsetDateTime};

use super::records::{Answer, Basis, Event, Outcome, Record};
use crate::judge::render::rules_label;

/// A merge's match counts once it has stayed unreverted this long.
pub const REVERT_WINDOW: Duration = Duration::days(7);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    pub matches_in_a_row: u32,
    pub misses: u32,
    /// Auto lowered to auto preview until you reinstate the kind. Matches alone never lift it.
    pub demoted: bool,
}

/// Which records of a kind count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// One learned workflow's, or those of runs without a learned workflow.
    LearnedWorkflow(Option<&'a str>),
    /// Every learned workflow's, as `/approvals` shows a kind.
    AllLearnedWorkflows,
}

impl Scope<'_> {
    fn covers(self, learned_workflow: Option<&str>) -> bool {
        match self {
            Scope::LearnedWorkflow(wanted) => wanted == learned_workflow,
            Scope::AllLearnedWorkflows => true,
        }
    }
}

/// A kind's settings as far as earning autonomy goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointSettings {
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    /// Changes with the rules, `decide_with`, or the judge it uses, never with `mode` or
    /// `otherwise`.
    pub fingerprint: String,
}

/// `None` for kinds without approval settings, which never earn auto.
pub fn point_settings(approvals: &ApprovalSettings, kind: ApprovalKind) -> Option<PointSettings> {
    use ApprovalKind as Kind;
    let rules = &approvals.rules;
    let promotion = approvals.workflow_promotion.as_approval();
    Some(match kind {
        Kind::Understand => of(&approvals.understand, &NoRules {}, approvals),
        Kind::Plan => of(&approvals.plan, &rules.plan, approvals),
        Kind::Merge => of(&approvals.merge, &rules.merge, approvals),
        Kind::WorkflowPromotion => of(&promotion, &rules.workflow_promotion, approvals),
        Kind::Stuck => of(&approvals.stuck, &rules.stuck, approvals),
        Kind::Limit => of(&approvals.limit, &rules.limit, approvals),
        Kind::ChangeRequest => of(&approvals.change_request, &NoRules {}, approvals),
        Kind::Fallback
        | Kind::AgentDown
        | Kind::GoalChange
        | Kind::TakeOverReturn
        | Kind::Question
        | Kind::OriginalReturned => return None,
    })
}

/// A goal and a change request have no numbers of their own.
#[derive(Serialize)]
struct NoRules {}

#[derive(Serialize)]
struct Fingerprinted<'a, R> {
    rules: &'a R,
    decide_with: DecideWith,
    /// Who judges, and by which rules file, where a model has a say.
    decides: Option<&'a Decides>,
    judge: Option<&'a JudgeSettings>,
}

fn of<R: Serialize>(
    point: &ApprovalPointSettings,
    rules: &R,
    approvals: &ApprovalSettings,
) -> PointSettings {
    let uses_model = point.decide_with != DecideWith::Rules;
    let value = serde_json::to_value(Fingerprinted {
        rules,
        decide_with: point.decide_with,
        decides: uses_model.then_some(&approvals.decides),
        judge: uses_model.then_some(&approvals.judge),
    })
    // Plain settings data can't fail to serialize.
    .unwrap_or_default();
    PointSettings {
        mode: point.mode,
        decide_with: point.decide_with,
        fingerprint: sha256_hex(canonical_json(&value).as_bytes()),
    }
}

/// The SHA-256 of your rules document when the judge reads it for this kind, since
/// changing that document restarts the count as changing the settings does. `None` when rules
/// alone decide, or the document isn't in the evidence, where the judge asks you anyway.
pub fn rules_document(
    approvals: &ApprovalSettings,
    point: &PointSettings,
    evidence: &EvidencePacket,
) -> Option<String> {
    if point.decide_with == DecideWith::Rules {
        return None;
    }
    let label = rules_label(&approvals.judge.model_rules);
    let document = evidence
        .documents
        .iter()
        .find(|document| document.label == label)?;
    Some(match &document.sha256 {
        Some(sha256) => sha256.trim().to_ascii_lowercase(),
        None => sha256_hex(document.text.as_bytes()),
    })
}

/// Only an unedited approve or merge agrees with auto's approval. Every other answer decides
/// the approval some other way, except the few that decide nothing.
pub fn answer(action: &AnswerAction, edited: bool) -> Answer {
    use AnswerAction as Action;
    match action {
        Action::Approve | Action::Merge if !edited => Answer::Approved,
        Action::ShowDiff | Action::OtherCandidates => Answer::Other,
        _ => Answer::Corrected,
    }
}

/// Only a verdict auto would have approved counts.
pub fn score(would: Verdict, answer: Answer) -> Option<Outcome> {
    match (would, answer) {
        (Verdict::Approve, Answer::Approved) => Some(Outcome::Match),
        (Verdict::Approve, Answer::Corrected) => Some(Outcome::Miss),
        _ => None,
    }
}

impl Basis<'_> {
    /// A record whose rules document isn't known goes on with any streak under the same
    /// settings.
    fn goes_on_from(self, streak: Basis<'_>) -> bool {
        self.fingerprint == streak.fingerprint
            && (self.rules.is_none() || streak.rules.is_none() || self.rules == streak.rules)
    }
}

/// With `fingerprint`, a streak scored under other settings reads as restarted. A change of
/// your rules document restarts it once a record scored under the new document arrives. A
/// wrong spot check on any learned workflow of the kind restarts every learned workflow's streak
/// and demotes the kind until you reinstate it, and each learned workflow then counts on its own.
pub fn tally(
    records: &[Record],
    kind: ApprovalKind,
    scope: Scope<'_>,
    fingerprint: Option<&str>,
    now: OffsetDateTime,
) -> Tally {
    let of_kind = || records.iter().filter(move |record| record.kind == kind);
    let reverted: BTreeSet<_> = of_kind()
        .filter(|record| record.event == Event::Reverted)
        .map(Record::key)
        .collect();
    let mut streak = 0u32;
    let mut misses = 0u32;
    let mut streak_basis: Option<Basis<'_>> = None;
    let mut demoted = false;

    for record in of_kind() {
        let covered = scope.covers(record.learned_workflow.as_deref());
        match &record.event {
            Event::Scored {
                fingerprint: scored_under,
                rules,
                outcome,
            } if covered => {
                let basis = Basis {
                    fingerprint: scored_under,
                    rules: rules.as_deref(),
                };
                streak_basis = match streak_basis {
                    Some(current) if basis.goes_on_from(current) => Some(Basis {
                        rules: basis.rules.or(current.rules),
                        ..current
                    }),
                    _ => {
                        streak = 0;
                        misses = 0;
                        Some(basis)
                    }
                };
                let outcome = if reverted.contains(&record.key()) {
                    Outcome::Miss
                } else {
                    *outcome
                };
                match outcome {
                    Outcome::Match
                        if kind == ApprovalKind::Merge && record.at + REVERT_WINDOW > now => {}
                    Outcome::Match => streak = streak.saturating_add(1),
                    Outcome::Miss => {
                        streak = 0;
                        misses = misses.saturating_add(1);
                    }
                }
            }
            Event::SpotChecked {
                fingerprint: checked_under,
                correct: false,
            } => {
                if covered {
                    if let Some(checked_under) = checked_under.as_deref()
                        && streak_basis.map(|basis| basis.fingerprint) != Some(checked_under)
                    {
                        misses = 0;
                        streak_basis = Some(Basis {
                            fingerprint: checked_under,
                            rules: None,
                        });
                    }
                    misses = misses.saturating_add(1);
                }
                streak = 0;
                demoted = true;
            }
            Event::Reinstated => demoted = false,
            _ => {}
        }
    }

    let restarted = matches!(
        (fingerprint, streak_basis),
        (Some(current), Some(scored)) if current != scored.fingerprint
    );
    Tally {
        matches_in_a_row: if restarted { 0 } else { streak },
        misses: if restarted { 0 } else { misses },
        demoted,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::base::names::ApprovalId;
    use time::macros::datetime;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-09-29 10:00 UTC);

    fn record(
        kind: ApprovalKind,
        learned_workflow: Option<&str>,
        at: OffsetDateTime,
        event: Event,
    ) -> Record {
        Record {
            at,
            run: Some(RunId::new("run-a").expect("run id")),
            approval: Some(ApprovalId::numbered(1)),
            kind,
            learned_workflow: learned_workflow.map(str::to_owned),
            event,
        }
    }

    fn scored(learned_workflow: Option<&str>, fingerprint: &str, outcome: Outcome) -> Record {
        scored_with_rules(fingerprint, None, outcome, learned_workflow)
    }

    fn scored_with_rules(
        fingerprint: &str,
        rules: Option<&str>,
        outcome: Outcome,
        learned_workflow: Option<&str>,
    ) -> Record {
        let event = Event::Scored {
            fingerprint: fingerprint.to_owned(),
            rules: rules.map(str::to_owned),
            outcome,
        };
        record(ApprovalKind::Plan, learned_workflow, NOW, event)
    }

    fn wrong(learned_workflow: Option<&str>) -> Record {
        let event = Event::SpotChecked {
            fingerprint: Some("a".to_owned()),
            correct: false,
        };
        record(ApprovalKind::Plan, learned_workflow, NOW, event)
    }

    #[test]
    fn learned_workflows_count_apart_and_a_demotion_covers_the_kind() {
        let records = [
            scored(Some("easy"), "a", Outcome::Match),
            scored(Some("easy"), "a", Outcome::Match),
            scored(Some("risky"), "a", Outcome::Match),
            wrong(Some("risky")),
            scored(Some("easy"), "a", Outcome::Match),
        ];
        let plan = ApprovalKind::Plan;
        let easy = tally(
            &records,
            plan,
            Scope::LearnedWorkflow(Some("easy")),
            Some("a"),
            NOW,
        );
        assert_eq!(easy.matches_in_a_row, 1);
        assert_eq!(easy.misses, 0);
        assert!(easy.demoted);
        let risky = tally(
            &records,
            plan,
            Scope::LearnedWorkflow(Some("risky")),
            Some("a"),
            NOW,
        );
        assert_eq!((risky.matches_in_a_row, risky.misses), (0, 1));

        // Matches never lift a demotion. Only reinstating the kind does.
        let mut records = records.to_vec();
        for _ in 0..5 {
            records.push(scored(Some("easy"), "a", Outcome::Match));
        }
        let easy = tally(
            &records,
            plan,
            Scope::LearnedWorkflow(Some("easy")),
            Some("a"),
            NOW,
        );
        assert_eq!(easy.matches_in_a_row, 6);
        assert!(easy.demoted);
        records.push(Record::for_kind(plan, NOW, Event::Reinstated));
        for learned_workflow in ["easy", "risky"] {
            let after = tally(
                &records,
                plan,
                Scope::LearnedWorkflow(Some(learned_workflow)),
                Some("a"),
                NOW,
            );
            assert!(!after.demoted);
        }
        records.push(wrong(Some("easy")));
        let easy = tally(
            &records,
            plan,
            Scope::LearnedWorkflow(Some("easy")),
            Some("a"),
            NOW,
        );
        assert!(easy.demoted);
    }

    #[test]
    fn a_new_rules_document_restarts_the_streak() {
        let plan = ApprovalKind::Plan;
        let mut records = vec![
            scored_with_rules("a", Some("r1"), Outcome::Match, None),
            scored_with_rules("a", None, Outcome::Match, None),
            scored_with_rules("a", Some("r1"), Outcome::Match, None),
        ];
        let before = tally(&records, plan, Scope::LearnedWorkflow(None), Some("a"), NOW);
        assert_eq!(before.matches_in_a_row, 3);
        records.push(scored_with_rules("a", Some("r2"), Outcome::Match, None));
        let after = tally(&records, plan, Scope::LearnedWorkflow(None), Some("a"), NOW);
        assert_eq!((after.matches_in_a_row, after.misses), (1, 0));
    }

    #[test]
    fn a_reverted_match_counts_as_a_miss() {
        let plan = ApprovalKind::Plan;
        let mut records = vec![scored(None, "a", Outcome::Match)];
        let mut second = scored(None, "a", Outcome::Match);
        second.approval = Some(ApprovalId::numbered(2));
        records.push(second.clone());
        records.push(scored(None, "a", Outcome::Match));
        assert_eq!(
            tally(&records, plan, Scope::LearnedWorkflow(None), None, NOW).matches_in_a_row,
            3
        );
        second.event = Event::Reverted;
        records.push(second);
        let reverted = tally(&records, plan, Scope::LearnedWorkflow(None), None, NOW);
        assert_eq!((reverted.matches_in_a_row, reverted.misses), (1, 1));
        assert!(!reverted.demoted);
    }

    #[test]
    fn only_approvals_score() {
        assert_eq!(score(Verdict::AskYou, Answer::Approved), None);
        assert_eq!(score(Verdict::Reject, Answer::Corrected), None);
        assert_eq!(score(Verdict::Approve, Answer::Other), None);
        assert_eq!(answer(&AnswerAction::Merge, false), Answer::Approved);
        assert_eq!(answer(&AnswerAction::Approve, true), Answer::Corrected);
        assert_eq!(answer(&AnswerAction::SendBack, false), Answer::Corrected);
        for declined in [
            AnswerAction::PullRequest,
            AnswerAction::KeepBranch,
            AnswerAction::SwitchToCaptain,
            AnswerAction::Wait,
            AnswerAction::UpgradeReview,
        ] {
            assert_eq!(answer(&declined, false), Answer::Corrected, "{declined:?}");
        }
        assert_eq!(answer(&AnswerAction::ShowDiff, false), Answer::Other);
        assert_eq!(answer(&AnswerAction::OtherCandidates, false), Answer::Other);
    }

    #[test]
    fn mode_and_otherwise_leave_the_fingerprint_alone() {
        let mut approvals = ApprovalSettings::default();
        let before = point_settings(&approvals, ApprovalKind::Merge).expect("merge");
        approvals.merge.mode = ApprovalMode::Auto;
        approvals.merge.otherwise = argonauts_core::settings::approvals::Otherwise::SendBack;
        approvals.judge.second_model = Some("codex".parse().expect("worker"));
        let after = point_settings(&approvals, ApprovalKind::Merge).expect("merge");
        assert_eq!(before.fingerprint, after.fingerprint);

        approvals.merge.decide_with = DecideWith::Model;
        let model = point_settings(&approvals, ApprovalKind::Merge).expect("merge");
        assert_ne!(model.fingerprint, after.fingerprint);
        approvals.judge.second_model = None;
        let other_judge = point_settings(&approvals, ApprovalKind::Merge).expect("merge");
        assert_ne!(model.fingerprint, other_judge.fingerprint);
        assert_eq!(point_settings(&approvals, ApprovalKind::Question), None);
    }
}
