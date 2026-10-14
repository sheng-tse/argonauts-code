//! One task from Ready to Merged: its worktree, `checks.setup` once, the worker's session,
//! a commit after every session end, checks, review, and merge. Re-entered after a crash,
//! it resumes the worker's unfinished session instead of starting another.

use std::sync::atomic::{AtomicU32, Ordering};

use tokio_util::sync::CancellationToken;

use argonauts_core::TaskId;
use argonauts_core::events::session::{EndReason, SessionEnd, loaded_words};
use argonauts_core::events::task::{TaskEvent, TaskKind, TaskState, TryCause};
use argonauts_core::limits::{FallbackNote, RecoveryAction};
use argonauts_core::names::ModelRef;
use argonauts_core::session_runner::RunnerKind;
use argonauts_core::steps::task::ReportStatus;

use crate::blackboard;
use crate::captain;
use crate::engine::host::Host;
use crate::engine::shared::lock;
use crate::error::{EngineError, EngineResult};
use crate::fallback;
use crate::fallback::on_return::Applied;
use crate::goal;
use crate::goal::task::AfterTry;
use crate::merge::{self, TaskMerge, task_merge};
use crate::plan::context::providers;
use crate::review::{self, endpoint_tests};
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::asked_anyway;
use crate::task::checks::{self, CheckVerdict};
use crate::task::machine::{self, Trigger};
use crate::task::prompt;
use crate::task::recovery;
use crate::task::session::{self, Ended, Next, SessionCall, same_session};
use crate::task::stuck::{self, Decision};
use crate::task::support::{
    newest_worker_session, progress_of, record_of, record_task, record_task_in, state_of,
    wait_until, worker_of,
};
use crate::task::take_over;
use crate::task::worktree::{self, TaskTree};
use crate::workflow::StepAt;

/// Returns the state the task stopped in: `Merged`, or one that waits on a decision.
pub async fn run_task(host: &Host, task: &TaskId, at: &StepAt) -> EngineResult<TaskState> {
    drive(host, task, at, Until::Merged).await
}

/// Sends a merged task back from the merge approval and runs it to merge again,
/// resuming its worker with `note` as a round of review changes.
pub async fn send_back(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    note: &str,
) -> EngineResult<TaskState> {
    transition(host, task, Trigger::SentBack, note)?;
    let text = format!(
        "The merge approval sent this task back for changes: {note}\nMake them, then end \
         with your report again."
    );
    let parts = vec![prompt::message_part("merge approval", text)];
    // A task you finished yourself has no worker session to resume.
    let next = if host.read(|state| newest_worker_session(state, task).is_some()) {
        Next::resume(TryCause::ReviewChanges, parts)
    } else {
        Next::Fresh {
            cause: TryCause::ReviewChanges,
            why: note.to_owned(),
            note: None,
            extra: parts,
        }
    };
    drive_from(host, task, at, Until::Merged, Some(Carry::Next(next))).await
}

/// A workflow's own review step sent the task back: its worker goes again with `note`, as far
/// as its checks. The workflow reviews it again itself, so no review of the engine's runs.
pub(crate) async fn revise(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    note: &str,
) -> EngineResult<TaskState> {
    transition(
        host,
        task,
        Trigger::ChangesRequested,
        "the workflow's review sent it back",
    )?;
    let parts = vec![prompt::message_part("review", note)];
    let next = Next::resume(TryCause::ReviewChanges, parts);
    drive_from(host, task, at, Until::ChecksPassed, Some(Carry::Next(next))).await
}

/// Gives a merged task to a new worker from the merge approval, who starts fresh with
/// `why`, and runs it to merge again.
pub async fn give_back(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    why: &str,
) -> EngineResult<TaskState> {
    transition(host, task, Trigger::SentBack, why)?;
    let next = Next::fresh(TryCause::Reassigned, why);
    drive_from(host, task, at, Until::Merged, Some(Carry::Next(next))).await
}

/// You took a merged task over from the merge approval. Once you're done, it runs to
/// merge again.
pub async fn take_over_back(host: &Host, task: &TaskId, at: &StepAt) -> EngineResult<TaskState> {
    transition(
        host,
        task,
        Trigger::SentBack,
        "you took it over at the merge approval",
    )?;
    take_over::take_over(host, task)
        .await
        .map_err(EngineError::Refused)?;
    run_task_back(host, task, at).await
}

/// [`run_task`] for a task sent back from the merge approval, which waits for you first when
/// you took it over.
pub async fn run_task_back(host: &Host, task: &TaskId, at: &StepAt) -> EngineResult<TaskState> {
    if state_of(host, task)? == TaskState::TakenOver {
        wait_until(host, |state| {
            state.tasks.progress(task).take_over_end.is_some()
                || state
                    .tasks
                    .get(task)
                    .is_none_or(|record| record.state != TaskState::TakenOver)
        })
        .await?;
    }
    run_task(host, task, at).await
}

/// Moves the task by the table and records `Task::State`. A move the table doesn't allow is
/// refused.
pub fn transition(
    host: &Host,
    task: &TaskId,
    trigger: Trigger,
    reason: &str,
) -> EngineResult<TaskState> {
    let from = state_of(host, task)?;
    let to = machine::next(from, trigger).ok_or_else(|| {
        EngineError::Refused(format!("{task} can't move from {from:?} on {trigger:?}"))
    })?;
    let event = TaskEvent::State {
        from: Some(from),
        to,
        reason: reason.to_owned(),
    };
    record_task(host, task, event)?;
    Ok(to)
}

/// How one worker's try at a task ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TryEnd {
    /// Its checks passed, and its review approved it.
    Passed,
    Failed {
        why: String,
    },
    Stopped,
}

/// One candidate's try at a task in a tree of its own, such as a race's.
pub(crate) struct TryCall<'a> {
    pub task: &'a TaskId,
    pub worker: &'a ModelRef,
    pub tree: &'a TaskTree,
    pub at: &'a StepAt,
    /// Its first session's slot: how many worker sessions of this worker the task had in this
    /// step's round before the try, which is where replay finds the try's sessions.
    pub first_slot: u32,
    /// What its first session is told.
    pub note: &'a FallbackNote,
    /// Review rounds, shared by a race's tries so no two reviews of the task share one.
    pub rounds: &'a AtomicU32,
    /// Cancelled once another try won: no new session, check run, or review starts.
    pub stop: &'a CancellationToken,
}

/// One candidate's try in its own tree, through its checks (one resume with the check
/// output) and its review (a resume per change round), without merging and without
/// moving the task. The task's own tries run in [`run_task`], which reads the worker from
/// the state at each try, so a reassignment takes effect at the next one.
pub(crate) async fn run_try(host: &Host, call: TryCall<'_>) -> EngineResult<TryEnd> {
    let TryCall {
        task,
        worker,
        tree,
        at,
        first_slot,
        note,
        rounds,
        stop,
    } = call;
    let mut next = Next::Fresh {
        cause: TryCause::Fallback,
        why: "another agent tries it on its own branch".to_owned(),
        note: Some(Box::new(note.clone())),
        extra: Vec::new(),
    };
    let mut resume = None;
    let mut check_retried = false;
    let mut change_rounds = 0;
    let mut slot = first_slot;
    loop {
        if stop.is_cancelled() {
            return Ok(TryEnd::Stopped);
        }
        let call = SessionCall {
            task,
            worker,
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
                return Ok(TryEnd::Failed { why });
            }
            (EndReason::Completed, None) => {
                if let Some(decide) = asked_anyway::reply(host, task, worker, &ended)? {
                    next = Next::resume(TryCause::Resumed, vec![decide]);
                    continue;
                }
                let why = "it ended without a report that matches the schema".to_owned();
                return Ok(TryEnd::Failed { why });
            }
            (EndReason::Stopped | EndReason::Paused, _) => return Ok(TryEnd::Stopped),
            (reason, _) => {
                let why = format!("its session ended: {}", end_text(reason));
                return Ok(TryEnd::Failed { why });
            }
        }
        if stop.is_cancelled() {
            return Ok(TryEnd::Stopped);
        }
        match checks::run_in(host, task, tree, ended.report.as_ref()).await? {
            CheckVerdict::Passed => {}
            CheckVerdict::Failed { output } if !check_retried => {
                check_retried = true;
                let parts = vec![prompt::check_output_part(&output)];
                next = Next::resume(TryCause::CheckRetry, parts);
                continue;
            }
            CheckVerdict::Failed { output } => return Ok(TryEnd::Failed { why: output }),
            CheckVerdict::Stopped { reason } => return Ok(TryEnd::Failed { why: reason }),
        }
        if stop.is_cancelled() {
            return Ok(TryEnd::Stopped);
        }
        let record = record_of(host, task)?;
        if !needs_review(host, &record) {
            return Ok(TryEnd::Passed);
        }
        let reviewers = review::reviewers::eligible(
            &record.spec.reviewers,
            Some(worker),
            &providers(host),
            host.settings().review.other_provider,
        );
        let round = rounds.fetch_add(1, Ordering::SeqCst);
        match review::review_try(host, task, round, &reviewers, &tree.path).await? {
            review::Verdict::Approved => return Ok(TryEnd::Passed),
            review::Verdict::ChangesRequested { comments }
                if change_rounds < host.settings().review.max_change_rounds =>
            {
                change_rounds += 1;
                check_retried = false;
                let parts = vec![prompt::review_part(change_rounds, &comments)];
                next = Next::resume(TryCause::ReviewChanges, parts);
            }
            verdict @ review::Verdict::ChangesRequested { .. } => {
                let what = format!("review asked for changes again after {change_rounds} rounds");
                let why = review::with_findings(what, &verdict);
                return Ok(TryEnd::Failed { why });
            }
            review::Verdict::NeedsYou { reasons } => {
                let why = format!("its review sent it to you: {}", reasons.join("; "));
                return Ok(TryEnd::Failed { why });
            }
        }
    }
}

/// How far [`drive`] takes a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Until {
    Merged,
    /// Stop once the checks pass, before review: best of n keeps one such try.
    ChecksPassed,
}

/// What one state's handler hands the next, within this process.
#[derive(Debug, Clone, PartialEq)]
enum Carry {
    Next(Next),
    /// The recovery action decided for a session that broke.
    Action(RecoveryAction),
    /// Why the worker stopped for a decision.
    Ask(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Moved,
    /// Nothing more can happen until something outside changes.
    Wait,
}

/// Runs the task's state machine until it's merged (or, for best of n, its checks pass) or
/// waits on something outside it.
pub(crate) async fn drive(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    until: Until,
) -> EngineResult<TaskState> {
    drive_from(host, task, at, until, None).await
}

async fn drive_from(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    until: Until,
    mut carry: Option<Carry>,
) -> EngineResult<TaskState> {
    loop {
        if until == Until::ChecksPassed && state_of(host, task)? == TaskState::InReview {
            return Ok(TaskState::InReview);
        }
        match fallback::on_return::apply_pending(host, task).await? {
            Applied::Nothing => {}
            Applied::Moved => carry = None,
            Applied::NewWorker { why } => {
                carry = Some(Carry::Next(Next::fresh(TryCause::Reassigned, why)));
            }
            Applied::Changes { round, comments } => {
                let parts = vec![prompt::review_part(round, &comments)];
                carry = Some(Carry::Next(Next::resume(TryCause::ReviewChanges, parts)));
            }
        }
        let step = match state_of(host, task)? {
            TaskState::Waiting => waiting(host, task)?,
            TaskState::Ready => {
                transition(host, task, Trigger::Start, "worker, slot, and limit free")?;
                Step::Moved
            }
            TaskState::Running => running(host, task, at, &mut carry).await?,
            TaskState::Checking => checking(host, task, at, &mut carry).await?,
            TaskState::InReview => reviewing(host, task, &mut carry).await?,
            TaskState::Merging => merging(host, task, &mut carry).await?,
            TaskState::Failed => failed(host, task, &mut carry).await?,
            TaskState::NeedsYou => needs_you(host, task, at, &mut carry).await?,
            TaskState::NeedsDecision => needs_decision(host, task, at, &mut carry).await?,
            TaskState::TakenOver => taken_over(host, task, at, &mut carry).await?,
            // An answer you gave while the run was waiting goes on now.
            TaskState::Blocked if crate::blackboard::questions::answered_meanwhile(host, task)? => {
                Step::Moved
            }
            TaskState::Blocked | TaskState::Deferred | TaskState::Merged | TaskState::Cancelled => {
                Step::Wait
            }
        };
        if step == Step::Wait {
            return state_of(host, task);
        }
    }
}

/// Every dependency is merged, or was dropped.
pub(crate) fn dependencies_done(state: &RunState, task: &TaskId) -> bool {
    let Some(record) = state.tasks.get(task) else {
        return false;
    };
    record.spec.depends_on.iter().all(|dep| {
        state
            .tasks
            .get(dep)
            .is_some_and(|d| matches!(d.state, TaskState::Merged | TaskState::Cancelled))
    })
}

fn waiting(host: &Host, task: &TaskId) -> EngineResult<Step> {
    if !host.read(|state| dependencies_done(state, task)) {
        return Ok(Step::Wait);
    }
    let reason = "its dependencies are merged into the run";
    transition(host, task, Trigger::DependenciesMerged, reason)?;
    Ok(Step::Moved)
}

async fn running(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    let record = record_of(host, task)?;
    let worker = worker_of(host, &record);
    if writes_tests_as_output(host, &record, &worker) {
        endpoint_tests::write_tests(host, task).await?;
        if state_of(host, task)? == TaskState::Running {
            let reason = "the reviewer's acceptance tests are written";
            transition(host, task, Trigger::ReportedDone, reason)?;
        }
        return Ok(Step::Moved);
    }
    let tree = worktree::ensure(host, task).await?;
    // Only the worker's own session can be resumed, and only when it's the task's newest: after
    // someone else worked on the task, its old session knows nothing of that work.
    let resume = host.read(|state| {
        newest_worker_session(state, task)
            .filter(|session| session.started.worker == worker)
            .map(|session| session.key.clone())
    });
    let (next, open) = next_session(host, task, &worker, carry.take());
    let next = match next {
        Next::Resume { parts, .. } if resume.is_none() => Next::Fresh {
            cause: TryCause::Reassigned,
            why: "the task has a new worker".to_owned(),
            note: None,
            extra: parts,
        },
        next => next,
    };
    // A task worked as a goal takes its goal as it starts, and a fresh session is told of it.
    let next = match (next, goal::task::brief(host, task, &worker)?) {
        (
            Next::Fresh {
                cause,
                why,
                note,
                mut extra,
            },
            Some(told),
        ) => {
            extra.push(told);
            Next::Fresh {
                cause,
                why,
                note,
                extra,
            }
        }
        (next, _) => next,
    };
    if !open {
        let cause = next.cause();
        let try_number = if same_session(cause) {
            record.tries.max(1)
        } else {
            record.tries + 1
        };
        let event = TaskEvent::Try {
            try_number,
            worker: worker.clone(),
            cause,
        };
        record_task(host, task, event)?;
    }
    let call = SessionCall {
        task,
        worker: &worker,
        tree: &tree,
        at,
        next,
        resume,
        slot: None,
    };
    // A restart asked of an earlier session that never got to its end, as when the run
    // paused first, isn't one of this session.
    lock(&host.shared().restarting).remove(task);
    let ended = session::run(host, call).await?;
    after_session(host, task, &worker, ended, carry).await
}

/// An acceptance-test task owned by an endpoint reviewer: the tests come back as the reviewer's
/// output and Argonauts Code writes them, since an endpoint has no tools.
fn writes_tests_as_output(host: &Host, record: &TaskEntry, worker: &ModelRef) -> bool {
    matches!(record.spec.kind, TaskKind::AcceptanceTests { .. })
        && host
            .services()
            .workers
            .get(worker)
            .is_ok_and(|model| model.kind() == RunnerKind::Endpoint)
}

/// The next worker session, and whether it continues a try already recorded (after a
/// restart) instead of starting a new one.
fn next_session(
    host: &Host,
    task: &TaskId,
    worker: &ModelRef,
    carry: Option<Carry>,
) -> (Next, bool) {
    let (open, cause, newest_worker) = host.read(|state| {
        let newest = newest_worker_session(state, task);
        let progress = state.tasks.progress(task);
        let open = progress.try_seq.is_some_and(|seq| {
            newest.is_none_or(|session| session.since < seq || session.end.is_none())
        });
        let cause = progress.last_cause.unwrap_or(TryCause::First);
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
        None => Next::fresh(TryCause::First, ""),
        Some(model) if model != *worker => {
            Next::fresh(TryCause::Reassigned, "the task has a new worker")
        }
        Some(_) => Next::resume(TryCause::Resumed, vec![continue_part()]),
    };
    (next, false)
}

fn after_restart(cause: TryCause) -> Next {
    if same_session(cause) {
        Next::resume(cause, vec![continue_part()])
    } else if cause == TryCause::First {
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
    task: &TaskId,
    worker: &ModelRef,
    ended: Ended,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    // Whether you asked for it to start over while this session ran.
    let restart = lock(&host.shared().restarting).remove(task);
    // Someone moved the task while the session ran: you took it over, stopped it, or a
    // question blocked it.
    if state_of(host, task)? != TaskState::Running {
        return Ok(Step::Moved);
    }
    let now = worker_of(host, &record_of(host, task)?);
    if now != *worker {
        let why = format!("you reassigned it from {worker} to {now}");
        *carry = Some(Carry::Next(Next::fresh(TryCause::Reassigned, why)));
        return Ok(Step::Moved);
    }
    if restart {
        let next = Next::fresh(TryCause::Restarted, "you restarted it");
        *carry = Some(Carry::Next(next));
        return Ok(Step::Moved);
    }
    match &ended.end.reason {
        EndReason::Completed => completed(host, task, worker, &ended, carry).await,
        EndReason::Paused => {
            host.admitted().await?;
            let part = prompt::message_part(
                "resumed",
                "The run was paused and has resumed. Continue where you left off.",
            );
            *carry = Some(Carry::Next(Next::resume(TryCause::Resumed, vec![part])));
            Ok(Step::Moved)
        }
        reason => {
            match recovery::decide(host, task, &ended.end).await? {
                RecoveryAction::ResumeSame { message } => {
                    let parts = vec![prompt::message_part("message", message)];
                    *carry = Some(Carry::Next(Next::resume(TryCause::Resumed, parts)));
                }
                RecoveryAction::Rotate { note } => {
                    *carry = Some(Carry::Next(Next::Fresh {
                        cause: TryCause::Rotation,
                        why: "its context was full".to_owned(),
                        note: Some(note),
                        extra: Vec::new(),
                    }));
                }
                action if recovery::is_failure(reason) => {
                    transition(host, task, Trigger::Broke, &end_text(reason))?;
                    *carry = Some(Carry::Action(action));
                }
                action => recovery::apply(host, task, action).await?,
            }
            Ok(Step::Moved)
        }
    }
}

async fn completed(
    host: &Host,
    task: &TaskId,
    worker: &ModelRef,
    ended: &Ended,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    let Some(report) = ended.report.clone() else {
        // A turn that ended with a question goes on in its own session, told to decide.
        if let Some(decide) = asked_anyway::reply(host, task, worker, ended)? {
            let next = Next::resume(TryCause::Resumed, vec![decide]);
            *carry = Some(Carry::Next(next));
            return Ok(Step::Moved);
        }
        let reason = "the session ended without a report that matches the schema";
        transition(host, task, Trigger::Broke, reason)?;
        return Ok(Step::Moved);
    };
    let reported = TaskEvent::Reported(Box::new(report.clone()));
    record_task_in(host, task, &ended.key, reported)?;
    asked_anyway::on_report(host, task, &ended.key, &report)?;
    blackboard::on_report(host, task, &report).await?;
    if state_of(host, task)? != TaskState::Running {
        return Ok(Step::Moved);
    }
    let carries_on = goal::task::after_report(host, task, &report)?;
    match report.status {
        // Its CLI keeps the goal and says it's still at it: the same session goes on.
        ReportStatus::Done if carries_on.is_some() => {
            let parts = carries_on.into_iter().collect();
            *carry = Some(Carry::Next(Next::resume(TryCause::Resumed, parts)));
        }
        ReportStatus::Done => {
            transition(
                host,
                task,
                Trigger::ReportedDone,
                "the worker reported done",
            )?;
        }
        ReportStatus::Blocked => blocked(host, task, carry)?,
        ReportStatus::NeedsDecision => {
            let reason = "the worker needs a decision";
            transition(host, task, Trigger::NeedsDecision, reason)?;
            *carry = Some(Carry::Ask(report.summary.clone()));
        }
        ReportStatus::Failed => {
            transition(
                host,
                task,
                Trigger::Broke,
                "the worker reported that it failed",
            )?;
        }
    }
    Ok(Step::Moved)
}

/// How many reports in a row may say blocked with every question answered before the task
/// counts as failed, so an agent that never takes an answer can't turn forever.
const BLOCKED_ROUNDS: usize = 3;

/// A report that stopped for an answer. Where every question it asked is answered already, as
/// the main agent answers on autopilot and an earlier record or the consultant answers at once,
/// the same session carries on with the answers. Otherwise the task waits, and the answer
/// moves it on.
fn blocked(host: &Host, task: &TaskId, carry: &mut Option<Carry>) -> EngineResult<()> {
    let (waits, in_a_row) = host.read(|state| {
        let in_a_row = state.tasks.get(task).map_or(0, |record| {
            record
                .reports
                .iter()
                .rev()
                .take_while(|report| report.status == ReportStatus::Blocked)
                .count()
        });
        (
            blackboard::questions::waits_on_the_board(state, task),
            in_a_row,
        )
    });
    if waits {
        let reason = "the worker asked a question and waits for the answer";
        transition(host, task, Trigger::Question, reason)?;
    } else if in_a_row >= BLOCKED_ROUNDS {
        let reason = format!(
            "the worker reported blocked {in_a_row} times in a row, each time with its \
             questions answered"
        );
        transition(host, task, Trigger::Broke, &reason)?;
    } else {
        let part = prompt::message_part(
            "answered",
            "Your questions are answered in the messages with this one. Carry on with the task \
             and end with your report.",
        );
        *carry = Some(Carry::Next(Next::resume(TryCause::Resumed, vec![part])));
    }
    Ok(())
}

async fn checking(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    let verdict = checks::run(host, task).await?;
    // A task worked as a goal is checked against its lines after every try, and what still
    // fails goes back to the same session, with no fixed number of tries.
    if !matches!(verdict, CheckVerdict::Stopped { .. })
        && host.read(|state| goal::task::pursued(state, task))
    {
        let passed = verdict == CheckVerdict::Passed;
        if let AfterTry::Again(told) = goal::task::after_try(host, task, at, passed).await? {
            let reason = "the goal doesn't hold yet; the same session gets what still fails";
            transition(host, task, Trigger::GoalNotMet, reason)?;
            *carry = Some(Carry::Next(Next::resume(TryCause::GoalRetry, vec![told])));
            return Ok(Step::Moved);
        }
    }
    match verdict {
        CheckVerdict::Passed => {
            transition(host, task, Trigger::ChecksPassed, "the checks passed")?;
        }
        CheckVerdict::Failed { output } => {
            if progress_of(host, task).check_retries == 0 {
                let reason = "the checks failed; the same session gets their output";
                transition(host, task, Trigger::FirstCheckFailure, reason)?;
                // The checks ran on the commit alone, so a file it left out may be the cause.
                let mut parts = vec![prompt::check_output_part(&output)];
                parts.extend(worktree::left_out_part(host, task).await?);
                *carry = Some(Carry::Next(Next::resume(TryCause::CheckRetry, parts)));
            } else {
                let reason = "the checks failed again after the resume";
                transition(host, task, Trigger::ChecksFailedAgain, reason)?;
            }
        }
        CheckVerdict::Stopped { reason } => {
            transition(host, task, Trigger::ChecksFailedAgain, &reason)?;
            *carry = Some(Carry::Action(RecoveryAction::AskYou { reason }));
        }
    }
    Ok(Step::Moved)
}

async fn reviewing(host: &Host, task: &TaskId, carry: &mut Option<Carry>) -> EngineResult<Step> {
    let record = record_of(host, task)?;
    if !needs_review(host, &record) {
        transition(
            host,
            task,
            Trigger::Approved,
            "no review is set for this task",
        )?;
        return Ok(Step::Moved);
    }
    let change_rounds = progress_of(host, task).change_rounds;
    let round = change_rounds + 1;
    let verdict = review::review_task(host, task, round).await?;
    if verdict == review::Verdict::Approved {
        transition(host, task, Trigger::Approved, "the review approved it")?;
        return Ok(Step::Moved);
    }
    let review::Verdict::ChangesRequested { comments } = &verdict else {
        // The reviewers rejected the approach or couldn't review: it's yours to decide.
        let reason = review::with_findings("the review sent it to you".to_owned(), &verdict);
        transition(host, task, Trigger::ChangesRequestedAgain, &reason)?;
        return Ok(Step::Moved);
    };
    if change_rounds < host.settings().review.max_change_rounds {
        let reason = format!("review round {round} asked for changes");
        transition(host, task, Trigger::ChangesRequested, &reason)?;
        let parts = vec![prompt::review_part(round, comments)];
        *carry = Some(Carry::Next(Next::resume(TryCause::ReviewChanges, parts)));
    } else {
        let what = format!("review asked for changes again after {change_rounds} rounds");
        let reason = review::with_findings(what, &verdict);
        transition(host, task, Trigger::ChangesRequestedAgain, &reason)?;
    }
    Ok(Step::Moved)
}

/// Plain Captain runs have no reviewer; every other task is reviewed by another provider.
fn needs_review(host: &Host, record: &TaskEntry) -> bool {
    !(record.spec.kind == TaskKind::Captain
        && record.spec.reviewers.is_empty()
        && !host.settings().captain.review)
}

async fn merging(host: &Host, task: &TaskId, carry: &mut Option<Carry>) -> EngineResult<Step> {
    let merge = merge::merge_task(host, task).await?;
    let returns = progress_of(host, task).merge_returns;
    match merge {
        TaskMerge::Merged { sha } => {
            let short: String = sha.chars().take(8).collect();
            transition(host, task, Trigger::Merged, &format!("merged as {short}"))?;
        }
        TaskMerge::TestsFailed { .. } | TaskMerge::Conflict { .. } if returns >= MERGE_RETURNS => {
            let what = match &merge {
                TaskMerge::Conflict { files } => {
                    format!("the merge conflicted in {}", files.join(", "))
                }
                _ => "the affected tests failed after the merge".to_owned(),
            };
            transition(host, task, Trigger::AffectedTestsFailed, &what)?;
            let reason = format!("{what}, after it came back from merge {returns} times");
            transition(host, task, Trigger::Broke, &reason)?;
        }
        TaskMerge::TestsFailed { output } => {
            let reason = "the affected tests failed after the merge";
            transition(host, task, Trigger::AffectedTestsFailed, reason)?;
            let mut parts = vec![prompt::check_output_part(&output)];
            parts.extend(worktree::left_out_part(host, task).await?);
            *carry = Some(Carry::Next(Next::resume(TryCause::MergeFailed, parts)));
        }
        TaskMerge::Conflict { files } => {
            task_merge::bring_in_run_branch(host, task).await?;
            let reason = format!("the merge conflicted in {}", files.join(", "));
            transition(host, task, Trigger::AffectedTestsFailed, &reason)?;
            let text = format!(
                "Your task conflicts with the run branch in: {}. Argonauts Code merged \
                 the run branch into your worktree, and those files hold conflict \
                 markers. Resolve them so both sides' work survives, then end with your report \
                 again. Don't run git yourself.",
                files.join(", ")
            );
            let parts = vec![prompt::message_part("merge conflict", text)];
            *carry = Some(Carry::Next(Next::resume(TryCause::MergeFailed, parts)));
        }
    }
    Ok(Step::Moved)
}

async fn failed(host: &Host, task: &TaskId, carry: &mut Option<Carry>) -> EngineResult<Step> {
    let action = match carry.take() {
        Some(Carry::Action(action)) => action,
        _ => {
            let end =
                host.read(|state| newest_worker_session(state, task).and_then(|s| s.end.clone()));
            match end {
                Some(end) => recovery::decide(host, task, &end).await?,
                None => RecoveryAction::Proceed,
            }
        }
    };
    let why = failure_summary(&record_of(host, task)?, end_of(host, task).as_ref());
    match action {
        RecoveryAction::AskYou { reason } => {
            transition(host, task, Trigger::AskYou, &reason)?;
        }
        action @ RecoveryAction::Fallback { .. } if progress_of(host, task).ladder_retries == 0 => {
            let before = worker_of(host, &record_of(host, task)?);
            fallback::apply(host, task, action).await?;
            let record = record_of(host, task)?;
            let now = worker_of(host, &record);
            if record.state == TaskState::Failed {
                if now == before {
                    ladder(host, task, &why, carry)?;
                } else {
                    let reason = format!("{why}; a fresh session on {now}");
                    transition(host, task, Trigger::Retry, &reason)?;
                    *carry = Some(Carry::Next(Next::fresh(TryCause::AfterFailure, why)));
                }
            }
        }
        _ => ladder(host, task, &why, carry)?,
    }
    Ok(Step::Moved)
}

/// How often the worker gets its task back from merge before the task fails and the retry
/// ladder runs. More than the checks' one resume, since other tasks merging in the meantime can
/// bring new conflicts that aren't the worker's fault.
const MERGE_RETURNS: u32 = 3;

/// The retry ladder: a fresh session on another provider once, then it needs you.
fn ladder(host: &Host, task: &TaskId, why: &str, carry: &mut Option<Carry>) -> EngineResult<()> {
    if recovery::retry_on_other_provider(host, task, why)?.is_some() {
        *carry = Some(Carry::Next(Next::fresh(TryCause::AfterFailure, why)));
    }
    Ok(())
}

async fn needs_you(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    let snapshot = host.read(Clone::clone);
    if snapshot.run.mode == Some(argonauts_core::events::run::RunMode::Captain)
        && captain::moves_to_crew(&snapshot, host)
    {
        return Ok(Step::Wait);
    }
    let why = failure_summary(&record_of(host, task)?, end_of(host, task).as_ref());
    let from = TaskState::NeedsYou;
    if let Decision::Moved(Some(next)) = stuck::decide(host, task, Some(at), from, &why).await? {
        *carry = Some(Carry::Next(next));
    }
    Ok(Step::Moved)
}

async fn needs_decision(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    if progress_of(host, task).waiting_for.is_some() {
        return Ok(Step::Wait);
    }
    let why = match carry.take() {
        Some(Carry::Ask(summary)) => format!("its worker needs a decision: {summary}"),
        Some(Carry::Action(action)) => return apply_and_settle(host, task, action).await,
        _ => match end_of(host, task) {
            Some(end) if end.reason == EndReason::Stopped => "you stopped its session".to_owned(),
            Some(end) if end.reason != EndReason::Completed => {
                let action = recovery::decide(host, task, &end).await?;
                if !matches!(
                    action,
                    RecoveryAction::Proceed | RecoveryAction::ResumeSame { .. }
                ) {
                    return apply_and_settle(host, task, action).await;
                }
                format!("its agent stopped: {}", end_text(&end.reason))
            }
            _ => "it needs a decision".to_owned(),
        },
    };
    let from = TaskState::NeedsDecision;
    if let Decision::Moved(Some(next)) = stuck::decide(host, task, Some(at), from, &why).await? {
        *carry = Some(Carry::Next(next));
    }
    Ok(Step::Moved)
}

async fn taken_over(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    carry: &mut Option<Carry>,
) -> EngineResult<Step> {
    if progress_of(host, task).take_over_end.is_none() {
        return Ok(Step::Wait);
    }
    if let Decision::Moved(Some(next)) = stuck::take_over_return(host, task, Some(at)).await? {
        *carry = Some(Carry::Next(next));
    }
    Ok(Step::Moved)
}

fn end_of(host: &Host, task: &TaskId) -> Option<SessionEnd> {
    host.read(|state| newest_worker_session(state, task).and_then(|s| s.end.clone()))
}

async fn apply_and_settle(
    host: &Host,
    task: &TaskId,
    action: RecoveryAction,
) -> EngineResult<Step> {
    recovery::apply(host, task, action).await?;
    Ok(if state_of(host, task)? == TaskState::NeedsDecision {
        Step::Wait
    } else {
        Step::Moved
    })
}

/// One line on why the task is where it is: its newest failed check, else its newest report,
/// else how its newest session ended.
fn failure_summary(record: &TaskEntry, end: Option<&SessionEnd>) -> String {
    if let Some(check) = record.checks.iter().rev().find(|check| !check.passed) {
        return format!("{} failed: {}", check.name, check.summary);
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
        EndReason::LimitHit { dimension } => format!("hit its {dimension:?} limit"),
        EndReason::Paused => "paused".to_owned(),
        EndReason::Stopped => "stopped".to_owned(),
        EndReason::Stuck { watchdog } => format!("stuck ({watchdog:?})"),
        EndReason::UsageLimit { .. } => "usage limit".to_owned(),
        EndReason::RateLimited => "rate limited".to_owned(),
        EndReason::CliError { message, .. } => format!("agent error: {message}"),
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
    use crate::testutil::task_spec;

    fn state_with(tasks: &[(&str, &[&str], TaskState)]) -> RunState {
        let mut state = RunState::default();
        for (id, deps, task_state) in tasks {
            let mut record = TaskEntry::new(task_spec(id, deps, None));
            record.state = *task_state;
            state.tasks.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    #[test]
    fn a_task_waits_until_every_dependency_is_merged_or_dropped() {
        let id = |s: &str| TaskId::new(s).unwrap();
        let state = state_with(&[
            ("task-001", &[], TaskState::Merged),
            ("task-002", &[], TaskState::Running),
            ("task-003", &["task-001"], TaskState::Waiting),
            ("task-004", &["task-001", "task-002"], TaskState::Waiting),
            ("task-005", &["task-006"], TaskState::Waiting),
            ("task-006", &[], TaskState::Cancelled),
        ]);
        assert!(dependencies_done(&state, &id("task-003")));
        assert!(!dependencies_done(&state, &id("task-004")));
        assert!(dependencies_done(&state, &id("task-005")));
        assert!(!dependencies_done(&state, &id("task-404")));
    }

    #[test]
    fn failure_summary_prefers_the_newest_failed_check() {
        let mut record = TaskEntry::new(task_spec("task-001", &[], None));
        assert_eq!(failure_summary(&record, None), "it failed");
        record
            .checks
            .push(argonauts_core::events::task::CheckOutcome {
                name: "test".into(),
                kind: argonauts_core::events::task::CheckKind::Command,
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
    fn a_task_with_no_report_says_how_its_session_ended() {
        let record = TaskEntry::new(task_spec("task-001", &[], None));
        let end = |reason: EndReason| SessionEnd {
            cli_session: None,
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
