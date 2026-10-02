//! Earning and losing autonomy from the records: matches in a row, misses, restarts when a
//! point's settings or your rules document change, merge matches that count only after 7 days
//! without a revert, and demotions after a wrong spot check or a revert, which last until you
//! reinstate the kind.

use std::collections::BTreeSet;

use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind};
use argonauts_core::policy::{EvidencePacket, Verdict};
use argonauts_core::settings::approvals::{
    ApprovalMode, ApprovalSettings, ApproverSettings, CheckpointSettings, DecideWith,
};
use argonauts_core::{canonical_json, sha256_hex};
use serde::Serialize;
use time::{Duration, OffsetDateTime};

use super::records::{Answer, Basis, Event, Outcome, Record};
use crate::approver::render::rules_label;

/// A merge's match counts once it has stayed unreverted this long.
pub const REVERT_WINDOW: Duration = Duration::days(7);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    pub matches_in_a_row: u32,
    pub misses: u32,
    /// Auto lowered to shadow until you reinstate the kind. Matches alone never lift it.
    pub demoted: bool,
}

/// Which records of a kind count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// One recipe's, or those of runs without a recipe.
    Recipe(Option<&'a str>),
    /// Every recipe's, as `/approvals` shows a kind.
    AllRecipes,
}

impl Scope<'_> {
    fn covers(self, recipe: Option<&str>) -> bool {
        match self {
            Scope::Recipe(wanted) => wanted == recipe,
            Scope::AllRecipes => true,
        }
    }
}

/// A kind's settings as far as earning autonomy goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointSettings {
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    /// Changes with the rules, `decide_with`, or the approver it uses, never with `mode` or
    /// `otherwise`.
    pub fingerprint: String,
}

/// `None` for kinds without approval settings, which never earn auto.
pub fn point_settings(approvals: &ApprovalSettings, kind: CheckpointKind) -> Option<PointSettings> {
    use CheckpointKind as Kind;
    let approver = &approvals.approver;
    Some(match kind {
        Kind::Goal => of(&approvals.goal, approver),
        Kind::Plan => of(&approvals.plan, approver),
        Kind::Merge => of(&approvals.merge, approver),
        Kind::RecipePromotion => of(&approvals.recipe_promotion, approver),
        Kind::Escalation => of(&approvals.escalation, approver),
        Kind::Budget => of(&approvals.budget, approver),
        Kind::ChangeRequest => of(&approvals.change_request, approver),
        Kind::Handoff
        | Kind::VendorDown
        | Kind::GoalChange
        | Kind::TakeOverReturn
        | Kind::Question
        | Kind::OriginalReturned => return None,
    })
}

#[derive(Serialize)]
struct Fingerprinted<'a, R> {
    rules: &'a R,
    decide_with: DecideWith,
    approver: Option<&'a ApproverSettings>,
}

fn of<R: Default + Serialize>(
    point: &CheckpointSettings<R>,
    approver: &ApproverSettings,
) -> PointSettings {
    let uses_model = point.decide_with != DecideWith::Rules;
    let value = serde_json::to_value(Fingerprinted {
        rules: &point.rules,
        decide_with: point.decide_with,
        approver: uses_model.then_some(approver),
    })
    // Plain settings data can't fail to serialize.
    .unwrap_or_default();
    PointSettings {
        mode: point.mode,
        decide_with: point.decide_with,
        fingerprint: sha256_hex(canonical_json(&value).as_bytes()),
    }
}

/// The SHA-256 of your rules document when the approver model reads it for this kind, since
/// changing that document restarts the count as changing the settings does. `None` when rules
/// alone decide, or the document isn't in the evidence, where the approver escalates anyway.
pub fn rules_document(
    approvals: &ApprovalSettings,
    point: &PointSettings,
    evidence: &EvidencePacket,
) -> Option<String> {
    if point.decide_with == DecideWith::Rules {
        return None;
    }
    let label = rules_label(&approvals.approver.model_rules);
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
/// the checkpoint some other way, except the few that decide nothing.
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
/// wrong spot check on any recipe of the kind restarts every recipe's streak and demotes the
/// kind until you reinstate it, and each recipe then counts on its own.
pub fn tally(
    records: &[Record],
    kind: CheckpointKind,
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
        let covered = scope.covers(record.recipe.as_deref());
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
                        if kind == CheckpointKind::Merge && record.at + REVERT_WINDOW > now => {}
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
    use argonauts_core::names::CheckpointId;
    use time::macros::datetime;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-09-29 10:00 UTC);

    fn record(
        kind: CheckpointKind,
        recipe: Option<&str>,
        at: OffsetDateTime,
        event: Event,
    ) -> Record {
        Record {
            at,
            run: Some(RunId::new("run-a").expect("run id")),
            checkpoint: Some(CheckpointId::numbered(1)),
            kind,
            recipe: recipe.map(str::to_owned),
            event,
        }
    }

    fn scored(recipe: Option<&str>, fingerprint: &str, outcome: Outcome) -> Record {
        scored_with_rules(fingerprint, None, outcome, recipe)
    }

    fn scored_with_rules(
        fingerprint: &str,
        rules: Option<&str>,
        outcome: Outcome,
        recipe: Option<&str>,
    ) -> Record {
        let event = Event::Scored {
            fingerprint: fingerprint.to_owned(),
            rules: rules.map(str::to_owned),
            outcome,
        };
        record(CheckpointKind::Plan, recipe, NOW, event)
    }

    fn wrong(recipe: Option<&str>) -> Record {
        let event = Event::SpotChecked {
            fingerprint: Some("a".to_owned()),
            correct: false,
        };
        record(CheckpointKind::Plan, recipe, NOW, event)
    }

    #[test]
    fn recipes_count_apart_and_a_demotion_covers_the_kind() {
        let records = [
            scored(Some("easy"), "a", Outcome::Match),
            scored(Some("easy"), "a", Outcome::Match),
            scored(Some("risky"), "a", Outcome::Match),
            wrong(Some("risky")),
            scored(Some("easy"), "a", Outcome::Match),
        ];
        let plan = CheckpointKind::Plan;
        let easy = tally(&records, plan, Scope::Recipe(Some("easy")), Some("a"), NOW);
        assert_eq!(easy.matches_in_a_row, 1);
        assert_eq!(easy.misses, 0);
        assert!(easy.demoted);
        let risky = tally(&records, plan, Scope::Recipe(Some("risky")), Some("a"), NOW);
        assert_eq!((risky.matches_in_a_row, risky.misses), (0, 1));

        // Matches never lift a demotion. Only reinstating the kind does.
        let mut records = records.to_vec();
        for _ in 0..5 {
            records.push(scored(Some("easy"), "a", Outcome::Match));
        }
        let easy = tally(&records, plan, Scope::Recipe(Some("easy")), Some("a"), NOW);
        assert_eq!(easy.matches_in_a_row, 6);
        assert!(easy.demoted);
        records.push(Record::for_kind(plan, NOW, Event::Reinstated));
        for recipe in ["easy", "risky"] {
            let after = tally(&records, plan, Scope::Recipe(Some(recipe)), Some("a"), NOW);
            assert!(!after.demoted);
        }
        records.push(wrong(Some("easy")));
        let easy = tally(&records, plan, Scope::Recipe(Some("easy")), Some("a"), NOW);
        assert!(easy.demoted);
    }

    #[test]
    fn a_new_rules_document_restarts_the_streak() {
        let plan = CheckpointKind::Plan;
        let mut records = vec![
            scored_with_rules("a", Some("r1"), Outcome::Match, None),
            scored_with_rules("a", None, Outcome::Match, None),
            scored_with_rules("a", Some("r1"), Outcome::Match, None),
        ];
        let before = tally(&records, plan, Scope::Recipe(None), Some("a"), NOW);
        assert_eq!(before.matches_in_a_row, 3);
        records.push(scored_with_rules("a", Some("r2"), Outcome::Match, None));
        let after = tally(&records, plan, Scope::Recipe(None), Some("a"), NOW);
        assert_eq!((after.matches_in_a_row, after.misses), (1, 0));
    }

    #[test]
    fn a_reverted_match_counts_as_a_miss() {
        let plan = CheckpointKind::Plan;
        let mut records = vec![scored(None, "a", Outcome::Match)];
        let mut second = scored(None, "a", Outcome::Match);
        second.checkpoint = Some(CheckpointId::numbered(2));
        records.push(second.clone());
        records.push(scored(None, "a", Outcome::Match));
        assert_eq!(
            tally(&records, plan, Scope::Recipe(None), None, NOW).matches_in_a_row,
            3
        );
        second.event = Event::Reverted;
        records.push(second);
        let reverted = tally(&records, plan, Scope::Recipe(None), None, NOW);
        assert_eq!((reverted.matches_in_a_row, reverted.misses), (1, 1));
        assert!(!reverted.demoted);
    }

    #[test]
    fn only_approvals_score() {
        assert_eq!(score(Verdict::Escalate, Answer::Approved), None);
        assert_eq!(score(Verdict::Reject, Answer::Corrected), None);
        assert_eq!(score(Verdict::Approve, Answer::Other), None);
        assert_eq!(answer(&AnswerAction::Merge, false), Answer::Approved);
        assert_eq!(answer(&AnswerAction::Approve, true), Answer::Corrected);
        assert_eq!(answer(&AnswerAction::SendBack, false), Answer::Corrected);
        for declined in [
            AnswerAction::PullRequest,
            AnswerAction::KeepBranch,
            AnswerAction::SwitchToSolo,
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
        let before = point_settings(&approvals, CheckpointKind::Merge).expect("merge");
        approvals.merge.mode = ApprovalMode::Auto;
        approvals.merge.otherwise = argonauts_core::settings::approvals::Otherwise::SendBack;
        approvals.approver.second_model = Some("codex".parse().expect("worker"));
        let after = point_settings(&approvals, CheckpointKind::Merge).expect("merge");
        assert_eq!(before.fingerprint, after.fingerprint);

        approvals.merge.decide_with = DecideWith::Model;
        let model = point_settings(&approvals, CheckpointKind::Merge).expect("merge");
        assert_ne!(model.fingerprint, after.fingerprint);
        approvals.approver.second_model = None;
        let other_approver = point_settings(&approvals, CheckpointKind::Merge).expect("merge");
        assert_ne!(model.fingerprint, other_approver.fingerprint);
        assert_eq!(point_settings(&approvals, CheckpointKind::Question), None);
    }
}
