//! A fresh adjudicator rules when two or more reviewers disagree.

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::ReviewEvent;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::StepName;
use argonauts_core::steps::judgment::{
    Adjudicate, AdjudicateInput, AdjudicateOutput, ReviewOutput, ReviewVerdict,
};
use argonauts_core::worker::Access;

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{self, crew_for, families};
use crate::prompts::review::ADJUDICATE;
use crate::review::{Verdict, agree, card_of, diff_cut};
use crate::state::RunState;
use crate::workflow::{StepCall, StepDone};

pub async fn adjudicate(host: &Host, card: &CardId) -> EngineResult<Verdict> {
    let family = families(host);
    let (record, goal) =
        host.read(|state| (state.cards.get(card).cloned(), context::goal_text(state)));
    let Some(record) = record else {
        return Err(EngineError::Refused(format!(
            "{card} is not a card of this run"
        )));
    };
    let mut round = 0;
    let mut reviews: Vec<(WorkerRef, ReviewOutput)> = Vec::new();
    let mut ruled: Option<AdjudicateOutput> = None;
    for event in &record.reviews {
        match event {
            ReviewEvent::Reviewed {
                reviewer,
                round: r,
                review,
                ..
            } => {
                if *r != round {
                    round = *r;
                    reviews.clear();
                    ruled = None;
                }
                reviews.push((reviewer.clone(), review.clone()));
            }
            ReviewEvent::Adjudicated { ruling, .. } => ruled = Some(ruling.clone()),
            ReviewEvent::TestsWritten { .. } => {}
        }
    }
    if let Some(ruling) = ruled {
        return Ok(verdict(&ruling));
    }
    let owner = record.owner.clone().or_else(|| record.spec.owner.clone());
    let reviewers: Vec<WorkerRef> = reviews.iter().map(|(r, _)| r.clone()).collect();
    let Some(adjudicator) = pick(host, owner.as_ref(), &reviewers, &family) else {
        return Ok(Verdict::Escalated {
            reasons: vec!["No adjudicator outside the owner and reviewers".to_owned()],
        });
    };
    let diff = match (&record.worktree, &record.base) {
        (Some(worktree), Some(base)) => host.git().review_diff(worktree, base, "HEAD").await?,
        _ => String::new(),
    };
    let (chunks, note) = diff_cut::cut(&diff, host.settings().review.max_diff_lines);
    let input = AdjudicateInput {
        goal,
        card: format!("{}\n\n{}", record.spec.title, record.spec.task),
        diff: chunks.concat() + &note.map(|n| format!("\n{n}\n")).unwrap_or_default(),
        reviews: reviews.into_iter().map(|(_, review)| review).collect(),
    };
    let mut ask = Ask::new(
        None,
        RoleName::Adjudicator,
        adjudicator.clone(),
        ADJUDICATE,
        input,
    );
    ask.card = Some(card.clone());
    ask.access = Access::NoTools;
    ask.blind = true;
    ask.slot = round;
    match host.ask::<Adjudicate>(ask).await {
        Ok(answered) => {
            let verdict = verdict(&answered.output);
            host.record(
                Some(card),
                Some(&answered.session),
                RunEventKind::Review(ReviewEvent::Adjudicated {
                    adjudicator,
                    ruling: answered.output,
                }),
            )?;
            Ok(verdict)
        }
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        Err(error) => Ok(Verdict::Escalated {
            reasons: vec![format!("The adjudicator failed: {error}")],
        }),
    }
}

/// A fresh adjudicator: never the owner or a reviewer, from another family than the owner when
/// there is one.
fn pick(
    host: &Host,
    owner: Option<&WorkerRef>,
    reviewers: &[WorkerRef],
    family: &dyn Fn(&WorkerRef) -> String,
) -> Option<WorkerRef> {
    let owner_family = owner.map(family);
    let free: Vec<WorkerRef> = crew_for(host.settings(), RoleName::Adjudicator)
        .into_iter()
        .filter(|w| Some(w) != owner && !reviewers.contains(w))
        .collect();
    free.iter()
        .find(|w| Some(family(w)) != owner_family)
        .or(free.first())
        .cloned()
}

fn verdict(ruling: &AdjudicateOutput) -> Verdict {
    match ruling.verdict {
        ReviewVerdict::Approve => Verdict::Approved,
        ReviewVerdict::RequestChanges => Verdict::ChangesRequested {
            comments: ruling
                .required_changes
                .iter()
                .chain(&ruling.reasons)
                .cloned()
                .collect(),
        },
        ReviewVerdict::Reject => Verdict::Escalated {
            reasons: ruling.reasons.clone(),
        },
    }
}

/// `judge` with role `adjudicator`: rules on the newest disagreeing reviews, of the card the
/// step works on or of the workflow's last `review` step.
pub async fn step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    if let Some(card) = card_of(host, call) {
        let disagree = host.read(|state| {
            let reviews = latest_card_reviews(state, &card);
            reviews.len() > 1 && !agree(&reviews)
        });
        if !disagree {
            return Ok(StepDone::skipped("The reviewers agree"));
        }
        let verdict = adjudicate(host, &card).await?;
        return Ok(StepDone::done(verdict.line()));
    }
    let (reviewers, reviews) = host.read(latest_step_reviews);
    if reviews.len() < 2 || agree(&reviews) {
        return Ok(StepDone::skipped("The reviewers agree"));
    }
    let family = families(host);
    let Some(adjudicator) = pick(host, None, &reviewers, &family) else {
        return Ok(StepDone::skipped("No adjudicator outside the reviewers"));
    };
    let text = context::step_input(host, call).await?;
    let (chunks, note) = diff_cut::cut(&text, host.settings().review.max_diff_lines);
    let input = AdjudicateInput {
        goal: host.read(context::goal_text),
        card: call
            .step
            .task
            .clone()
            .unwrap_or_else(|| "Settle the reviews below.".to_owned()),
        diff: chunks.concat() + &note.map(|n| format!("\n{n}\n")).unwrap_or_default(),
        reviews,
    };
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::Adjudicator,
        adjudicator.clone(),
        ADJUDICATE,
        input,
    );
    ask.access = Access::NoTools;
    ask.blind = true;
    let answered = host.ask::<Adjudicate>(ask).await?;
    let line = verdict(&answered.output).line();
    host.record(
        None,
        Some(&answered.session),
        RunEventKind::Review(ReviewEvent::Adjudicated {
            adjudicator,
            ruling: answered.output,
        }),
    )?;
    Ok(StepDone::done(line))
}

fn latest_card_reviews(state: &RunState, card: &CardId) -> Vec<ReviewOutput> {
    let Some(record) = state.cards.get(card) else {
        return Vec::new();
    };
    let round = record
        .reviews
        .iter()
        .filter_map(|event| match event {
            ReviewEvent::Reviewed { round, .. } => Some(*round),
            _ => None,
        })
        .max();
    record
        .reviews
        .iter()
        .filter_map(|event| match event {
            ReviewEvent::Reviewed {
                round: r, review, ..
            } if Some(*r) == round => Some(review.clone()),
            _ => None,
        })
        .collect()
}

/// The reviews of the newest workflow `review` step outside any card, from its reviewers'
/// sessions.
fn latest_step_reviews(state: &RunState) -> (Vec<WorkerRef>, Vec<ReviewOutput>) {
    let sessions: Vec<_> = state
        .sessions
        .by_key
        .values()
        .filter(|s| {
            s.card.is_none()
                && s.started.role == RoleName::Reviewer
                && s.started.step == Some(StepName::Review)
        })
        .collect();
    let Some(newest) = sessions.iter().max_by_key(|s| s.since) else {
        return (Vec::new(), Vec::new());
    };
    let step = newest.started.workflow_step.clone();
    sessions
        .iter()
        .filter(|s| s.started.workflow_step == step)
        .filter_map(|s| {
            let value = s.end.as_ref()?.structured.clone()?;
            let review = serde_json::from_value::<ReviewOutput>(value).ok()?;
            Some((s.started.worker.clone(), review))
        })
        .unzip()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rulings_map_to_verdicts() {
        let ruling = |verdict| AdjudicateOutput {
            verdict,
            reasons: vec!["tests are thin".to_owned()],
            required_changes: vec!["add a test for 429".to_owned()],
        };
        assert_eq!(verdict(&ruling(ReviewVerdict::Approve)), Verdict::Approved);
        assert_eq!(
            verdict(&ruling(ReviewVerdict::RequestChanges)),
            Verdict::ChangesRequested {
                comments: vec!["add a test for 429".to_owned(), "tests are thin".to_owned()]
            }
        );
        assert_eq!(
            verdict(&ruling(ReviewVerdict::Reject)),
            Verdict::Escalated {
                reasons: vec!["tests are thin".to_owned()]
            }
        );
    }
}
