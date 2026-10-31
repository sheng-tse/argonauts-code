//! What the main session hears of the runs the conversation starts. The start tool's call
//! waits until its run is under way or has failed, so the session never says a run started
//! when it didn't, and a failure comes back as a failure. A run whose orchestrator copies the
//! session is answered once Argonauts Code took it, since the copy is made after. A run that
//! fails after the answer is told to the session in a turn of its own, which says so to you in
//! one line.

use std::collections::BTreeSet;

use argonauts_core::RunId;
use argonauts_core::runs::asking::AskReply;
use argonauts_core::runs::events::run::RunOutcome;
use tokio::sync::oneshot;

use super::ConversationState;
use super::lifecycle::begin;
use crate::controller::Ctx;
use crate::controller::runs::RunNews;
use crate::controller::turns::turn::TurnStep;

/// What the session goes on with once its run is under way.
const GO_ON: &str = "Say so in one line and stop: its agents do the work, the user watches it, \
                     and its closing answer lands in this conversation.";

#[derive(Default)]
pub(super) struct Watched {
    /// The start tool's call, waiting for its run to begin or fail.
    starting: Option<Starting>,
    /// Runs the conversation started that are still going, whose failure the session is told.
    going: BTreeSet<RunId>,
    /// What the session hasn't been told yet, oldest first: it hears once no turn runs.
    untold: Vec<String>,
}

/// When the start tool's call hears that its run started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Answer {
    /// Once its first agent started, or the run failed or waits for its CLIs.
    WhenUnderWay,
    /// Once Argonauts Code took the request: the run's first agent is a copy of this session,
    /// which must hold the answer rather than a call that never got one.
    WhenTaken,
}

struct Starting {
    /// Set once Argonauts Code took the request.
    run: Option<RunId>,
    when: Answer,
    /// What started, as "The council workflow".
    what: String,
    /// The sentence for a run under way, as "The council workflow started on this.".
    began: String,
    reply: oneshot::Sender<AskReply>,
}

/// The start tool's call waits on `reply` until its run begins or fails, or until Argonauts Code
/// took it, as `when` says. `what` names the run as "The council workflow", and `began` is what
/// the session hears once it is under way.
pub(super) fn hold(
    conversation: &mut ConversationState,
    reply: oneshot::Sender<AskReply>,
    what: String,
    began: String,
    when: Answer,
) {
    conversation.run_news.starting = Some(Starting {
        run: None,
        when,
        what,
        began,
        reply,
    });
}

/// Argonauts Code took the request for `run`. One a turn of the conversation asked for
/// (`from_conversation`) is watched, so the session hears if it fails later.
pub(crate) fn run_started(
    conversation: &mut ConversationState,
    run: &RunId,
    from_conversation: bool,
) {
    let watched = &mut conversation.run_news;
    if let Some(starting) = watched
        .starting
        .as_mut()
        .filter(|starting| starting.run.is_none())
    {
        starting.run = Some(run.clone());
    }
    // A later failure is told in a turn of its own, as for any run that began.
    let taken = watched
        .starting
        .take_if(|starting| starting.when == Answer::WhenTaken);
    if let Some(starting) = taken {
        answer(starting, &RunNews::Began);
    }
    if from_conversation {
        watched.going.insert(run.clone());
    }
}

/// Argonauts Code refused to start the run. True when the session was told, so the failure is
/// said once, by it.
pub(crate) fn run_not_started(conversation: &mut ConversationState, err: &anyhow::Error) -> bool {
    let Some(starting) = conversation
        .run_news
        .starting
        .take_if(|starting| starting.run.is_none())
    else {
        return false;
    };
    let line = argonauts_core::runs::events::failure::run_failure_line(Some(&format!("{err:#}")));
    let text = format!(
        "{} didn't start: {line}. Tell the user in one line that it didn't start and why, and \
         stop.",
        starting.what
    );
    if let Some(turn) = conversation.turn.as_mut() {
        turn.run_began = false;
    }
    starting.reply.send(failure(text)).is_ok()
}

/// How the runs on screen got on. The start tool answers once its run began or ended, and a
/// run of the conversation's that failed later is told to the session.
pub(crate) fn hear(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    news: Vec<(RunId, RunNews)>,
) {
    for (run, news) in news {
        let waiting = conversation
            .run_news
            .starting
            .take_if(|starting| starting.run.as_ref() == Some(&run));
        let told = match waiting {
            Some(starting) => {
                let (told, failed) = answer(starting, &news);
                // A run that failed doesn't count as this request's workflow, so the session
                // may start another for it.
                if failed && let Some(turn) = conversation.turn.as_mut() {
                    turn.run_began = false;
                }
                told
            }
            None => false,
        };
        if let RunNews::Ended { outcome, line } = news {
            let ours = conversation.run_news.going.remove(&run);
            if ours && !told && outcome == RunOutcome::Failed {
                conversation.run_news.untold.push(failed_later(&run, &line));
            }
        }
    }
    tell(ctx, conversation);
}

/// The start tool's answer: whether the session heard it, and whether it was a failure. A run
/// that ended before it began says how, one that waits for its CLIs says so, and one that
/// began goes on.
fn answer(starting: Starting, news: &RunNews) -> (bool, bool) {
    let Starting {
        what, began, reply, ..
    } = starting;
    let reply_with = match news {
        RunNews::Waiting { line } => AskReply::text(format!(
            "{what} is waiting: {line}; nothing runs until then. Say so in one line and stop."
        )),
        RunNews::Began
        | RunNews::Ended {
            outcome: RunOutcome::Merged | RunOutcome::PullRequest | RunOutcome::BranchLeft,
            ..
        } => AskReply::text(format!("{began} {GO_ON}")),
        RunNews::Ended {
            outcome: RunOutcome::Failed,
            line,
        } => failure(format!(
            "{what} failed before any agent started: {line}. Tell the user in one line that it \
             failed and why, and stop: nothing is running."
        )),
        RunNews::Ended { .. } => AskReply::text(format!(
            "{what} was stopped before any agent started. Say so in one line and stop."
        )),
    };
    let failed = reply_with.is_error;
    (reply.send(reply_with).is_ok(), failed)
}

/// A call's answer the session reads as a failure.
fn failure(text: String) -> AskReply {
    AskReply {
        text,
        is_error: true,
    }
}

/// What the session is told of a run that failed after it began.
fn failed_later(run: &RunId, line: &str) -> String {
    format!(
        "Argonauts Code, not the user, tells you: {run}, the run started for the user's request, \
         failed: {line}. Tell the user in one line that it failed and why. Start nothing, since \
         they decide what comes next."
    )
}

/// Tells the session what it hasn't heard, once no turn runs and nothing you typed waits.
fn tell(ctx: &mut Ctx, conversation: &mut ConversationState) {
    if conversation.run_news.untold.is_empty()
        || conversation.turn.is_some()
        || !conversation.queued.is_empty()
    {
        return;
    }
    let text = std::mem::take(&mut conversation.run_news.untold).join("\n\n");
    if let Err(err) = begin(ctx, conversation, text, |_| TurnStep::Told) {
        ctx.error(&err);
    }
}
