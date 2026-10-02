//! Attaching to a run: its events from the latest snapshot, the condensed replay, then
//! `ReplayDone`, so replayed checkpoints ring no bell. Each stream runs on its own task and
//! reports through `RunUpdate`.

use std::io;
use std::sync::Arc;

use anyhow::Context;
use argonauts_core::api::{EventStream, RunState, RunSummary, StatusSummary};
use argonauts_core::app::{AppEvent, AppMode};
use argonauts_core::events::RunEvent;
use argonauts_core::events::run::{ParkReason, RunOutcome};
use argonauts_core::layout::run_files;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::budget::BudgetAmount;
use argonauts_core::{NoteLevel, RunId, Spend};
use argonauts_crew::status::Viewer;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::journal_history;
use super::narrator_host::NarratorHost;
use super::recap;
use super::runs::{RunUpdate, RunsState};
use super::{Ctx, Internal};
use crate::{client, views};

/// How a run's stream opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Opening {
    /// A run this app just started: every event, and no condensed replay.
    New,
    /// A run started elsewhere, such as by `argonauts run`: every event, then the replay.
    Watch,
    /// From the latest snapshot, then the replay.
    Reattach,
}

/// Where the run's lifecycle stands, from its latest lifecycle event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    Going,
    Parked,
    Finished(RunOutcome),
}

/// The run on screen and what its events said so far.
pub(super) struct Attached {
    pub(super) run: RunId,
    show_replay: bool,
    /// The stream began at the latest snapshot, not at the run's first event.
    from_snapshot: bool,
    /// Events up to this `seq` are history.
    replay_until: Option<u64>,
    pub(super) replay_done: bool,
    /// A supervisor serves the run now.
    pub(super) live: bool,
    /// The stream closed, so a resume needs a new one.
    pub(super) ended: bool,
    pub(super) crashed: bool,
    pub(super) stage: Stage,
    /// What the run's events make the app mode, applied once replay is done.
    pub(super) mode: AppMode,
    pub(super) approval_mode: Option<ApprovalMode>,
    pub(super) spent: Option<Spend>,
    pub(super) limit: Option<BudgetAmount>,
    /// Every event of this stream, for the replay and for views when the log can't be read.
    pub(super) events: Vec<RunEvent>,
    pub(super) narrator: NarratorHost,
    stream: JoinHandle<()>,
}

impl Attached {
    /// The supervisor is gone and the run neither parked nor finished.
    /// This event is the last of the history.
    pub(super) fn replay_ends_at(&self, seq: u64) -> bool {
        !self.replay_done && self.replay_until.is_some_and(|until| seq >= until)
    }

    pub(super) fn has_crashed(&self) -> bool {
        self.replay_done && !self.live && self.stage == Stage::Going
    }
}

#[cfg(test)]
impl Attached {
    /// As a stream opens on a log that already holds events up to `seq`, which the fake
    /// conductor's streams never do.
    pub(super) fn replay_history_until(&mut self, seq: u64) {
        self.replay_until = Some(seq);
        self.replay_done = false;
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.stream.abort();
    }
}

/// Opens the run's stream and puts it on screen, in place of any other.
pub(super) async fn attach(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    summary: RunSummary,
    opening: Opening,
) -> anyhow::Result<()> {
    let run = summary.run.clone();
    let conductor = Arc::clone(&ctx.services.conductor);
    let stream = match opening {
        Opening::Reattach => conductor.reattach(&run).await,
        Opening::New | Opening::Watch => conductor.subscribe(&run, 0).await,
    }
    .with_context(|| format!("attaching to {run}"))?;
    if let Some(old) = runs.attached.take()
        && old.run != run
    {
        ctx.send(AppEvent::Detached {
            run: old.run.clone(),
        });
    }
    let mode = mode_from_summary(&summary);
    ctx.send(AppEvent::Attached {
        summary,
        started_here: opening == Opening::New,
    });
    let narrator = NarratorHost::start(ctx, &run);
    ctx.send(AppEvent::NarratorStatus {
        status: narrator.status(),
    });
    // A run this app just started is live from its first event, so the screen draws its
    // triage and every step rather than folding them into a replay.
    let replay_until = match opening {
        Opening::New => None,
        Opening::Watch | Opening::Reattach => stream.replay_until,
    };
    let live = *stream.live.borrow();
    let task = forward(run.clone(), stream, ctx.internal.clone());
    let attached = Attached {
        run: run.clone(),
        show_replay: opening != Opening::New,
        from_snapshot: opening == Opening::Reattach,
        replay_until,
        replay_done: replay_until.is_none(),
        live,
        ended: false,
        crashed: false,
        stage: Stage::Going,
        mode,
        approval_mode: None,
        spent: None,
        limit: None,
        events: Vec::new(),
        narrator,
        stream: task,
    };
    // An empty log has no history to wait for, and a new run's history prints as it comes.
    if attached.replay_done {
        ctx.send(AppEvent::ReplayDone { run, seq: 0 });
        ctx.set_mode(attached.mode.clone());
    }
    runs.attached = Some(attached);
    runs.attaches += 1;
    Ok(())
}

/// Follows the run on screen again from the event after its last, so a stopped run you
/// reopened goes on in place rather than replaying.
pub(super) async fn follow_on(ctx: &Ctx, attached: &mut Attached) -> anyhow::Result<()> {
    let next = attached.events.last().map_or(0, |event| event.seq + 1);
    let stream = ctx
        .services
        .conductor
        .subscribe(&attached.run, next)
        .await
        .with_context(|| format!("following {}", attached.run))?;
    attached.live = *stream.live.borrow();
    attached.ended = false;
    attached.stream.abort();
    attached.stream = forward(attached.run.clone(), stream, ctx.internal.clone());
    Ok(())
}

/// Sends the condensed replay when there was history, then `ReplayDone`, and applies the mode
/// the history left.
pub(super) async fn finish_replay(ctx: &mut Ctx, attached: &mut Attached) {
    attached.replay_done = true;
    let run = attached.run.clone();
    if attached.show_replay && !attached.events.is_empty() {
        let summary = status_summary(ctx, &run, &attached.events).await;
        let outcome = match attached.stage {
            Stage::Finished(outcome) => Some(outcome),
            Stage::Going | Stage::Parked => None,
        };
        let recap = recap::recap_from_summary(&summary, outcome);
        let journal = whole_history(ctx, attached).await;
        let history = journal.as_deref().unwrap_or(&attached.events);
        if let Some(replay) = views::condensed_replay(history, &run, recap) {
            ctx.send(replay);
        }
    }
    let seq = attached.replay_until.unwrap_or(0);
    ctx.send(AppEvent::ReplayDone { run, seq });
    ctx.set_mode(attached.mode.clone());
    report_crash(ctx, attached);
}

/// For a stream that began at the latest snapshot: what the replay needs from the whole
/// journal. `None` leaves the replay to the events this app holds.
async fn whole_history(ctx: &Ctx, attached: &Attached) -> Option<Vec<RunEvent>> {
    let upto = attached.replay_until.filter(|_| attached.from_snapshot)?;
    let path = ctx.env.run_dir(&attached.run).join(run_files::EVENTS);
    let read =
        tokio::task::spawn_blocking(move || journal_history::replay_history(&path, upto)).await;
    let err = match read {
        Ok(Ok(history)) => return (!history.is_empty()).then_some(history),
        // A conductor that keeps no journal here has nothing more to give.
        Ok(Err(err)) if err.kind() == io::ErrorKind::NotFound => return None,
        Ok(Err(err)) => err.to_string(),
        Err(err) => err.to_string(),
    };
    ctx.notice(
        NoteLevel::Warn,
        format!("The replay covers only what happened since the latest snapshot: {err}"),
    );
    None
}

/// Shows a crashed run as crashed, once, rather than quietly tailing its log. The run's lock
/// and log have the last word, since its last events can reach this app after its exit does.
pub(super) fn report_crash(ctx: &mut Ctx, attached: &mut Attached) -> bool {
    if attached.crashed
        || !attached.has_crashed()
        || !client::supervisor_crashed(&ctx.env.run_dir(&attached.run))
    {
        return false;
    }
    attached.crashed = true;
    attached.mode = AppMode::Idle;
    ctx.notice(
        NoteLevel::Warn,
        format!(
            "{run} crashed: its supervisor exited before parking. /resume {run} recovers it.",
            run = attached.run
        ),
    );
    ctx.set_mode(AppMode::Idle);
    true
}

/// The code-built summary, from the conductor or else from the events this app holds.
pub(super) async fn status_summary(ctx: &Ctx, run: &RunId, events: &[RunEvent]) -> StatusSummary {
    match ctx.services.conductor.status(run).await {
        Ok(summary) => summary,
        Err(_) => argonauts_crew::status::summary(events, &Viewer::User),
    }
}

/// The best guess before the events say more.
fn mode_from_summary(summary: &RunSummary) -> AppMode {
    let run = summary.run.clone();
    match summary.state {
        RunState::Running => AppMode::Working { run },
        RunState::Pausing => AppMode::Pausing { run },
        RunState::Parked => {
            let reason = if summary.waiting.is_empty() {
                ParkReason::Paused
            } else {
                ParkReason::Checkpoint
            };
            AppMode::Parked { run, reason }
        }
        RunState::Crashed
        | RunState::Merged
        | RunState::PullRequest
        | RunState::BranchLeft
        | RunState::Stopped
        | RunState::Failed
        | RunState::Abandoned => AppMode::Idle,
    }
}

/// Forwards the stream's events and liveness, then `Ended` once it closes.
fn forward(
    run: RunId,
    mut stream: EventStream,
    internal: mpsc::UnboundedSender<Internal>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let send = |update: RunUpdate| internal.send(Internal::Run(update)).is_ok();
        let mut live = *stream.live.borrow_and_update();
        let mut watching_live = true;
        loop {
            tokio::select! {
                // Events first, so a supervisor's last events land before its exit does.
                biased;
                event = stream.events.recv() => match event {
                    Some(event) => {
                        let update = RunUpdate::Event { run: run.clone(), event: Box::new(event) };
                        if !send(update) {
                            return;
                        }
                    }
                    None => break,
                },
                changed = stream.live.changed(), if watching_live => match changed {
                    Ok(()) => {
                        let now = *stream.live.borrow_and_update();
                        if now != live {
                            live = now;
                            if !send(RunUpdate::Live { run: run.clone(), live }) {
                                return;
                            }
                        }
                    }
                    // The sender is gone; its last value still counts below.
                    Err(_) => watching_live = false,
                },
            }
        }
        let now = *stream.live.borrow();
        if now != live {
            send(RunUpdate::Live {
                run: run.clone(),
                live: now,
            });
        }
        send(RunUpdate::Ended { run });
    })
}
