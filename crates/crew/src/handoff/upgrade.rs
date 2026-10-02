//! The upgrade review: the original, or a model at least as strong, reviews everything since
//! the checkpoint, whoever owns the card now. Its approval clears the debt, and so does its
//! request for changes, which sends the work back with its comments. A rejection keeps the
//! debt, so the merge rule still asks you.

use std::slice;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardEvent, ReviewEvent};
use argonauts_core::names::WorkerRef;
use argonauts_core::{CardId, Note, NoteLevel};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::handoff::{debt, rules};
use crate::review::{self, Verdict};
use crate::state::cards::CardRecord;

/// Reviews the card for its debt with `reviewer` alone, in the round after the card's last
/// review. Returns the verdict and whether the debt cleared.
pub async fn review(
    host: &Host,
    card: &CardId,
    reviewer: &WorkerRef,
) -> EngineResult<(Verdict, bool)> {
    let record = rules::record(host, card)?;
    let Some(owed) = record.debt.clone() else {
        return Ok((Verdict::Approved, true));
    };
    let round = next_round(&record.reviews);
    let verdict = review::review_by(host, card, round, slice::from_ref(reviewer)).await?;
    let record = rules::record(host, card)?;
    let unknown = rules::applied_for(host, &record.spec, &owed.needs).unknown_tier;
    let reviewed = round_reviewers(&record.reviews, round).contains(reviewer);
    let strong = !debt::weaker(host, &record.spec, reviewer, &owed.needs, unknown);
    let settles = matches!(
        verdict,
        Verdict::Approved | Verdict::ChangesRequested { .. }
    );
    let cleared = reviewed && strong && settles;
    if cleared {
        host.record(
            Some(card),
            None,
            RunEventKind::Card(CardEvent::DebtCleared {
                by: reviewer.clone(),
            }),
        )?;
    } else {
        let why = if !reviewed {
            format!("{reviewer}'s review didn't happen: {}", verdict.line())
        } else if !strong {
            format!("{reviewer} is weaker than {}", owed.needs)
        } else {
            format!("{reviewer} rejected the work: {}", verdict.line())
        };
        let text = format!("{card} still needs a review by {}, since {why}", owed.needs);
        host.record(
            Some(card),
            None,
            RunEventKind::Note(Note {
                level: NoteLevel::Warn,
                text,
            }),
        )?;
    }
    Ok((verdict, cleared))
}

/// Whether the card's newest review is `reviewer`'s, so nothing changed since it looked.
pub fn reviewed_last(record: &CardRecord, reviewer: &WorkerRef) -> bool {
    record.reviews.iter().rev().find_map(|event| match event {
        ReviewEvent::Reviewed { reviewer: by, .. } => Some(by == reviewer),
        _ => None,
    }) == Some(true)
}

/// The round after the card's last review, from 1.
pub fn next_round(reviews: &[ReviewEvent]) -> u32 {
    reviews
        .iter()
        .filter_map(|event| match event {
            ReviewEvent::Reviewed { round, .. } => Some(*round),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1
}

/// Who reviewed in `round`, and the adjudicator when there was one.
pub fn round_reviewers(reviews: &[ReviewEvent], round: u32) -> Vec<WorkerRef> {
    let mut found: Vec<WorkerRef> = Vec::new();
    let mut in_round = false;
    for event in reviews {
        let worker = match event {
            ReviewEvent::Reviewed {
                reviewer,
                round: this,
                ..
            } => {
                in_round = *this == round;
                in_round.then_some(reviewer)
            }
            ReviewEvent::Adjudicated { adjudicator, .. } => in_round.then_some(adjudicator),
            ReviewEvent::TestsWritten { .. } => None,
        };
        if let Some(worker) = worker
            && !found.contains(worker)
        {
            found.push(worker.clone());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::steps::judgment::{AdjudicateOutput, ReviewOutput, ReviewVerdict};

    fn reviewed(reviewer: &str, round: u32) -> ReviewEvent {
        ReviewEvent::Reviewed {
            reviewer: reviewer.parse().unwrap(),
            blind: false,
            round,
            review: ReviewOutput {
                verdict: ReviewVerdict::Approve,
                summary: String::new(),
                comments: Vec::new(),
                tamper_suspected: false,
            },
        }
    }

    #[test]
    fn rounds_and_their_reviewers() {
        assert_eq!(next_round(&[]), 1);
        let reviews = vec![
            reviewed("codex/gpt-5.6", 1),
            reviewed("agy/gemini-3.6", 1),
            reviewed("codex/gpt-5.6", 2),
            ReviewEvent::Adjudicated {
                adjudicator: "claude-code/opus".parse().unwrap(),
                ruling: AdjudicateOutput {
                    verdict: ReviewVerdict::Approve,
                    reasons: Vec::new(),
                    required_changes: Vec::new(),
                },
            },
        ];
        assert_eq!(next_round(&reviews), 3);
        let names = |round| -> Vec<String> {
            round_reviewers(&reviews, round)
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(names(1), ["codex/gpt-5.6", "agy/gemini-3.6"]);
        assert_eq!(names(2), ["codex/gpt-5.6", "claude-code/opus"]);
        assert!(names(3).is_empty());
    }
}
