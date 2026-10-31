//! Runs that left the screen while they worked, because another workflow took it. Their events
//! still stream into the conversation, so both workflows show there as they work, and the one off
//! screen still ends with its closing answer. Its questions still open, and say which run they
//! answer.

use std::collections::HashSet;

use anyhow::Context as _;
use argonauts_core::RunId;
use argonauts_core::app::AppEvent;
use argonauts_core::runs::events::failure::run_failure_line;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::runs::events::{RunEvent, RunEventKind, RunLifecycle};
use tokio::task::JoinHandle;

use super::{RunNews, RunsState};
use crate::controller::Ctx;
use crate::controller::runs::attach::{self, Attached, Stage};

/// A run off screen whose events still stream into the conversation.
pub(super) struct Alongside {
    run: RunId,
    /// Every event since it was first on screen, for its recap.
    events: Vec<RunEvent>,
    finished: bool,
    stream: JoinHandle<()>,
}

impl Drop for Alongside {
    fn drop(&mut self) {
        self.stream.abort();
    }
}

/// The runs off screen that stream into the conversation, and those that ended there.
#[derive(Default)]
pub(super) struct Streaming {
    runs: Vec<Alongside>,
    /// Runs whose ending the conversation showed, so the listing doesn't say it again.
    ended_here: HashSet<RunId>,
}

/// Whether the run leaving the screen goes on working, and so goes on streaming.
pub(super) fn keeps_working(old: &Attached) -> bool {
    old.replay_done && !old.ended && old.stage == Stage::Going
}

/// Follows `old` from the event after its last, as it leaves the screen.
pub(super) async fn follow(
    ctx: &Ctx,
    runs: &mut RunsState,
    old: &mut Attached,
) -> anyhow::Result<()> {
    let next = old.events.last().map_or(0, |event| event.seq + 1);
    let stream = ctx
        .services
        .conductor
        .subscribe(&old.run, next)
        .await
        .with_context(|| format!("following {} off screen", old.run))?;
    let live = *stream.live.borrow();
    runs.streaming.runs.push(Alongside {
        run: old.run.clone(),
        events: std::mem::take(&mut old.events),
        finished: false,
        stream: attach::forward(old.run.clone(), stream, live, ctx.internal.clone()),
    });
    Ok(())
}

/// A run back on screen streams there again, so it stops streaming alongside.
pub(super) fn on_screen(runs: &mut RunsState, run: &RunId) {
    runs.streaming.runs.retain(|side| &side.run != run);
    runs.streaming.ended_here.remove(run);
}

/// Whether the conversation already showed how the run ended.
pub(super) fn ended_here(runs: &RunsState, run: &RunId) -> bool {
    runs.streaming.ended_here.contains(run)
        || runs
            .streaming
            .runs
            .iter()
            .any(|side| &side.run == run && side.finished)
}

/// An event of a run off screen goes to the conversation. Its finish brings its recap, as the run
/// on screen's does, and leaves the mode to the run on screen.
pub(super) async fn on_event(ctx: &mut Ctx, runs: &mut RunsState, run: &RunId, event: RunEvent) {
    let Some(side) = runs.streaming.runs.iter_mut().find(|side| &side.run == run) else {
        return;
    };
    // The stream it left the screen with may still deliver what the new one sends again.
    if side.events.last().is_some_and(|last| event.seq <= last.seq) {
        return;
    }
    ctx.send(AppEvent::Run {
        event: Box::new(crate::views::required::for_screen(&event, &ctx.env)),
    });
    let outcome = match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, detail }) => {
            // The conversation that started it hears how it ended, as from a run on screen.
            let line = run_failure_line(detail.as_deref());
            runs.news.push((
                run.clone(),
                RunNews::Ended {
                    outcome: *outcome,
                    line,
                },
            ));
            Some(*outcome)
        }
        _ => None,
    };
    side.events.push(event);
    let Some(outcome) = outcome else {
        return;
    };
    side.finished = true;
    let events = std::mem::take(&mut side.events);
    super::send_recap(ctx, run, &events, outcome).await;
    super::notify_finished(ctx, runs, run, outcome, None);
    // Its row goes now rather than at the next minute's listing.
    super::refresh_background(ctx);
}

/// Its stream closed: it ended, or its run host went. One that ended was told here.
pub(super) fn on_ended(runs: &mut RunsState, run: &RunId) {
    let streaming = &mut runs.streaming;
    let Some(at) = streaming.runs.iter().position(|side| &side.run == run) else {
        return;
    };
    let side = streaming.runs.remove(at);
    if side.finished {
        streaming.ended_here.insert(side.run.clone());
    } else {
        let line = "its run host exited before waiting".to_owned();
        let outcome = RunOutcome::Failed;
        runs.news
            .push((side.run.clone(), RunNews::Ended { outcome, line }));
    }
}

/// Whether the run streams into the conversation from off screen.
#[cfg(test)]
pub(super) fn streams(runs: &RunsState, run: &RunId) -> bool {
    runs.streaming.runs.iter().any(|side| &side.run == run)
}
