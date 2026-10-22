//! The deeper map from inside the app: what `d` on the Behaviors tab asks, the update its
//! answers start, in the background with its progress on screen, `x` stopping it, and the
//! update after each run. One update runs at a time.

use std::sync::{Arc, Mutex};

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::behavior_map::MapRequest;
use argonauts_core::deep_map::{DeepDone, DeepProgress, DeepStart};
use argonauts_core::settings::behaviors::{BehaviorMap, BehaviorUpdate};
use tokio_util::sync::CancellationToken;

use super::gather::project_of;
use super::service::{AfterRun, DeepMaps};
use crate::controller::{Ctx, Effect, Internal, Outbox};
use crate::host::clis::RepoModels;

const ALREADY_RUNNING: &str =
    "The deeper map is already being made. x on the Behaviors tab stops it.";

/// Said once, after the first run that would have added to a map waiting to be made again.
const REDO_WAITS: &str = "The deeper map takes no new actions until it's made again with the \
    summarizer and prompt it now asks for. d on the Behaviors tab asks.";

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

/// The deeper map as the app's settings, servers and sessions make it.
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
pub(crate) fn offer(ctx: &Ctx) -> anyhow::Result<Vec<Effect>> {
    let maps = maps(ctx);
    let out = ctx.out.clone();
    tokio::spawn(async move {
        let offer = maps.offer().await;
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
    let maps = maps(ctx);
    let (out, work, internal) = (ctx.out.clone(), ctx.deep.clone(), ctx.internal.clone());
    tokio::spawn(async move {
        let mut progress = progress_to(out.clone());
        // The first dots placed bring the tab to the deeper map; later ones fill it in.
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
        out.send(AppEvent::DeepDone { done });
        let _ = internal.send(Internal::DeepMapMade { deeper: true });
    });
    Ok(Vec::new())
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
        out.send(AppEvent::DeepDone { done });
        let _ = internal.send(Internal::DeepMapMade { deeper: false });
    });
}

/// An update ended: the map the tab last asked for goes to it again, the deeper one when the
/// tab's own `d` made it.
pub(crate) async fn made(ctx: &mut Ctx, deeper: bool) -> anyhow::Result<Vec<Effect>> {
    let Some(mut request) = ctx.deep.shown() else {
        return Ok(Vec::new());
    };
    if deeper {
        request.map = Some(BehaviorMap::Deeper);
        ctx.deep.showing(request);
    }
    crate::views::behaviors::view(ctx, request).await
}

fn progress_to(out: Outbox) -> impl FnMut(DeepProgress) + Send {
    move |progress| out.send(AppEvent::DeepProgress { progress })
}

fn failed(err: anyhow::Error) -> DeepDone {
    DeepDone {
        failed: Some(format!("{err:#}")),
        ..DeepDone::default()
    }
}

#[cfg(test)]
#[path = "tab_tests.rs"]
mod tests;
