//! One card from Ready to Integrated: its worktree, `gates.setup` once, the owner's session,
//! a commit after every session end, gates, review, and integration. Re-entered after a crash,
//! it resumes the owner's unfinished session instead of starting another.

use std::sync::atomic::{AtomicU32, Ordering};

use tokio_util::sync::CancellationToken;

use argonauts_core::CardId;
use argonauts_core::events::card::{AttemptCause, CardEvent, CardKind, CardState};
use argonauts_core::events::session::{EndReason, SessionEnd, loaded_words};
use argonauts_core::limits::{HandoffPacket, RecoveryAction};
use argonauts_core::names::WorkerRef;
use argonauts_core::steps::card::ReportStatus;
use argonauts_core::worker::WorkerKind;

use crate::blackboard;
use crate::card::asked_anyway;
use crate::card::escalation::{self, Decision};
use crate::card::gates::{self, GateVerdict};
use crate::card::machine::{self, Trigger};
use crate::card::prompt;
use crate::card::recovery;
use crate::card::session::{self, Ended, Next, SessionCall, same_session};
use crate::card::support::{
    newest_owner_session, owner_of, progress_of, record_card, record_card_in, record_of, state_of,
    wait_until,
};
use crate::card::takeover;
use crate::card::worktree::{self, CardTree};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::goal;
use crate::goal::task::AfterTry;
use crate::handoff;
use crate::handoff::on_return::Applied;
use crate::integrate::{self, CardMerge, card_merge};
use crate::plan::context::families;
use crate::review::{self, endpoint_tests};
use crate::solo;
use crate::state::RunState;
use crate::state::cards::CardRecord;
use crate::workflow::StepAt;

/// Returns the state the card stopped in: `Integrated`, or one that waits on a decision.
pub async fn run_card(host: &Host, card: &CardId, at: &StepAt) -> EngineResult<CardState> {
    drive(host, card, at, Until::Integrated).await
}

/// Sends an integrated card back from the merge checkpoint and runs it to integration again,
/// resuming its owner with `note` as a round of review changes.
pub async fn send_back(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    note: &str,
) -> EngineResult<CardState> {
    transition(host, card, Trigger::SentBack, note)?;
    let text = format!(
        "The merge approval sent this task back for changes: {note}\nMake them, then end \
         with your report again."
    );
    let parts = vec![prompt::message_part("merge approval", text)];
    // A card you finished yourself has no owner session to resume.
    let next = if host.read(|state| newest_owner_session(state, card).is_some()) {
        Next::resume(AttemptCause::ReviewChanges, parts)
    } else {
        Next::Fresh {
            cause: AttemptCause::ReviewChanges,
            why: note.to_owned(),
            packet: None,
            extra: parts,
        }
    };
    drive_from(host, card, at, Until::Integrated, Some(Carry::Next(next))).await
}

/// A workflow's own review step sent the task back: its worker goes again with `note`, as far
/// as its checks. The workflow reviews it again itself, so no review of the engine's runs.
pub(crate) async fn revise(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    note: &str,
) -> EngineResult<CardState> {
    transition(
        host,
        card,
        Trigger::ChangesRequested,
        "the workflow's review sent it back",
    )?;
    let parts = vec![prompt::message_part("review", note)];
    let next = Next::resume(AttemptCause::ReviewChanges, parts);
    drive_from(host, card, at, Until::GatesPassed, Some(Carry::Next(next))).await
}

/// Gives an integrated card to a new owner from the merge checkpoint, who starts fresh with
/// `why`, and runs it to integration again.
pub async fn hand_back(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    why: &str,
) -> EngineResult<CardState> {
    transition(host, card, Trigger::SentBack, why)?;
    let next = Next::fresh(AttemptCause::Reassigned, why);
    drive_from(host, card, at, Until::Integrated, Some(Carry::Next(next))).await
}

/// You took an integrated card over from the merge checkpoint. Once you're done, it runs to
/// integration again.
pub async fn take_over_back(host: &Host, card: &CardId, at: &StepAt) -> EngineResult<CardState> {
    transition(
        host,
        card,
        Trigger::SentBack,
        "you took it over at the merge approval",
    )?;
    takeover::take_over(host, card)
        .await
        .map_err(EngineError::Refused)?;
    run_card_back(host, card, at).await
}

/// [`run_card`] for a card sent back from the merge checkpoint, which waits for you first when
/// you took it over.
pub async fn run_card_back(host: &Host, card: &CardId, at: &StepAt) -> EngineResult<CardState> {
    if state_of(host, card)? == CardState::TakenOver {
        wait_until(host, |state| {
            state.cards.progress(card).take_over_end.is_some()
                || state
                    .cards
                    .get(card)
                    .is_none_or(|record| record.state != CardState::TakenOver)
        })
        .await?;
    }
    run_card(host, card, at).await
}

/// Moves the card by the table and records `Card::State`. A move the table doesn't allow is
/// refused.
pub fn transition(
    host: &Host,
    card: &CardId,
    trigger: Trigger,
    reason: &str,
) -> EngineResult<CardState> {
    let from = state_of(host, card)?;
    let to = machine::next(from, trigger).ok_or_else(|| {
        EngineError::Refused(format!("{card} can't move from {from:?} on {trigger:?}"))
    })?;
    let event = CardEvent::State {
        from: Some(from),
        to,
        reason: reason.to_owned(),
    };
    record_card(host, card, event)?;
    Ok(to)
}

/// How one owner's attempt at a card ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptEnd {
    /// Its gates passed, and its review approved it.
    Passed,
    Failed {
        why: String,
    },
    Stopped,
}

/// One candidate's attempt at a card in a tree of its own, such as a race's.
pub(crate) struct AttemptCall<'a> {
    pub card: &'a CardId,
    pub owner: &'a WorkerRef,
    pub tree: &'a CardTree,
    pub at: &'a StepAt,
    /// Its first session's slot: how many owner sessions of this worker the card had in this
    /// step's round before the attempt, which is where replay finds the attempt's sessions.
    pub first_slot: u32,
    /// What its first session is told.
    pub packet: &'a HandoffPacket,
    /// Review rounds, shared by a race's attempts so no two reviews of the card share one.
    pub rounds: &'a AtomicU32,
    /// Cancelled once another attempt won: no new session, gate run, or review starts.
    pub stop: &'a CancellationToken,
}

/// One candidate's attempt in its own tree, through its gates (one resume with the gate
/// output) and its review (a resume per change round), without integrating and without
/// moving the card. The card's own attempts run in [`run_card`], which reads the owner from
/// the state at each attempt, so a reassignment takes effect at the next one.
pub(crate) async fn run_attempt(host: &Host, call: AttemptCall<'_>) -> EngineResult<AttemptEnd> {
    let AttemptCall {
        card,
        owner,
        tree,
        at,
        first_slot,
        packet,
        rounds,
        stop,
    } = call;
    let mut next = Next::Fresh {
        cause: AttemptCause::Handoff,
        why: "another agent tries it on its own branch".to_owned(),
        packet: Some(Box::new(packet.clone())),
        extra: Vec::new(),
    };
    let mut resume = None;
    let mut gate_retried = false;
    let mut change_rounds = 0;
    let mut slot = first_slot;
    loop {
        if stop.is_cancelled() {
            return Ok(AttemptEnd::Stopped);
        }
        let call = SessionCall {
            card,
            owner,
            tree,
            at,
            next,
            resume: resume.clone(),
            slot: Some(slot),
        };
        let ended = session::run(host, call).await?;
        slot += 1;
        resume = Some(ended.key.clone());
        match (&ended.end.reason, &ended.report) {
            (EndReason::Completed, Some(report)) if report.status == ReportStatus::Done => {}
            (EndReason::Completed, Some(report)) => {
                let why = format!("it reported {:?}: {}", report.status, report.summary);
                return Ok(AttemptEnd::Failed { why });
            }
            (EndReason::Completed, None) => {
                if let Some(decide) = asked_anyway::reply(host, card, owner, &ended)? {
                    next = Next::resume(AttemptCause::Resumed, vec![decide]);
                    continue;
                }
                let why = "it ended without a report that matches the schema".to_owned();
                return Ok(AttemptEnd::Failed { why });
            }
            (EndReason::Stopped | EndReason::Paused, _) => return Ok(AttemptEnd::Stopped),
            (reason, _) => {
                let why = format!("its session ended: {}", end_text(reason));
                return Ok(AttemptEnd::Failed { why });
            }
        }
        if stop.is_cancelled() {
            return Ok(AttemptEnd::Stopped);
        }
        match gates::run_in(host, card, tree, ended.report.as_ref()).await? {
            GateVerdict::Passed => {}
            GateVerdict::Failed { output } if !gate_retried => {
                gate_retried = true;
                let parts = vec![prompt::gate_output_part(&output)];
                next = Next::resume(AttemptCause::GateRetry, parts);
                continue;
            }
            GateVerdict::Failed { output } => return Ok(AttemptEnd::Failed { why: output }),
            GateVerdict::Stopped { reason } => return Ok(AttemptEnd::Failed { why: reason }),
        }
        if stop.is_cancelled() {
            return Ok(AttemptEnd::Stopped);
        }
        let record = record_of(host, card)?;
        if !needs_review(host, &record) {
            return Ok(AttemptEnd::Passed);
        }
        let reviewers =
            review::reviewers::eligible(&record.spec.reviewers, Some(owner), &families(host));
        let round = rounds.fetch_add(1, Ordering::SeqCst);
        match review::review_attempt(host, card, round, &reviewers, &tree.path).await? {
            review::Verdict::Approved => return Ok(AttemptEnd::Passed),
            review::Verdict::ChangesRequested { comments }
                if change_rounds < host.settings().review.max_change_rounds =>
            {
                change_rounds += 1;
                gate_retried = false;
                let parts = vec![prompt::review_part(change_rounds, &comments)];
                next = Next::resume(AttemptCause::ReviewChanges, parts);
            }
            review::Verdict::ChangesRequested { .. } => {
                let why = format!("review asked for changes again after {change_rounds} rounds");
                return Ok(AttemptEnd::Failed { why });
            }
            review::Verdict::Escalated { reasons } => {
                let why = format!("its review sent it to you: {}", reasons.join("; "));
                return Ok(AttemptEnd::Failed { why });
            }
        }
    }
}

/// How far [`drive`] takes a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Until {
    Integrated,
    /// Stop once the gates pass, before review: best of n keeps one such attempt.
    GatesPassed,
}

/// What one state's handler hands the next, within this process.
#[derive(Debug, Clone, PartialEq)]
enum Carry {
    Next(Next),
    /// The recovery action decided for a session that broke.
    Action(RecoveryAction),
    /// Why the owner stopped for a decision.
    Ask(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Moved,
    /// Nothing more can happen until something outside changes.
    Wait,
}

/// Runs the card's state machine until it's integrated (or, for best of n, its gates pass) or
/// waits on something outside it.
pub(crate) async fn drive(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    until: Until,
) -> EngineResult<CardState> {
    drive_from(host, card, at, until, None).await
}

async fn drive_from(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    until: Until,
    mut carry: Option<Carry>,
) -> EngineResult<CardState> {
    loop {
        if until == Until::GatesPassed && state_of(host, card)? == CardState::InReview {
            return Ok(CardState::InReview);
        }
        match handoff::on_return::apply_pending(host, card).await? {
            Applied::Nothing => {}
            Applied::Moved => carry = None,
            Applied::NewOwner { why } => {
                carry = Some(Carry::Next(Next::fresh(AttemptCause::Reassigned, why)));
            }
            Applied::Changes { round, comments } => {
                let parts = vec![prompt::review_part(round, &comments)];
                carry = Some(Carry::Next(Next::resume(
                    AttemptCause::ReviewChanges,
                    parts,
                )));
            }
        }
        let step = match state_of(host, card)? {
            CardState::Waiting => waiting(host, card)?,
            CardState::Ready => {
                transition(host, card, Trigger::Start, "worker, slot, and limit free")?;
                Step::Moved
            }
            CardState::Running => running(host, card, at, &mut carry).await?,
            CardState::Gating => gating(host, card, at, &mut carry).await?,
            CardState::InReview => reviewing(host, card, &mut carry).await?,
            CardState::Integrating => integrating(host, card, &mut carry).await?,
            CardState::Failed => failed(host, card, &mut carry).await?,
            CardState::Escalated => escalated(host, card, at, &mut carry).await?,
            CardState::NeedsDecision => needs_decision(host, card, at, &mut carry).await?,
            CardState::TakenOver => taken_over(host, card, at, &mut carry).await?,
            CardState::Blocked
            | CardState::Deferred
            | CardState::Integrated
            | CardState::Cancelled => Step::Wait,
        };
        if step == Step::Wait {
            return state_of(host, card);
        }
    }
}

/// Every dependency is integrated, or was dropped.
pub(crate) fn dependencies_done(state: &RunState, card: &CardId) -> bool {
    let Some(record) = state.cards.get(card) else {
        return false;
    };
    record.spec.depends_on.iter().all(|dep| {
        state
            .cards
            .get(dep)
            .is_some_and(|d| matches!(d.state, CardState::Integrated | CardState::Cancelled))
    })
}

fn waiting(host: &Host, card: &CardId) -> EngineResult<Step> {
    if !host.read(|state| dependencies_done(state, card)) {
        return Ok(Step::Wait);
    }
    let reason = "its dependencies are merged into the run";
    transition(host, card, Trigger::DependenciesIntegrated, reason)?;
    Ok(Step::Moved)
}

async fn running(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    let record = record_of(host, card)?;
    let owner = owner_of(host, &record);
    if writes_tests_as_output(host, &record, &owner) {
        endpoint_tests::write_tests(host, card).await?;
        if state_of(host, card)? == CardState::Running {
            let reason = "the reviewer's acceptance tests are written";
            transition(host, card, Trigger::ReportedDone, reason)?;
        }
        return Ok(Step::Moved);
    }
    let tree = worktree::ensure(host, card).await?;
    // Only the owner's own session can be resumed, and only when it's the card's newest: after
    // someone else worked on the card, its old session knows nothing of that work.
    let resume = host.read(|state| {
        newest_owner_session(state, card)
            .filter(|session| session.started.worker == owner)
            .map(|session| session.key.clone())
    });
    let (next, open) = next_session(host, card, &owner, carry.take());
    let next = match next {
        Next::Resume { parts, .. } if resume.is_none() => Next::Fresh {
            cause: AttemptCause::Reassigned,
            why: "the task has a new worker".to_owned(),
            packet: None,
            extra: parts,
        },
        next => next,
    };
    // A task worked as a goal takes its goal as it starts, and a fresh session is told of it.
    let next = match (next, goal::task::brief(host, card, &owner)?) {
        (
            Next::Fresh {
                cause,
                why,
                packet,
                mut extra,
            },
            Some(told),
        ) => {
            extra.push(told);
            Next::Fresh {
                cause,
                why,
                packet,
                extra,
            }
        }
        (next, _) => next,
    };
    if !open {
        let cause = next.cause();
        let attempt = if same_session(cause) {
            record.attempts.max(1)
        } else {
            record.attempts + 1
        };
        let event = CardEvent::Attempt {
            attempt,
            worker: owner.clone(),
            cause,
        };
        record_card(host, card, event)?;
    }
    let call = SessionCall {
        card,
        owner: &owner,
        tree: &tree,
        at,
        next,
        resume,
        slot: None,
    };
    let ended = session::run(host, call).await?;
    after_session(host, card, &owner, ended, carry).await
}

/// An acceptance-test card owned by an endpoint reviewer: the tests come back as the reviewer's
/// output and the conductor writes them, since an endpoint has no tools.
fn writes_tests_as_output(host: &Host, record: &CardRecord, owner: &WorkerRef) -> bool {
    matches!(record.spec.kind, CardKind::AcceptanceTests { .. })
        && host
            .services()
            .workers
            .get(owner)
            .is_ok_and(|worker| worker.kind() == WorkerKind::Endpoint)
}

/// The next owner session, and whether it continues an attempt already recorded (after a
/// restart) instead of starting a new one.
fn next_session(
    host: &Host,
    card: &CardId,
    owner: &WorkerRef,
    carry: Option<Carry>,
) -> (Next, bool) {
    let (open, cause, newest_worker) = host.read(|state| {
        let newest = newest_owner_session(state, card);
        let progress = state.cards.progress(card);
        let open = progress.attempt_seq.is_some_and(|seq| {
            newest.is_none_or(|session| session.since < seq || session.end.is_none())
        });
        let cause = progress.last_cause.unwrap_or(AttemptCause::First);
        (
            open,
            cause,
            newest.map(|session| session.started.worker.clone()),
        )
    });
    let carried = match carry {
        Some(Carry::Next(next)) => Some(next),
        _ => None,
    };
    if open {
        let next = carried
            .filter(|next| next.cause() == cause)
            .unwrap_or_else(|| after_restart(cause));
        return (next, true);
    }
    if let Some(next) = carried {
        return (next, false);
    }
    let next = match newest_worker {
        None => Next::fresh(AttemptCause::First, ""),
        Some(worker) if worker != *owner => {
            Next::fresh(AttemptCause::Reassigned, "the task has a new worker")
        }
        Some(_) => Next::resume(AttemptCause::Resumed, vec![continue_part()]),
    };
    (next, false)
}

fn after_restart(cause: AttemptCause) -> Next {
    if same_session(cause) {
        Next::resume(cause, vec![continue_part()])
    } else if cause == AttemptCause::First {
        Next::fresh(cause, "")
    } else {
        Next::fresh(cause, "the run restarted")
    }
}

fn continue_part() -> argonauts_core::events::session::InputPart {
    prompt::message_part("continue", "Continue the task where you left off.")
}

async fn after_session(
    host: &Host,
    card: &CardId,
    owner: &WorkerRef,
    ended: Ended,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    // Someone moved the card while the session ran: you took it over, stopped it, or a
    // question blocked it.
    if state_of(host, card)? != CardState::Running {
        return Ok(Step::Moved);
    }
    let now = owner_of(host, &record_of(host, card)?);
    if now != *owner {
        let why = format!("you reassigned it from {owner} to {now}");
        *carry = Some(Carry::Next(Next::fresh(AttemptCause::Reassigned, why)));
        return Ok(Step::Moved);
    }
    match &ended.end.reason {
        EndReason::Completed => completed(host, card, owner, &ended, carry).await,
        EndReason::Paused => {
            host.admitted().await?;
            let part = prompt::message_part(
                "resumed",
                "The run was paused and has resumed. Continue where you left off.",
            );
            *carry = Some(Carry::Next(Next::resume(AttemptCause::Resumed, vec![part])));
            Ok(Step::Moved)
        }
        reason => {
            match recovery::decide(host, card, &ended.end).await? {
                RecoveryAction::ResumeSame { message } => {
                    let parts = vec![prompt::message_part("message", message)];
                    *carry = Some(Carry::Next(Next::resume(AttemptCause::Resumed, parts)));
                }
                RecoveryAction::Rotate { packet } => {
                    *carry = Some(Carry::Next(Next::Fresh {
                        cause: AttemptCause::Rotation,
                        why: "its context was full".to_owned(),
                        packet: Some(packet),
                        extra: Vec::new(),
                    }));
                }
                action if recovery::is_failure(reason) => {
                    transition(host, card, Trigger::Broke, &end_text(reason))?;
                    *carry = Some(Carry::Action(action));
                }
                action => recovery::apply(host, card, action).await?,
            }
            Ok(Step::Moved)
        }
    }
}

async fn completed(
    host: &Host,
    card: &CardId,
    owner: &WorkerRef,
    ended: &Ended,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    let Some(report) = ended.report.clone() else {
        // A turn that ended with a question goes on in its own session, told to decide.
        if let Some(decide) = asked_anyway::reply(host, card, owner, ended)? {
            let next = Next::resume(AttemptCause::Resumed, vec![decide]);
            *carry = Some(Carry::Next(next));
            return Ok(Step::Moved);
        }
        let reason = "the session ended without a report that matches the schema";
        transition(host, card, Trigger::Broke, reason)?;
        return Ok(Step::Moved);
    };
    let reported = CardEvent::Reported(Box::new(report.clone()));
    record_card_in(host, card, &ended.key, reported)?;
    asked_anyway::on_report(host, card, &ended.key, &report)?;
    blackboard::on_report(host, card, &report).await?;
    if state_of(host, card)? != CardState::Running {
        return Ok(Step::Moved);
    }
    let carries_on = goal::task::after_report(host, card, &report)?;
    match report.status {
        // Its CLI keeps the goal and says it's still at it: the same session goes on.
        ReportStatus::Done if carries_on.is_some() => {
            let parts = carries_on.into_iter().collect();
            *carry = Some(Carry::Next(Next::resume(AttemptCause::Resumed, parts)));
        }
        ReportStatus::Done => {
            transition(
                host,
                card,
                Trigger::ReportedDone,
                "the worker reported done",
            )?;
        }
        ReportStatus::Blocked => {
            let reason = "the worker asked a question and waits for the answer";
            transition(host, card, Trigger::Question, reason)?;
        }
        ReportStatus::NeedsDecision => {
            let reason = "the worker needs a decision";
            transition(host, card, Trigger::NeedsDecision, reason)?;
            *carry = Some(Carry::Ask(report.summary.clone()));
        }
        ReportStatus::Failed => {
            transition(
                host,
                card,
                Trigger::Broke,
                "the worker reported that it failed",
            )?;
        }
    }
    Ok(Step::Moved)
}

async fn gating(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    let verdict = gates::run(host, card).await?;
    // A task worked as a goal is checked against its lines after every try, and what still
    // fails goes back to the same session, with no fixed number of tries.
    if !matches!(verdict, GateVerdict::Stopped { .. })
        && host.read(|state| goal::task::pursued(state, card))
    {
        let passed = verdict == GateVerdict::Passed;
        if let AfterTry::Again(told) = goal::task::after_try(host, card, at, passed).await? {
            let reason = "the goal doesn't hold yet; the same session gets what still fails";
            transition(host, card, Trigger::GoalNotMet, reason)?;
            *carry = Some(Carry::Next(Next::resume(
                AttemptCause::GoalRetry,
                vec![told],
            )));
            return Ok(Step::Moved);
        }
    }
    match verdict {
        GateVerdict::Passed => {
            transition(host, card, Trigger::GatesPassed, "the checks passed")?;
        }
        GateVerdict::Failed { output } => {
            if progress_of(host, card).gate_retries == 0 {
                let reason = "the checks failed; the same session gets their output";
                transition(host, card, Trigger::FirstGateFailure, reason)?;
                let parts = vec![prompt::gate_output_part(&output)];
                *carry = Some(Carry::Next(Next::resume(AttemptCause::GateRetry, parts)));
            } else {
                let reason = "the checks failed again after the resume";
                transition(host, card, Trigger::GatesFailedAgain, reason)?;
            }
        }
        GateVerdict::Stopped { reason } => {
            transition(host, card, Trigger::GatesFailedAgain, &reason)?;
            *carry = Some(Carry::Action(RecoveryAction::Escalate { reason }));
        }
    }
    Ok(Step::Moved)
}

async fn reviewing(host: &Host, card: &CardId, carry: &mut Option<Carry>) -> EngineResult<Step> {
    let record = record_of(host, card)?;
    if !needs_review(host, &record) {
        transition(
            host,
            card,
            Trigger::Approved,
            "no review is set for this task",
        )?;
        return Ok(Step::Moved);
    }
    let change_rounds = progress_of(host, card).change_rounds;
    let round = change_rounds + 1;
    let verdict = review::review_card(host, card, round).await?;
    if verdict == review::Verdict::Approved {
        transition(host, card, Trigger::Approved, "the review approved it")?;
        return Ok(Step::Moved);
    }
    let review::Verdict::ChangesRequested { comments } = &verdict else {
        // The reviewers rejected the approach or couldn't review: it's yours to decide.
        let reason = format!("the review sent it to you: {verdict:?}");
        transition(host, card, Trigger::ChangesRequestedAgain, &reason)?;
        return Ok(Step::Moved);
    };
    if change_rounds < host.settings().review.max_change_rounds {
        let reason = format!("review round {round} asked for changes");
        transition(host, card, Trigger::ChangesRequested, &reason)?;
        let parts = vec![prompt::review_part(round, comments)];
        *carry = Some(Carry::Next(Next::resume(
            AttemptCause::ReviewChanges,
            parts,
        )));
    } else {
        let reason = format!(
            "review asked for changes again after {} rounds",
            change_rounds
        );
        transition(host, card, Trigger::ChangesRequestedAgain, &reason)?;
    }
    Ok(Step::Moved)
}

/// Plain solo runs have no reviewer; every other card is reviewed by another family.
fn needs_review(host: &Host, record: &CardRecord) -> bool {
    !(record.spec.kind == CardKind::Solo
        && record.spec.reviewers.is_empty()
        && !host.settings().solo.review)
}

async fn integrating(host: &Host, card: &CardId, carry: &mut Option<Carry>) -> EngineResult<Step> {
    let merge = integrate::integrate_card(host, card).await?;
    let returns = progress_of(host, card).integration_returns;
    match merge {
        CardMerge::Integrated { sha } => {
            let short: String = sha.chars().take(8).collect();
            transition(host, card, Trigger::Merged, &format!("merged as {short}"))?;
        }
        CardMerge::TestsFailed { .. } | CardMerge::Conflict { .. }
            if returns >= INTEGRATION_RETURNS =>
        {
            let what = match &merge {
                CardMerge::Conflict { files } => {
                    format!("the merge conflicted in {}", files.join(", "))
                }
                _ => "the affected tests failed after the merge".to_owned(),
            };
            transition(host, card, Trigger::AffectedTestsFailed, &what)?;
            let reason = format!("{what}, after it came back from integration {returns} times");
            transition(host, card, Trigger::Broke, &reason)?;
        }
        CardMerge::TestsFailed { output } => {
            let reason = "the affected tests failed after the merge";
            transition(host, card, Trigger::AffectedTestsFailed, reason)?;
            let mut parts = vec![prompt::gate_output_part(&output)];
            parts.extend(worktree::left_out_part(host, card).await?);
            *carry = Some(Carry::Next(Next::resume(
                AttemptCause::IntegrationFailed,
                parts,
            )));
        }
        CardMerge::Conflict { files } => {
            card_merge::bring_in_integration(host, card).await?;
            let reason = format!("the merge conflicted in {}", files.join(", "));
            transition(host, card, Trigger::AffectedTestsFailed, &reason)?;
            let text = format!(
                "Your task conflicts with the integration branch in: {}. The conductor merged \
                 the integration branch into your worktree, and those files hold conflict \
                 markers. Resolve them so both sides' work survives, then end with your report \
                 again. Don't run git yourself.",
                files.join(", ")
            );
            let parts = vec![prompt::message_part("merge conflict", text)];
            *carry = Some(Carry::Next(Next::resume(
                AttemptCause::IntegrationFailed,
                parts,
            )));
        }
    }
    Ok(Step::Moved)
}

async fn failed(host: &Host, card: &CardId, carry: &mut Option<Carry>) -> EngineResult<Step> {
    let action = match carry.take() {
        Some(Carry::Action(action)) => action,
        _ => {
            let end =
                host.read(|state| newest_owner_session(state, card).and_then(|s| s.end.clone()));
            match end {
                Some(end) => recovery::decide(host, card, &end).await?,
                None => RecoveryAction::Proceed,
            }
        }
    };
    let why = failure_summary(&record_of(host, card)?, end_of(host, card).as_ref());
    match action {
        RecoveryAction::Escalate { reason } => {
            transition(host, card, Trigger::Escalate, &reason)?;
        }
        action @ RecoveryAction::HandOff { .. } if progress_of(host, card).ladder_retries == 0 => {
            let before = owner_of(host, &record_of(host, card)?);
            handoff::apply(host, card, action).await?;
            let record = record_of(host, card)?;
            let now = owner_of(host, &record);
            if record.state == CardState::Failed {
                if now == before {
                    ladder(host, card, &why, carry)?;
                } else {
                    let reason = format!("{why}; a fresh session on {now}");
                    transition(host, card, Trigger::Retry, &reason)?;
                    *carry = Some(Carry::Next(Next::fresh(AttemptCause::AfterFailure, why)));
                }
            }
        }
        _ => ladder(host, card, &why, carry)?,
    }
    Ok(Step::Moved)
}

/// How often the owner gets its card back from integration before the card fails and the retry
/// ladder runs. More than the gates' one resume, since other cards merging in the meantime can
/// bring new conflicts that aren't the owner's fault.
const INTEGRATION_RETURNS: u32 = 3;

/// The retry ladder: a fresh session on another family once, then escalation.
fn ladder(host: &Host, card: &CardId, why: &str, carry: &mut Option<Carry>) -> EngineResult<()> {
    if recovery::retry_on_other_provider(host, card, why)?.is_some() {
        *carry = Some(Carry::Next(Next::fresh(AttemptCause::AfterFailure, why)));
    }
    Ok(())
}

async fn escalated(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    let snapshot = host.read(Clone::clone);
    if snapshot.run.mode == Some(argonauts_core::events::run::RunMode::Solo)
        && solo::escalates_to_crew(&snapshot, host)
    {
        return Ok(Step::Wait);
    }
    let why = failure_summary(&record_of(host, card)?, end_of(host, card).as_ref());
    let from = CardState::Escalated;
    if let Decision::Moved(Some(next)) =
        escalation::decide(host, card, Some(at), from, &why).await?
    {
        *carry = Some(Carry::Next(next));
    }
    Ok(Step::Moved)
}

async fn needs_decision(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    if progress_of(host, card).waiting_for.is_some() {
        return Ok(Step::Wait);
    }
    let why = match carry.take() {
        Some(Carry::Ask(summary)) => format!("its worker needs a decision: {summary}"),
        Some(Carry::Action(action)) => return apply_and_settle(host, card, action).await,
        _ => match end_of(host, card) {
            Some(end) if end.reason == EndReason::Stopped => "you stopped its session".to_owned(),
            Some(end) if end.reason != EndReason::Completed => {
                let action = recovery::decide(host, card, &end).await?;
                if !matches!(
                    action,
                    RecoveryAction::Proceed | RecoveryAction::ResumeSame { .. }
                ) {
                    return apply_and_settle(host, card, action).await;
                }
                format!("its agent stopped: {}", end_text(&end.reason))
            }
            _ => "it needs a decision".to_owned(),
        },
    };
    let from = CardState::NeedsDecision;
    if let Decision::Moved(Some(next)) =
        escalation::decide(host, card, Some(at), from, &why).await?
    {
        *carry = Some(Carry::Next(next));
    }
    Ok(Step::Moved)
}

async fn taken_over(
    host: &Host,
    card: &CardId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    if progress_of(host, card).take_over_end.is_none() {
        return Ok(Step::Wait);
    }
    if let Decision::Moved(Some(next)) = escalation::take_over_return(host, card, Some(at)).await? {
        *carry = Some(Carry::Next(next));
    }
    Ok(Step::Moved)
}

fn end_of(host: &Host, card: &CardId) -> Option<SessionEnd> {
    host.read(|state| newest_owner_session(state, card).and_then(|s| s.end.clone()))
}

async fn apply_and_settle(
    host: &Host,
    card: &CardId,
    action: RecoveryAction,
) -> EngineResult<Step> {
    recovery::apply(host, card, action).await?;
    Ok(if state_of(host, card)? == CardState::NeedsDecision {
        Step::Wait
    } else {
        Step::Moved
    })
}

/// One line on why the card is where it is: its newest failed gate, else its newest report,
/// else how its newest session ended.
fn failure_summary(record: &CardRecord, end: Option<&SessionEnd>) -> String {
    if let Some(gate) = record.gates.iter().rev().find(|gate| !gate.passed) {
        return format!("{} failed: {}", gate.name, gate.summary);
    }
    if let Some(report) = record.reports.last() {
        return format!("its report: {}", report.summary);
    }
    match end.map(|end| &end.reason) {
        Some(EndReason::Completed) => "its agent ended without a report".to_owned(),
        Some(reason) => format!("its session ended: {}", end_text(reason)),
        None => "it failed".to_owned(),
    }
}

fn end_text(reason: &EndReason) -> String {
    match reason {
        EndReason::Completed => "completed".to_owned(),
        EndReason::BudgetHit { dimension } => format!("hit its {dimension:?} limit"),
        EndReason::Paused => "paused".to_owned(),
        EndReason::Stopped => "stopped".to_owned(),
        EndReason::Stuck { watchdog } => format!("stuck ({watchdog:?})"),
        EndReason::UsageLimit { .. } => "usage limit".to_owned(),
        EndReason::RateLimited => "rate limited".to_owned(),
        EndReason::VendorError { message, .. } => format!("agent error: {message}"),
        EndReason::Auth { message } => format!("logged out: {message}"),
        EndReason::Billing { message } => format!("billing: {message}"),
        EndReason::ContextFull => "context full".to_owned(),
        EndReason::ModelUnavailable { model } => format!(
            "{} is unavailable",
            argonauts_core::model_catalog::display_name_from_id(model)
        ),
        EndReason::AdapterBroken { detail } => format!("adapter broken: {detail}"),
        EndReason::IsolationBroken { plugins, skills } => {
            format!("isolation broken: loaded {}", loaded_words(plugins, skills))
        }
        EndReason::StartFailed { message } => format!("didn't start: {message}"),
        EndReason::Crashed { message } => format!("crashed: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::card_spec;

    fn state_with(cards: &[(&str, &[&str], CardState)]) -> RunState {
        let mut state = RunState::default();
        for (id, deps, card_state) in cards {
            let mut record = CardRecord::new(card_spec(id, deps, None));
            record.state = *card_state;
            state.cards.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    #[test]
    fn a_card_waits_until_every_dependency_is_integrated_or_dropped() {
        let id = |s: &str| CardId::new(s).unwrap();
        let state = state_with(&[
            ("task-001", &[], CardState::Integrated),
            ("task-002", &[], CardState::Running),
            ("task-003", &["task-001"], CardState::Waiting),
            ("task-004", &["task-001", "task-002"], CardState::Waiting),
            ("task-005", &["task-006"], CardState::Waiting),
            ("task-006", &[], CardState::Cancelled),
        ]);
        assert!(dependencies_done(&state, &id("task-003")));
        assert!(!dependencies_done(&state, &id("task-004")));
        assert!(dependencies_done(&state, &id("task-005")));
        assert!(!dependencies_done(&state, &id("task-404")));
    }

    #[test]
    fn failure_summary_prefers_the_newest_failed_gate() {
        let mut record = CardRecord::new(card_spec("task-001", &[], None));
        assert_eq!(failure_summary(&record, None), "it failed");
        record
            .gates
            .push(argonauts_core::events::card::GateOutcome {
                name: "test".into(),
                kind: argonauts_core::events::card::GateKind::Command,
                passed: false,
                command: None,
                exit_code: Some(1),
                duration_ms: 1,
                summary: "2 failed".into(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            });
        assert_eq!(failure_summary(&record, None), "test failed: 2 failed");
    }

    #[test]
    fn a_card_with_no_report_says_how_its_session_ended() {
        let record = CardRecord::new(card_spec("task-001", &[], None));
        let end = |reason: EndReason| SessionEnd {
            vendor_session: None,
            exit_code: Some(0),
            signal: None,
            duration_ms: 1,
            reason,
            spend: argonauts_core::Spend::default(),
            tool_calls: 0,
            result_text: None,
            structured: None,
            last_message: None,
            raw_log: None,
        };
        assert_eq!(
            failure_summary(&record, Some(&end(EndReason::Completed))),
            "its agent ended without a report"
        );
        let crashed = end(EndReason::Crashed {
            message: "exit 1".into(),
        });
        assert_eq!(
            failure_summary(&record, Some(&crashed)),
            "its session ended: crashed: exit 1"
        );
    }
}
