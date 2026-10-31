//! Summaries from inside the app: what `d` on the Behaviors tab asks, the update its
//! answers start, in the background with its progress on screen, `x` stopping it, and the
//! update after each run. One update runs at a time.

use std::sync::{Arc, Mutex};

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::behaviors::behavior_map::MapRequest;
use argonauts_core::behaviors::deep_map::{
    DeepDone, DeepOffer, DeepProgress, DeepScope, DeepStart,
};
use argonauts_core::settings::behaviors::{BehaviorMap, BehaviorUpdate};
use tokio_util::sync::CancellationToken;

use super::gather::project_of;
use super::service::{AfterRun, DeepMaps};
use crate::controller::{Ctx, Effect, Internal, Outbox};
use crate::host::clis::RepoModels;

const ALREADY_RUNNING: &str =
    "Summaries are already being made. x on the Behaviors tab stops them.";

/// Said once, after the first run that would have added to a map waiting to be made again.
const REDO_WAITS: &str = "Summaries take no new actions until they're made again with the \
    summarizer and prompt now asked for. d on the Behaviors tab asks.";

/// The update that's running, if one is, by what stops it, and the map the tab last asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct DeepWork {
    running: Arc<Mutex<Option<CancellationToken>>>,
    shown: Arc<Mutex<Option<MapRequest>>>,
    told_of_redo: Arc<std::sync::atomic::AtomicBool>,
}

impl DeepWork {
    /// What stops a new update, or `None` while another runs.
    fn begin(&self) -> Option<CancellationToken> {
        let mut running = self.lock();
        if running.is_some() {
            return None;
        }
        let stop = CancellationToken::new();
        *running = Some(stop.clone());
        Some(stop)
    }

    fn end(&self) {
        *self.lock() = None;
    }

    /// True the first time only, so the notice that a map waits to be made again shows once.
    fn tell_once_of_redo(&self) -> bool {
        !self
            .told_of_redo
            .swap(true, std::sync::atomic::Ordering::Relaxed)
    }

    fn stop(&self) {
        if let Some(stop) = self.lock().as_ref() {
            stop.cancel();
        }
    }

    /// The tab asked for `request`'s map.
    pub(crate) fn showing(&self, request: MapRequest) {
        *self
            .shown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request);
    }

    fn shown(&self) -> Option<MapRequest> {
        *self
            .shown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<CancellationToken>> {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Summaries as the app's settings, servers and sessions make them.
fn maps(ctx: &Ctx) -> DeepMaps {
    let env = &ctx.env;
    DeepMaps::new(
        env.paths.clone(),
        ctx.settings().clone(),
        ctx.services.registry.clone(),
        Some(RepoModels::new(&env.repo_root, &env.state_dir)),
        project_of(&env.state_dir),
    )
    .with_workers(ctx.services.workers.clone())
}

/// Answers `DeepOffer` once this machine's servers have said what they serve.
/// With `start`, the server installed here that you chose to start goes first, and the offer
/// says when it didn't answer in time.
pub(crate) fn offer(ctx: &Ctx, start: Option<String>) -> anyhow::Result<Vec<Effect>> {
    let maps = maps(ctx);
    let out = ctx.out.clone();
    tokio::spawn(async move {
        let started = match start {
            Some(runtime) => Some(maps.start_server(&runtime).await),
            None => None,
        };
        let mut offer = maps.offer().await;
        if started == Some(false)
            && let Some(stopped) = offer.open.stopped.as_mut()
        {
            stopped.tried = true;
        }
        out.send(AppEvent::View {
            data: Box::new(ViewData::DeepOffer {
                offer: Box::new(offer),
            }),
        });
    });
    Ok(Vec::new())
}

/// Starts the update `start` asks for.
pub(crate) fn start(ctx: &Ctx, start: DeepStart) -> anyhow::Result<Vec<Effect>> {
    let Some(stop) = ctx.deep.begin() else {
        ctx.notice(NoteLevel::Info, ALREADY_RUNNING);
        return Ok(Vec::new());
    };
    let job = Job::of(ctx);
    tokio::spawn(job.run(start, stop));
    Ok(Vec::new())
}

/// `u` on Summaries: the map's own summarizer adds the new actions at once when it runs on
/// this computer, since nothing leaves it. Otherwise the offer goes to the tab, whose
/// questions ask before sending them to a plan, or say the summarizer isn't answering. It
/// never redoes the map with another model.
pub(crate) fn update(ctx: &Ctx) -> anyhow::Result<Vec<Effect>> {
    let job = Job::of(ctx);
    tokio::spawn(async move {
        let offer = job.maps.offer().await;
        let Some(start) = its_own_update(&offer) else {
            job.out.send(AppEvent::View {
                data: Box::new(ViewData::DeepOffer {
                    offer: Box::new(offer),
                }),
            });
            return;
        };
        let Some(stop) = job.work.begin() else {
            job.out.send(AppEvent::Notice {
                level: NoteLevel::Info,
                text: ALREADY_RUNNING.to_owned(),
            });
            return;
        };
        job.run(start, stop).await;
    });
    Ok(Vec::new())
}

/// The update the map's own summarizer makes without asking: it answers on this computer and
/// would add to the map rather than make it again.
fn its_own_update(offer: &DeepOffer) -> Option<DeepStart> {
    let made_by = offer.made_by.as_ref()?;
    let summarizer = offer
        .summarizers
        .iter()
        .find(|summarizer| summarizer.model == made_by.model)?;
    (summarizer.local() && !made_by.redone_by(&made_by.model)).then(|| DeepStart {
        scope: DeepScope::Everything,
        summarizer: made_by.model.clone(),
        download_embedder: false,
        redo: false,
        download_summarizer: false,
    })
}

/// What an update the tab asked for reports to.
struct Job {
    maps: DeepMaps,
    out: Outbox,
    work: DeepWork,
    internal: tokio::sync::mpsc::UnboundedSender<Internal>,
}

impl Job {
    fn of(ctx: &Ctx) -> Job {
        Job {
            maps: maps(ctx),
            out: ctx.out.clone(),
            work: ctx.deep.clone(),
            internal: ctx.internal.clone(),
        }
    }

    async fn run(self, start: DeepStart, stop: CancellationToken) {
        let Job {
            maps,
            out,
            work,
            internal,
        } = self;
        let mut progress = progress_to(out.clone());
        // The first dots placed bring the tab to Summaries; later ones fill them in.
        let mut first = true;
        let mut placed = {
            let internal = internal.clone();
            move || {
                let _ = internal.send(Internal::DeepMapMade { deeper: first });
                first = false;
            }
        };
        let done = maps
            .update(&start, &stop, &mut progress, &mut placed)
            .await
            .unwrap_or_else(failed);
        work.end();
        // Only a new map brings the tab to Summaries; one that failed leaves it where it was.
        let made = done.new_places > 0;
        tell_unread(&out, &done);
        out.send(AppEvent::DeepDone { done });
        let _ = internal.send(Internal::DeepMapMade { deeper: made });
    }
}

/// `x`: the update stops after the request it's on, keeping what it finished.
pub(crate) fn stop(ctx: &Ctx) -> anyhow::Result<Vec<Effect>> {
    ctx.deep.stop();
    Ok(Vec::new())
}

/// After a run, with `behaviors.update = "after each run"`, adds the new actions to the map
/// that's there, when its summarizer runs on this machine. It waits for nobody: an update
/// already running covers them, and the screen hears of this one as of any other. The one
/// update's place is taken only once there's an update to make, so `d` meanwhile isn't
/// turned away.
pub(crate) fn after_run(ctx: &Ctx) {
    if ctx.settings().behaviors.update != BehaviorUpdate::AfterEachRun {
        return;
    }
    let maps = maps(ctx);
    let (out, work, internal) = (ctx.out.clone(), ctx.deep.clone(), ctx.internal.clone());
    tokio::spawn(async move {
        let start = match maps.after_run().await {
            Ok(AfterRun::Update(start)) => start,
            Ok(AfterRun::Nothing) => return,
            Ok(AfterRun::RedoWaits) => {
                if work.tell_once_of_redo() {
                    out.send(AppEvent::Notice {
                        level: NoteLevel::Info,
                        text: REDO_WAITS.to_owned(),
                    });
                }
                return;
            }
            Err(err) => {
                out.send(AppEvent::DeepDone { done: failed(err) });
                return;
            }
        };
        let Some(stop) = work.begin() else {
            return;
        };
        let mut progress = progress_to(out.clone());
        let mut placed = {
            let internal = internal.clone();
            move || {
                let _ = internal.send(Internal::DeepMapMade { deeper: false });
            }
        };
        let done = maps
            .update(&start, &stop, &mut progress, &mut placed)
            .await
            .unwrap_or_else(failed);
        work.end();
        tell_unread(&out, &done);
        out.send(AppEvent::DeepDone { done });
        let _ = internal.send(Internal::DeepMapMade { deeper: false });
    });
}

/// An update ended: the map the tab last asked for goes to it again, Summaries when the
/// tab's own `d` made it.
pub(crate) async fn made(ctx: &mut Ctx, summaries: bool) -> anyhow::Result<Vec<Effect>> {
    let Some(mut request) = ctx.deep.shown() else {
        return Ok(Vec::new());
    };
    if summaries {
        request.map = Some(BehaviorMap::Summaries);
        ctx.deep.showing(request);
    }
    crate::views::behaviors::view(ctx, request).await
}

fn progress_to(out: Outbox) -> impl FnMut(DeepProgress) + Send {
    move |progress| out.send(AppEvent::DeepProgress { progress })
}

/// Records that couldn't be read leave their actions out of the update, and their dots where
/// they were, which you're told with the first folder or log and why.
fn tell_unread(out: &Outbox, done: &DeepDone) {
    let Some(first) = done.unread.first() else {
        return;
    };
    let more = match done.unread.len() - 1 {
        0 => String::new(),
        1 => " and 1 more".to_owned(),
        more => format!(" and {more} more"),
    };
    out.send(AppEvent::Notice {
        level: NoteLevel::Warn,
        text: format!(
            "Summaries couldn't read {first}{more}, so the update left those actions out and \
             kept their dots."
        ),
    });
}

fn failed(err: anyhow::Error) -> DeepDone {
    DeepDone {
        failed: Some(format!("{err:#}")),
        ..DeepDone::default()
    }
}

#[cfg(test)]
mod tests;
