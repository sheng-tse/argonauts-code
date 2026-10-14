//! Review: each reviewer runs `Review` on the conductor's diff, the gate results, and the
//! acceptance tests, never the owner's notes; blind reviewers never see each other before
//! adjudication. Disagreement among two or more reviewers goes to a fresh adjudicator.

use std::path::Path;

use futures::future::join_all;

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, ReviewEvent};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::Step;
use argonauts_core::steps::judgment::{Review, ReviewInput, ReviewOutput, ReviewVerdict};
use argonauts_core::worker::{Access, WorkerKind};

use crate::engine::host::{Ask, Host};
use crate::engine::stand_in::{self, Seat};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{self, candidates_for, families, seats};
use crate::prompts::review::REVIEW;
use crate::state::RunState;
use crate::state::cards::CardRecord;
use crate::workflow::kinds::item_card;
use crate::workflow::{StepCall, StepDone, authors, runner};

pub mod adjudicate;
pub mod diff_cut;
pub mod endpoint_tests;
pub mod reviewers;
mod together;

/// What a card's review came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    /// The comments go back to the owner's session.
    ChangesRequested {
        comments: Vec<String>,
    },
    /// A reviewer or the adjudicator rejected the approach, or review couldn't happen: you or
    /// your rules decide.
    Escalated {
        reasons: Vec<String>,
    },
}

impl Verdict {
    pub fn line(&self) -> String {
        match self {
            Self::Approved => "Approved".to_owned(),
            Self::ChangesRequested { comments } => {
                format!("Changes requested ({} comments)", comments.len())
            }
            Self::Escalated { reasons } => format!("Needs you: {}", reasons.join("; ")),
        }
    }

    /// What the reviewers said, one finding after another on one line, for the reason a task
    /// stops with. Empty for an approval.
    pub fn findings(&self) -> String {
        let said: &[String] = match self {
            Self::Approved => &[],
            Self::ChangesRequested { comments } => comments,
            Self::Escalated { reasons } => reasons,
        };
        said.iter()
            .map(|finding| finding.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|finding| !finding.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// `what`, then the reviewers' findings after a colon when they said any.
pub(crate) fn with_findings(what: String, verdict: &Verdict) -> String {
    match verdict.findings() {
        findings if findings.is_empty() => what,
        findings => format!("{what}: {findings}"),
    }
}

/// `review`: a card's review inside `build`, or a review of the step's input against criteria.
pub async fn review_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    match card_of(host, call) {
        Some(card) => {
            let excluded = excluded_families(host, call);
            let round = host.read(|state| step_round(state, call, &card));
            let verdict = review_card_among(host, &card, round, &excluded).await?;
            Ok(StepDone::done(verdict.line()))
        }
        None => review_input(host, call).await,
    }
}

/// The card a step works on, when its `for_each` item is a card spec or a card's id.
pub(crate) fn card_of(host: &Host, call: &StepCall) -> Option<CardId> {
    let card = item_card(call.item.as_ref()?)?;
    host.read(|state| state.cards.get(&card).is_some())
        .then_some(card)
}

/// The card's own review round for a workflow `review` step: the round after its newest, or
/// that newest one again when this step round already began asking for it.
fn step_round(state: &RunState, call: &StepCall, card: &CardId) -> u32 {
    let newest = state.cards.get(card).map_or(0, |record| {
        record
            .reviews
            .iter()
            .filter_map(|event| match event {
                ReviewEvent::Reviewed { round, .. } => Some(*round),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    });
    let began = state
        .workflows
        .by_id
        .get(call.workflow())
        .and_then(|record| record.round(&call.at.step, call.at.round))
        .is_some_and(|progress| {
            state.sessions.by_key.values().any(|session| {
                session.card.as_ref() == Some(card)
                    && session.started.role == RoleName::Reviewer
                    && session.since > progress.since
            })
        });
    if began && newest > 0 {
        newest
    } else {
        newest + 1
    }
}

/// Reviews a card that passed its gates: the card's reviewers, then an adjudicator when they
/// disagree. `round` counts change rounds from 1.
pub async fn review_card(host: &Host, card: &CardId, round: u32) -> EngineResult<Verdict> {
    review_card_among(host, card, round, &[]).await
}

/// `review_card` without reviewers of the `excluded` families, besides the owner's.
async fn review_card_among(
    host: &Host,
    card: &CardId,
    round: u32,
    excluded: &[String],
) -> EngineResult<Verdict> {
    let family = families(host);
    let mut reviewers = host.read(|state| reviewers::for_card(state, card, &family));
    reviewers.retain(|reviewer| !excluded.contains(&family(reviewer)));
    review_by(host, card, round, &reviewers).await
}

/// Reviews the card's work with exactly these reviewers, even one that owns the card: the
/// model a quality debt waits for, say. `round` must be one no other review of the card used.
pub async fn review_by(
    host: &Host,
    card: &CardId,
    round: u32,
    reviewers: &[WorkerRef],
) -> EngineResult<Verdict> {
    review_in(host, card, round, reviewers, None).await
}

/// Reviews a race attempt's work in `tree` before it becomes the card's. An attempt is never
/// adjudicated: it passes only when every reviewer approves, since the other attempts compete.
pub async fn review_attempt(
    host: &Host,
    card: &CardId,
    round: u32,
    reviewers: &[WorkerRef],
    tree: &Path,
) -> EngineResult<Verdict> {
    review_in(host, card, round, reviewers, Some(tree)).await
}

/// The card's work in `tree`, or in its own worktree when `tree` is `None`.
async fn review_in(
    host: &Host,
    card: &CardId,
    round: u32,
    reviewers: &[WorkerRef],
    tree: Option<&Path>,
) -> EngineResult<Verdict> {
    let (record, goal, decisions, tests) = host.read(|state| {
        (
            state.cards.get(card).cloned(),
            context::goal_text(state),
            context::decision_lines(state),
            acceptance_tests(state, card),
        )
    });
    let Some(record) = record else {
        return Err(EngineError::Refused(format!(
            "{card} is not a task of this run"
        )));
    };
    if reviewers.is_empty() {
        return Ok(Verdict::Escalated {
            reasons: vec!["No reviewer from another provider than the worker".to_owned()],
        });
    }
    let workdir = tree
        .map(Path::to_path_buf)
        .or_else(|| record.worktree.clone());
    let settings = host.settings();
    let blind = settings.review.blind
        || (reviewers.len() > 1 && settings.visibility.reviews_hidden_until_adjudicated);
    let mut reviews = host.read(|state| reviews_of_round(state, &record, round, reviewers));
    // A stand-in may not come from the owner's company, as no reviewer may.
    let owner = record.owner.clone().or_else(|| record.spec.owner.clone());
    let (chunks, cut_note) = diff_cut::cut(
        &work_diff(host, workdir.as_deref(), record.base.as_deref()).await?,
        settings.review.max_diff_lines,
    );
    let mut diff = chunks.concat();
    if let Some(note) = cut_note {
        diff.push_str(&format!("\n{note}\n"));
    }
    let input = ReviewInput {
        goal,
        card: format!("{}\n\n{}", record.spec.title, record.spec.task),
        diff,
        gates: record
            .gates
            .iter()
            .map(|gate| {
                let result = if gate.passed { "passed" } else { "failed" };
                format!("{} {result}: {}", gate.name, gate.summary)
            })
            .collect(),
        acceptance_tests: tests,
        decisions,
        round,
    };
    let at = host.read(|state| runner::review_at(state, card));
    let ask_of = |reviewer: &WorkerRef| {
        let mut ask = Ask::new(
            at.clone(),
            RoleName::Reviewer,
            reviewer.clone(),
            REVIEW,
            input.clone(),
        );
        ask.card = Some(card.clone());
        ask.blind = blind;
        ask.slot = round;
        ask.independent_of = owner.iter().cloned().collect();
        if is_endpoint(host, reviewer) {
            ask.access = Access::NoTools;
        } else {
            ask.access = Access::ReadOnly;
            ask.workdir = workdir.clone();
        }
        ask
    };
    // A reviewer's seat, which fallbacks that take it together must fit as one alone would.
    let seat = Seat {
        at: at.clone(),
        role: RoleName::Reviewer,
        step: Some(Review::NAME),
        card: Some(card.clone()),
        independent_of: owner.iter().cloned().collect(),
        isolated: blind,
    };
    // Each reviewer has its own seat before any is asked, so none stands in for another too.
    for reviewer in reviewers {
        stand_in::hold(host, &seat, reviewer);
    }
    // Who is asked, and in whose seat: each reviewer still to review, or several fallbacks
    // at once in the seat of one that's out already, and those that join a fallback that
    // took a seat midway before the step was entered again.
    let mut asked: Vec<(WorkerRef, WorkerRef)> = Vec::new();
    for reviewer in reviewers {
        match reviews.iter().find(|(seat, _, _)| seat == reviewer) {
            None => {
                let by = together::from_the_start(host, &seat, reviewer).await;
                if by.len() < 2 {
                    asked.push((reviewer.clone(), reviewer.clone()));
                } else {
                    together::say_who_reviews(host, card, round, reviewer, &by, &[])?;
                    asked.extend(by.into_iter().map(|by| (reviewer.clone(), by)));
                }
            }
            Some((_, first, _)) if first != reviewer => {
                let more = together::joining(host, &seat, reviewer, first);
                together::say_who_reviews(host, card, round, reviewer, &more, &[first])?;
                asked.extend(more.into_iter().map(|by| (reviewer.clone(), by)));
            }
            Some(_) => {}
        }
    }
    let mut stalls = Vec::new();
    while !asked.is_empty() {
        let answers = join_all(asked.iter().map(|(reviewer, by)| {
            // A fallback asked in a seat reviews under its own name, so entered again its
            // review is found by that name.
            let given = (by != reviewer)
                .then(|| together::given(host, card, round, by))
                .flatten();
            let ask = ask_of(by);
            async move {
                let answer = match given {
                    Some(review) => Ok((None, review)),
                    None => host.ask::<Review>(ask).await.map(|answered| {
                        let review = answered.output.clone();
                        (Some(answered), review)
                    }),
                };
                (reviewer, by, answer)
            }
        }))
        .await;
        let mut joining = Vec::new();
        for (reviewer, by, answer) in answers {
            match answer {
                Ok((None, review)) => reviews.push((reviewer.clone(), by.clone(), review)),
                Ok((Some(answered), review)) => {
                    // Under the name of whoever reviewed, a stand-in included.
                    host.record(
                        Some(card),
                        Some(&answered.session),
                        RunEventKind::Review(ReviewEvent::Reviewed {
                            reviewer: answered.worker.clone(),
                            blind,
                            round,
                            review: answered.output,
                        }),
                    )?;
                    // A fallback took the reviewer's own seat midway, so the rest of its
                    // fallbacks join it.
                    if by == reviewer && answered.worker != *reviewer {
                        let first = &answered.worker;
                        let more = together::joining(host, &seat, reviewer, first);
                        together::say_who_reviews(host, card, round, reviewer, &more, &[first])?;
                        joining.extend(more.into_iter().map(|by| (reviewer.clone(), by)));
                    }
                    reviews.push((reviewer.clone(), answered.worker, review));
                }
                Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
                // Nobody else may take that reviewer's seat, so the review goes on without it.
                Err(EngineError::Stalled(stall)) => stalls.push(stall),
                Err(error) => {
                    return Ok(Verdict::Escalated {
                        reasons: vec![format!("{by}'s review failed: {error}")],
                    });
                }
            }
        }
        asked = joining;
    }
    if reviews.is_empty()
        && let Some(stall) = stalls.first()
    {
        return Err(EngineError::Stalled(stall.clone()));
    }
    for stall in stalls {
        let text = format!("{} The review goes on without that reviewer.", stall.line);
        context::warn(host, Some(card), text)?;
    }
    let outputs = together::counted_by_seat(host, reviews);
    if tree.is_some() {
        return Ok(strictest(&outputs));
    }
    if outputs.len() > 1 && !agree(&outputs) {
        return adjudicate::adjudicate(host, card).await;
    }
    Ok(combine(&outputs))
}

/// Reviews these reviewers already recorded for this round, so a re-entered review asks only
/// the rest: whose seat, who reviewed, and the review. A stand-in's review counts for the
/// reviewer whose seat it took.
fn reviews_of_round(
    state: &RunState,
    record: &CardRecord,
    round: u32,
    reviewers: &[WorkerRef],
) -> Vec<(WorkerRef, WorkerRef, ReviewOutput)> {
    record
        .reviews
        .iter()
        .filter_map(|event| match event {
            ReviewEvent::Reviewed {
                reviewer,
                round: r,
                review,
                ..
            } if *r == round => {
                let seat = if reviewers.contains(reviewer) {
                    Some(reviewer.clone())
                } else {
                    stood_in_for(state, &record.spec.id, reviewer, reviewers)
                };
                seat.map(|seat| (seat, reviewer.clone(), review.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The reviewer of `reviewers` whose seat on `card` `worker` took, its newest.
fn stood_in_for(
    state: &RunState,
    card: &CardId,
    worker: &WorkerRef,
    reviewers: &[WorkerRef],
) -> Option<WorkerRef> {
    state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .filter(|session| {
            session.card.as_ref() == Some(card)
                && session.started.role == RoleName::Reviewer
                && session.started.worker == *worker
        })
        .map(|session| session.asked_worker().clone())
        .find(|asked| asked != worker && reviewers.contains(asked))
}

pub(crate) fn agree(reviews: &[ReviewOutput]) -> bool {
    reviews
        .windows(2)
        .all(|pair| pair[0].verdict == pair[1].verdict)
}

/// The verdict of reviewers who agree.
fn combine(reviews: &[ReviewOutput]) -> Verdict {
    match reviews.first().map(|r| r.verdict) {
        Some(ReviewVerdict::Approve) | None => Verdict::Approved,
        Some(ReviewVerdict::RequestChanges) => Verdict::ChangesRequested {
            comments: reviews.iter().flat_map(comment_lines).collect(),
        },
        Some(ReviewVerdict::Reject) => Verdict::Escalated {
            reasons: reviews.iter().map(|r| r.summary.clone()).collect(),
        },
    }
}

/// The most severe verdict among `reviews`: one rejection escalates, one request for changes
/// sends the work back with every such comment.
fn strictest(reviews: &[ReviewOutput]) -> Verdict {
    let with = |verdict| reviews.iter().filter(move |r| r.verdict == verdict);
    if with(ReviewVerdict::Reject).next().is_some() {
        return Verdict::Escalated {
            reasons: with(ReviewVerdict::Reject)
                .map(|r| r.summary.clone())
                .collect(),
        };
    }
    if with(ReviewVerdict::RequestChanges).next().is_some() {
        return Verdict::ChangesRequested {
            comments: with(ReviewVerdict::RequestChanges)
                .flat_map(comment_lines)
                .collect(),
        };
    }
    Verdict::Approved
}

fn comment_lines(review: &ReviewOutput) -> Vec<String> {
    let mut lines = vec![review.summary.clone()];
    lines.extend(review.comments.iter().map(|c| match c.line {
        Some(line) => format!("{}:{line}: {}", c.path, c.text),
        None => format!("{}: {}", c.path, c.text),
    }));
    lines
}

/// Paths of the acceptance tests written for `card`.
fn acceptance_tests(state: &RunState, card: &CardId) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for record in state.cards.by_id.values() {
        let CardKind::AcceptanceTests { for_card } = &record.spec.kind else {
            continue;
        };
        if for_card != card {
            continue;
        }
        let written = record.reviews.iter().rev().find_map(|event| match event {
            ReviewEvent::TestsWritten { files, .. } => Some(files.clone()),
            _ => None,
        });
        match written {
            Some(files) => paths.extend(files),
            None => paths.extend(record.spec.owned_paths.iter().cloned()),
        }
    }
    paths
}

/// The work in `tree` against the card's base, computed by the conductor.
async fn work_diff(host: &Host, tree: Option<&Path>, base: Option<&str>) -> EngineResult<String> {
    match (tree, base) {
        (Some(tree), Some(base)) => Ok(host.git().review_diff(tree, base, "HEAD").await?),
        _ => Ok(String::new()),
    }
}

pub(crate) fn is_endpoint(host: &Host, worker: &WorkerRef) -> bool {
    host.services()
        .workers
        .get(worker)
        .is_ok_and(|w| w.kind() == WorkerKind::Endpoint)
}

/// A review of the step's input, outside a card: reviewers from another family than the
/// step's `different_family_from`, blind when the step says so.
async fn review_input(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let settings = host.settings();
    let family = families(host);
    let excluded = excluded_families(host, call);
    let seats = seats(
        call,
        settings,
        RoleName::Reviewer,
        Some(settings.review.per_task.max(1)),
    );
    let reviewers: Vec<WorkerRef> = candidates_for(call, settings, RoleName::Reviewer)
        .into_iter()
        .filter(|w| !excluded.contains(&family(w)))
        .take(seats.unwrap_or(1) as usize)
        .collect();
    if reviewers.is_empty() {
        return Ok(StepDone::skipped(
            "No reviewer from another provider is available",
        ));
    }
    let blind = call.step.blind || settings.review.blind;
    let text = context::step_input(host, call).await?;
    let (chunks, cut_note) = diff_cut::cut(&text, settings.review.max_diff_lines);
    let mut target = chunks.concat();
    if let Some(note) = cut_note {
        target.push_str(&format!("\n{note}\n"));
    }
    let input = host.read(|state| ReviewInput {
        goal: context::goal_text(state),
        card: call
            .step
            .task
            .clone()
            .unwrap_or_else(|| "Review the input below against the goal.".to_owned()),
        diff: target,
        gates: Vec::new(),
        acceptance_tests: Vec::new(),
        decisions: context::decision_lines(state),
        round: call.at.round,
    });
    // A stand-in may not come from the company of whoever made what's reviewed.
    let authors = host.read(|state| authors::excluded(state, settings, call));
    let asks = reviewers.iter().enumerate().map(|(slot, reviewer)| {
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Reviewer,
            reviewer.clone(),
            REVIEW,
            input.clone(),
        );
        ask.blind = blind;
        ask.slot = slot as u32;
        ask.independent_of = authors.clone();
        ask.access = if is_endpoint(host, reviewer) {
            Access::NoTools
        } else {
            Access::ReadOnly
        };
        async move { (reviewer, host.ask::<Review>(ask).await) }
    });
    let mut outputs = Vec::new();
    let mut stalls = Vec::new();
    for (reviewer, answer) in join_all(asks).await {
        match answer {
            Ok(answered) => {
                host.record(
                    None,
                    Some(&answered.session),
                    RunEventKind::Review(ReviewEvent::Reviewed {
                        reviewer: answered.worker.clone(),
                        blind,
                        round: call.at.round,
                        review: answered.output.clone(),
                    }),
                )?;
                outputs.push(answered.output);
            }
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(EngineError::Stalled(stall)) => stalls.push(stall),
            Err(error) => {
                return Ok(StepDone {
                    end: argonauts_core::events::workflow::StepEnd::Failed,
                    summary: Some(format!("{reviewer}'s review failed: {error}")),
                    flow: crate::workflow::Flow::Continue,
                });
            }
        }
    }
    if outputs.is_empty()
        && let Some(stall) = stalls.first()
    {
        return Err(EngineError::Stalled(stall.clone()));
    }
    for stall in stalls {
        let text = format!("{} The review goes on without that reviewer.", stall.line);
        context::warn(host, None, text)?;
    }
    let summary = if agree(&outputs) {
        format!(
            "{} reviewers agreed: {}",
            outputs.len(),
            combine(&outputs).line()
        )
    } else {
        "The reviewers disagree".to_owned()
    };
    Ok(StepDone::done(summary))
}

/// The families `different_family_from` leaves out (`workflow::authors`).
fn excluded_families(host: &Host, call: &StepCall) -> Vec<String> {
    let workers = host.read(|state| authors::excluded(state, host.settings(), call));
    let mut excluded: Vec<String> = workers
        .iter()
        .map(|worker| crate::roles::family(host, worker))
        .collect();
    excluded.sort();
    excluded.dedup();
    excluded
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::judgment::ReviewComment;
    use argonauts_core::steps::plan::Severity;

    use super::*;

    fn review(verdict: ReviewVerdict, summary: &str) -> ReviewOutput {
        ReviewOutput {
            verdict,
            summary: summary.to_owned(),
            comments: vec![ReviewComment {
                path: "src/lib.rs".to_owned(),
                line: Some(3),
                severity: Severity::Medium,
                text: "handle the error".to_owned(),
            }],
            tamper_suspected: false,
        }
    }

    #[test]
    fn agreeing_reviews_combine() {
        let approve = [
            review(ReviewVerdict::Approve, "fine"),
            review(ReviewVerdict::Approve, "ok"),
        ];
        assert!(agree(&approve));
        assert_eq!(combine(&approve), Verdict::Approved);

        let changes = [review(ReviewVerdict::RequestChanges, "needs work")];
        assert_eq!(
            combine(&changes),
            Verdict::ChangesRequested {
                comments: vec![
                    "needs work".to_owned(),
                    "src/lib.rs:3: handle the error".to_owned()
                ]
            }
        );

        let reject = [review(ReviewVerdict::Reject, "wrong approach")];
        assert_eq!(
            combine(&reject),
            Verdict::Escalated {
                reasons: vec!["wrong approach".to_owned()]
            }
        );
    }

    #[test]
    fn different_verdicts_disagree() {
        let split = [
            review(ReviewVerdict::Approve, "fine"),
            review(ReviewVerdict::RequestChanges, "no"),
        ];
        assert!(!agree(&split));
    }

    /// A task that stops after its review says why in the reviewers' own words.
    #[test]
    fn a_stop_after_review_carries_the_reviewers_findings() {
        let asked = strictest(&[review(
            ReviewVerdict::RequestChanges,
            "The diff adds compiled\n  .pyc files.",
        )]);
        assert_eq!(
            with_findings(
                "review asked for changes again after 2 rounds".into(),
                &asked
            ),
            "review asked for changes again after 2 rounds: The diff adds compiled .pyc files.; \
             src/lib.rs:3: handle the error"
        );
        let rejected = strictest(&[review(ReviewVerdict::Reject, "Wrong approach.")]);
        assert_eq!(
            with_findings("the review sent it to you".into(), &rejected),
            "the review sent it to you: Wrong approach."
        );
        assert_eq!(
            with_findings("approved".into(), &Verdict::Approved),
            "approved"
        );
    }
}
