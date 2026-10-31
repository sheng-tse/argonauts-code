//! Attaching to a run: its events from the latest snapshot, then `ReplayDone`, so replayed
//! approvals ring no bell. What the run did before is on screen already, drawn live or from its
//! record, so the replay only rebuilds state. Each stream runs on its own task and reports
//! through `RunUpdate`.

use std::sync::Arc;

use anyhow::Context;
use argonauts_core::app::{AppEvent, AppMode};
use argonauts_core::runs::api::{EventStream, RunState, RunSummary, StatusSummary};
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::run::{RunOutcome, WaitReason};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::limit::LimitAmount;
use argonauts_core::{NoteLevel, RunId, Spend};
use argonauts_crew::status::Viewer;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::narrator_host::NarratorHost;
use crate::client;
use crate::controller::runs::{self, RunUpdate, RunsState};
use crate::controller::{Ctx, Internal};
use crate::conversation::store::ConversationRun;

/// How a run's stream opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::controller) enum Opening {
    /// A run this app just started: every event, live from the first.
    New,
    /// A run started elsewhere, such as by `argonauts run`: every event, those it had already
    /// as history.
    Watch,
    /// From the latest snapshot, which is history.
    Reattach,
}

/// Where the run's lifecycle stands, from its latest lifecycle event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::controller) enum Stage {
    Going,
    Waiting,
    Finished(RunOutcome),
}

/// The run on screen and what its events said so far.
pub(in crate::controller) struct Attached {
    pub(in crate::controller) run: RunId,
    /// Saved with each conversation the run is on screen in.
    pub(in crate::controller) title: String,
    /// Events up to this `seq` are history.
    replay_until: Option<u64>,
    pub(in crate::controller) replay_done: bool,
    /// A run host serves the run now.
    pub(in crate::controller) live: bool,
    /// The stream closed, so a resume needs a new one.
    pub(in crate::controller) ended: bool,
    pub(in crate::controller) crashed: bool,
    pub(in crate::controller) stage: Stage,
    /// Another conversation came on screen once the run had finished, so the Context tab,
    /// which lists the conversation's last run, no longer lists it.
    pub(in crate::controller) left_behind: bool,
    /// What the run's events make the app mode, applied once replay is done.
    pub(in crate::controller) mode: AppMode,
    pub(in crate::controller) approval_mode: Option<ApprovalMode>,
    pub(in crate::controller) spent: Option<Spend>,
    pub(in crate::controller) limit: Option<LimitAmount>,
    /// Every event of this stream, for views when the log can't be read.
    pub(in crate::controller) events: Vec<RunEvent>,
    pub(in crate::controller) narrator: NarratorHost,
    stream: JoinHandle<()>,
}

impl Attached {
    /// The run host is gone and the run neither waiting nor finished.
    /// This event is the last of the history.
    pub(in crate::controller) fn replay_ends_at(&self, seq: u64) -> bool {
        !self.replay_done && self.replay_until.is_some_and(|until| seq >= until)
    }

    pub(in crate::controller) fn has_crashed(&self) -> bool {
        self.replay_done && !self.live && self.stage == Stage::Going
    }
}

#[cfg(test)]
impl Attached {
    /// As a stream opens on a log that already holds events up to `seq`, which the fake
    /// conductor's streams never do.
    pub(in crate::controller) fn replay_history_until(&mut self, seq: u64) {
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
pub(in crate::controller) async fn attach(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    summary: RunSummary,
    opening: Opening,
) -> anyhow::Result<()> {
    let run = summary.run.clone();
    let title = summary.title.clone();
    let conductor = Arc::clone(&ctx.services.conductor);
    let stream = match opening {
        Opening::Reattach => conductor.reattach(&run).await,
        Opening::New | Opening::Watch => conductor.subscribe(&run, 0).await,
    }
    .with_context(|| format!("attaching to {run}"))?;
    if let Some(old) = runs.attached.take()
        && old.run != run
    {
        super::give_way(ctx, runs, old).await;
        // The run it leaves goes on, as when you start a second workflow while the first
        // works, so its row comes now rather than at the next minute's listing.
        super::refresh_background(ctx);
    }
    super::came_on_screen(runs, &run);
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
    // the estimate and every step rather than folding them into a replay.
    let replay_until = match opening {
        Opening::New => None,
        Opening::Watch | Opening::Reattach => stream.replay_until,
    };
    let live = *stream.live.borrow();
    let task = forward(run.clone(), stream, live, ctx.internal.clone());
    let mut attached = Attached {
        run: run.clone(),
        title: title.clone(),
        replay_until,
        replay_done: replay_until.is_none(),
        live,
        ended: false,
        crashed: false,
        stage: Stage::Going,
        left_behind: false,
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
        // A run host gone before the stream opened sends no change of its own, so a run it
        // never began is said now, as after a replay.
        report_crash(ctx, &mut attached);
    }
    // Started or opened here, it's this conversation's run from now on.
    runs.ours.insert(attached.run.clone());
    runs.opened.push(ConversationRun {
        run: attached.run.clone(),
        title,
    });
    runs.attached = Some(attached);
    runs.attaches += 1;
    Ok(())
}

/// Follows the run on screen again from the event after its last, so a stopped run you
/// reopened goes on in place rather than replaying.
pub(in crate::controller) async fn follow_on(
    ctx: &Ctx,
    attached: &mut Attached,
) -> anyhow::Result<()> {
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
    attached.stream = forward(
        attached.run.clone(),
        stream,
        attached.live,
        ctx.internal.clone(),
    );
    Ok(())
}

/// The history is in: `ReplayDone`, then the mode the history left.
pub(in crate::controller) fn finish_replay(ctx: &mut Ctx, attached: &mut Attached) {
    attached.replay_done = true;
    let seq = attached.replay_until.unwrap_or(0);
    ctx.send(AppEvent::ReplayDone {
        run: attached.run.clone(),
        seq,
    });
    ctx.set_mode(attached.mode.clone());
    report_crash(ctx, attached);
}

/// Shows a crashed run as crashed, once, rather than quietly tailing its log. The run's lock
/// and log have the last word, since its last events can reach this app after its exit does.
pub(in crate::controller) fn report_crash(ctx: &mut Ctx, attached: &mut Attached) -> bool {
    if attached.crashed
        || !attached.has_crashed()
        || !client::run_host_crashed(&ctx.env.run_dir(&attached.run))
    {
        return false;
    }
    attached.crashed = true;
    attached.mode = AppMode::Idle;
    let text = lost_its_run_host(ctx, &attached.run);
    ctx.notice(NoteLevel::Warn, text);
    ctx.set_mode(AppMode::Idle);
    true
}

/// What is said of the run on screen once its run host is gone: how far it got and what
/// `/resume` does, as the next start would say it, or why it never started. Said here, it
/// isn't said again at the next start.
fn lost_its_run_host(ctx: &Ctx, run: &RunId) -> String {
    let run_dir = ctx.env.run_dir(run);
    let Some(summary) = client::summarize(&run_dir, ctx.settings()) else {
        return format!("{run} stopped when its run host exited · /resume {run} carries it on");
    };
    let title = summary.name.as_deref().unwrap_or(&summary.title);
    if summary.state == RunState::Crashed {
        return match runs::told::once(&run_dir, &summary) {
            Some((_, line)) => line,
            None => format!("\"{title}\" {}", summary.stopped_at),
        };
    }
    // Before its first event a run has nothing to resume, and its run host's last words say
    // what kept it from starting.
    match client::run_host_last_words(&run_dir) {
        Some(why) => format!("\"{title}\" didn't start: {why}"),
        None => format!("\"{title}\" didn't start: its run host exited before the run began"),
    }
}

/// The code-built summary, from Argonauts Code or else from the events this app holds.
pub(in crate::controller) async fn status_summary(
    ctx: &Ctx,
    run: &RunId,
    events: &[RunEvent],
) -> StatusSummary {
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
        RunState::Waiting => {
            let reason = if summary.waiting.is_empty() {
                WaitReason::Paused
            } else {
                WaitReason::Approval
            };
            AppMode::Waiting { run, reason }
        }
        RunState::Crashed
        | RunState::NeverStarted
        | RunState::FromAnotherBuild
        | RunState::Merged
        | RunState::PullRequest
        | RunState::BranchLeft
        | RunState::Stopped
        | RunState::Failed
        | RunState::Abandoned => AppMode::Idle,
    }
}

/// Forwards the stream's events and liveness, then `Ended` once it closes. `seen` is the
/// liveness the app took when the stream opened: one that changed before this task first
/// looks is still sent.
pub(in crate::controller) fn forward(
    run: RunId,
    mut stream: EventStream,
    seen: bool,
    internal: mpsc::UnboundedSender<Internal>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let send = |update: RunUpdate| internal.send(Internal::Run(update)).is_ok();
        let mut live = seen;
        let mut watching_live = true;
        loop {
            tokio::select! {
                // Events first, so a run host's last events land before its exit does.
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::watch;

    use super::*;

    /// The run host can let go between the app's look at a new stream and this task's first:
    /// the change still reaches the app, or a run that never began would read as working.
    #[tokio::test]
    async fn a_run_host_gone_before_the_first_look_is_still_sent() {
        let (_events, receiver) = mpsc::channel(1);
        let (live, live_receiver) = watch::channel(true);
        let stream = EventStream {
            events: receiver,
            replay_until: None,
            live: live_receiver,
        };
        live.send_replace(false);
        let (internal, mut updates) = mpsc::unbounded_channel();
        let _task = forward(RunId::new("run-001").unwrap(), stream, true, internal);
        let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
            .await
            .expect("an update in time");
        assert!(
            matches!(
                update,
                Some(Internal::Run(RunUpdate::Live { live: false, .. }))
            ),
            "the run host's going"
        );
    }
}
