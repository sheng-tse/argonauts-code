//! The engine: rebuild the state from the log, recover, then loop over commands, sessions,
//! policy outcomes, vendor events, and shutdown while the run's workflow runs beside it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, MissedTickBehavior};

use argonauts_core::engine::{EngineCommand, EngineContext, EngineExit, RunEngine};
use argonauts_core::events::run::{ParkReason, RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::{RunEventKind, VendorEvent};
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::QuotaStatus;
use argonauts_core::log::{Note, NoteLevel};
use argonauts_core::names::{VendorId, WorkerRef, WorkflowId};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::settings::Settings;

use crate::ENGINE_NOT_BUILT;
use crate::engine::host::Host;
use crate::engine::shared::{Admission, PolicyResult, Shared};
use crate::error::{EngineError, EngineResult};
use crate::git::GitError;
use crate::goal::pursue;
use crate::state::RunState;
use crate::state::ids;
use crate::workflow::{Flow, change, naming, runner, shape};
use crate::{checkpoints, closing, handoff, retro};

pub mod commands;
pub mod host;
pub mod journal;
pub mod live;
pub mod park;
pub mod recover;
pub mod run_meter;
pub mod shared;
pub mod snapshot;
pub(crate) mod stand_in;
pub mod tasks;

/// How often held text and reasoning deltas are checked for being due.
const DELTA_TICK: Duration = Duration::from_millis(100);

/// How long sessions and policy evaluations still running when the run parks or finishes get
/// to end and record it.
const WIND_DOWN_GRACE: Duration = Duration::from_secs(10);

/// One engine for every workflow: solo, the crews, and the baselines differ only in settings.
pub struct CrewEngine {
    mode: RunMode,
}

impl CrewEngine {
    pub fn new(settings: &Settings, mode: RunMode) -> Self {
        let _ = settings;
        Self { mode }
    }

    pub fn mode(&self) -> RunMode {
        self.mode
    }
}

impl RunEngine for CrewEngine {
    fn run(self: Box<Self>, context: EngineContext) -> BoxFuture<'static, EngineExit> {
        Box::pin(async move {
            let sink = context.sink.clone();
            let not_built = match drive(*self, context).await {
                Ok(exit) => return exit,
                Err(EngineError::NotBuilt(what) | EngineError::Git(GitError::NotBuilt(what))) => {
                    what
                }
                Err(err) => return EngineExit::Crashed(err.to_string()),
            };
            let finished = RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::Failed,
                detail: Some(format!("{ENGINE_NOT_BUILT} ({not_built})")),
            });
            match sink.record(None, None, "conductor", finished) {
                Ok(_) => EngineExit::Finished(RunOutcome::Failed),
                Err(err) => EngineExit::Crashed(err.to_string()),
            }
        })
    }
}

/// Rebuild the state from `history`, recover, start or re-enter the run's workflow, then loop
/// until the run finishes or parks. A finished run ends at once, unless it was stopped and you
/// reopened it.
async fn drive(engine: CrewEngine, mut context: EngineContext) -> EngineResult<EngineExit> {
    let state = RunState::rebuild(&context.history)?;
    let reopened = context.reopen && state.run.finished == Some(RunOutcome::Stopped);
    if let Some(outcome) = state.run.finished.filter(|_| !reopened) {
        let host = Host::new(Arc::new(Shared::new(&context, state)?));
        while let Ok(command) = context.inbox.try_recv() {
            commands::after_finish(&host, command);
        }
        return Ok(EngineExit::Finished(outcome));
    }
    let inbox = std::mem::replace(&mut context.inbox, mpsc::channel(1).1);
    let fresh = context.vendor_events.resubscribe();
    let vendor_events = std::mem::replace(&mut context.vendor_events, fresh);
    let mode = state.run.mode.unwrap_or(engine.mode);
    let mut shared = Shared::new(&context, state)?;
    shared.mode = mode;
    drop(context);
    let shared = Arc::new(shared);
    let host = Host::new(shared.clone());
    let policy = shared
        .take_policy_inbox()
        .ok_or_else(|| EngineError::Refused("the policy inbox was already taken".to_owned()))?;

    ensure_base_worktree(&host).await?;
    recover::recover(&host).await?;
    change::apply_decided(&host)?;
    pursue::on_resume(&host)?;

    // A reset that passed before this process started was acted on then, or by recovery.
    let returned = host.read(|state| state.run.quota_reset_passed(host.now()));
    let mut driver = Loop {
        workflow: Some(tokio::spawn(root_workflow(host.clone()))),
        host,
        commands: JoinSet::new(),
        park_at: None,
        reenter: false,
        returned: returned.into_iter().collect(),
    };
    let exit = driver.run(inbox, vendor_events, policy).await;
    driver.quiesce().await;
    exit
}

/// The run's read-only sessions work in a detached worktree at its base commit.
async fn ensure_base_worktree(host: &Host) -> EngineResult<()> {
    let base = &host.repo().base_worktree;
    if base.exists() {
        return Ok(());
    }
    let commit = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .map(|started| started.base_commit.clone())
    });
    let Some(commit) = commit else {
        return Ok(());
    };
    if let Some(parent) = base.parent() {
        create_private_dir_all(parent)?;
    }
    host.git()
        .add_detached_worktree(&host.repo().root, base, &commit)
        .await?;
    Ok(())
}

/// Runs the run's own workflow from where the log stopped, follows switches to another
/// workflow, then runs the retro and writes the closing answer unless you stopped the run. A
/// new run's workflow is the lead's design when `workflows.lead_designs` is on, else the
/// preset's. A failed retro leaves the outcome as it is.
async fn root_workflow(host: Host) -> EngineResult<RunOutcome> {
    let first = match host.read(newest_top_level) {
        Some(id) => Ok(id),
        None => first_workflow(&host).await?,
    };
    let mut flow = match first {
        Ok(id) => runner::run(&host, &id).await?,
        Err(flow) => flow,
    };
    let outcome = loop {
        match flow {
            Flow::Continue => break RunOutcome::BranchLeft,
            Flow::Finish(outcome) => break outcome,
            Flow::Switch(to) => {
                let workflow =
                    naming::as_run(&host, Some(&to), runner::lookup(&host, to.as_str())?);
                let workflow = shape::as_written(&host, workflow);
                let id = runner::start(&host, Some(to), workflow, None)?;
                flow = runner::run(&host, &id).await?;
            }
        }
    };
    // A stopped run's answer comes from code alone, when it finishes.
    if outcome != RunOutcome::Stopped {
        retro::run_or_note(&host).await?;
        closing::record(&host, outcome, None).await?;
    }
    Ok(outcome)
}

/// Starts the lead's workflow, or the preset's, as code shapes it for the run: Understand first
/// when it runs, the checks, and a goal's attempts (`shape`). `Err` holds how the run ends
/// before any workflow starts.
async fn first_workflow(host: &Host) -> EngineResult<Result<WorkflowId, Flow>> {
    naming::name_from_intake(host)?;
    let (from, workflow) = match change::design_for_task(host).await? {
        Some(designed) => designed,
        None => {
            let (from, workflow) = runner::workflow_for_run(host)?;
            let workflow = naming::as_run(host, from.as_ref(), workflow);
            (from, shape::as_written(host, workflow))
        }
    };
    let id = runner::start(host, from, workflow, None)?;
    note_reuse(host, &id)?;
    Ok(Ok(id))
}

/// Says so in the log when a replay's asks before its phase take what the run its task came
/// from answered, which happens only when its workflow starts with the same phases.
fn note_reuse(host: &Host, id: &WorkflowId) -> EngineResult<()> {
    let Some(recorded) = host
        .shared()
        .replay
        .as_ref()
        .and_then(|terms| terms.reuse.as_ref())
    else {
        return Ok(());
    };
    let fits = host.read(|state| {
        state
            .workflows
            .by_id
            .get(id)
            .is_some_and(|workflow| recorded.fits(&workflow.workflow))
    });
    if fits {
        let phase = serde_json::to_value(recorded.from)
            .ok()
            .and_then(|phase| phase.as_str().map(str::to_owned))
            .unwrap_or_default();
        let note = Note {
            level: NoteLevel::Info,
            text: format!(
                "This replay reuses what {} answered before its {phase} phase.",
                recorded.run
            ),
        };
        host.record(None, None, RunEventKind::Note(note))?;
    }
    Ok(())
}

/// The run's own workflow as it runs now: after a switch, the newest one without a parent.
fn newest_top_level(state: &RunState) -> Option<WorkflowId> {
    state
        .workflows
        .by_id
        .values()
        .filter(|workflow| workflow.parent.is_none())
        .max_by_key(|workflow| ids::number(workflow.id.as_str()))
        .map(|workflow| workflow.id.clone())
        .or_else(|| state.workflows.root.clone())
}

struct Loop {
    host: Host,
    /// `None` after a pause cut the workflow short, until the run parks.
    workflow: Option<JoinHandle<EngineResult<RunOutcome>>>,
    /// Long commands, run beside the loop.
    commands: JoinSet<()>,
    /// When the run parks if nothing changes, after `supervisor.park_grace`.
    park_at: Option<Instant>,
    /// A pause or a stop cut the workflow short; it's entered again once admission reopens.
    reenter: bool,
    /// Vendors whose return was acted on, until they go out again, so a quota reset passing and
    /// the health board's later word don't both announce it.
    returned: BTreeSet<VendorId>,
}

impl Loop {
    async fn run(
        &mut self,
        mut inbox: mpsc::Receiver<EngineCommand>,
        mut vendor_events: broadcast::Receiver<VendorEvent>,
        mut policy: mpsc::UnboundedReceiver<PolicyResult>,
    ) -> EngineResult<EngineExit> {
        let host = self.host.clone();
        let shared = host.shared();
        let shutdown = shared.shutdown.clone();
        let mut inbox_open = true;
        let mut vendors_open = true;
        let mut deltas = tokio::time::interval(DELTA_TICK);
        deltas.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut park_due = false;
        loop {
            if let Some(exit) = self.settle(park_due).await? {
                return Ok(exit);
            }
            self.reenter_if_open();
            park_due = false;
            let park_at = self.park_at;
            let quota_reset = self.watch_quota_resets();
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return self.shut_down().await,
                joined = workflow_end(&mut self.workflow), if self.workflow.is_some() => {
                    self.workflow = None;
                    if let Some(exit) = self.workflow_ended(joined).await? {
                        return Ok(exit);
                    }
                }
                command = inbox.recv(), if inbox_open => match command {
                    Some(command) => self.command(command).await?,
                    None => inbox_open = false,
                },
                outcome = policy.recv() => {
                    if let Some((id, outcome)) = outcome {
                        checkpoints::policy_outcome(&self.host, &id, outcome)?;
                    }
                }
                event = vendor_events.recv(), if vendors_open => match event {
                    Ok(event) => self.vendor_event(event)?,
                    Err(broadcast::error::RecvError::Lagged(_)) => self.reread_vendors()?,
                    Err(broadcast::error::RecvError::Closed) => vendors_open = false,
                },
                Some(done) = self.commands.join_next(), if !self.commands.is_empty() => {
                    if let Err(err) = done
                        && err.is_panic()
                    {
                        self.note(NoteLevel::Error, &format!("a command panicked: {err}"));
                    }
                }
                () = shared.changed.notified() => {}
                _ = deltas.tick() => self.host.flush_due_deltas()?,
                () = sleep_until(park_at), if park_at.is_some() => park_due = true,
                // Only wakes the loop: the next turn sees the reset passed.
                () = sleep_until(quota_reset), if quota_reset.is_some() => {}
            }
        }
    }

    /// Finishes a stopped run, parks a paused one, or arms or fires the park timer.
    async fn settle(&mut self, park_due: bool) -> EngineResult<Option<EngineExit>> {
        let host = &self.host;
        let shared = host.shared();
        let all_out = shared.services.limits.all_out(&crew(host.settings()));
        let (stopping, reason) = host.read(|state| {
            (
                state.run.stop_requested,
                park::reason(state, shared.live.len(), all_out),
            )
        });
        if stopping {
            if shared.live.is_empty() && self.commands.is_empty() {
                return self.finish(RunOutcome::Stopped, None).await.map(Some);
            }
            return Ok(None);
        }
        match reason {
            // The pause parks as soon as its own task has committed and recorded `Paused`, and
            // every session has ended.
            Some(ParkReason::Paused) if self.commands.is_empty() && shared.live.is_empty() => {
                self.park(ParkReason::Paused).await.map(Some)
            }
            Some(ParkReason::Paused) => Ok(None),
            Some(reason) if park_due => self.park(reason).await.map(Some),
            Some(_) => {
                if self.park_at.is_none() {
                    let grace = host
                        .settings()
                        .supervisor
                        .park_grace
                        .parse()
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    self.park_at = Some(Instant::now() + grace);
                }
                Ok(None)
            }
            None => {
                self.park_at = None;
                Ok(None)
            }
        }
    }

    async fn workflow_ended(
        &mut self,
        joined: Result<EngineResult<RunOutcome>, tokio::task::JoinError>,
    ) -> EngineResult<Option<EngineExit>> {
        let result = match joined {
            Ok(result) => result,
            Err(err) => {
                return Ok(Some(EngineExit::Crashed(format!(
                    "the workflow task failed: {err}"
                ))));
            }
        };
        match result {
            Ok(outcome) => self.finish(outcome, None).await.map(Some),
            Err(EngineError::Interrupted) => {
                self.reenter = !self.host.read(|state| state.run.stop_requested);
                Ok(None)
            }
            Err(
                err @ (EngineError::NotBuilt(_) | EngineError::Journal(_) | EngineError::Io(_)),
            ) => Err(err),
            Err(EngineError::Git(GitError::NotBuilt(what))) => Err(EngineError::NotBuilt(what)),
            // A vendor error never ends a run: it parks until an agent can take the seat, tries
            // again on its own after a wait that grows with each try, and its note says why.
            Err(EngineError::Stalled(stall)) => {
                let tries = self.host.read(|state| state.run.stand_in_waits);
                let retry = stand_in::retry_after(self.host.settings(), tries);
                let until = stall
                    .resets_at
                    .or_else(|| retry.map(|retry| self.host.now() + retry));
                self.note(NoteLevel::Error, &stand_in::park_line(&stall, retry));
                let detail = stall.detail.clone();
                self.park(ParkReason::NoStandIn { detail, until })
                    .await
                    .map(Some)
            }
            Err(err) => self
                .finish(RunOutcome::Failed, Some(err.to_string()))
                .await
                .map(Some),
        }
    }

    /// A pause that was cancelled cut a step short: the workflow is entered again, and the
    /// step resumes its sessions.
    fn reenter_if_open(&mut self) {
        let host = &self.host;
        let open = host.shared().admission() == Admission::Open
            && !host.read(|state| state.run.stop_requested);
        if self.reenter && self.workflow.is_none() && open {
            self.reenter = false;
            self.workflow = Some(tokio::spawn(root_workflow(host.clone())));
        }
    }

    async fn finish(
        &mut self,
        outcome: RunOutcome,
        detail: Option<String>,
    ) -> EngineResult<EngineExit> {
        self.quiesce().await;
        closing::record_unasked(&self.host, outcome, detail.as_deref()).await?;
        self.host.record(
            None,
            None,
            RunEventKind::Run(RunLifecycle::Finished { outcome, detail }),
        )?;
        Ok(EngineExit::Finished(outcome))
    }

    /// Nothing can happen until something outside changes. The workflow task is dropped; the
    /// next supervisor re-enters it from the log.
    async fn park(&mut self, reason: ParkReason) -> EngineResult<EngineExit> {
        self.quiesce().await;
        pursue::on_park(&self.host, &reason)?;
        let waiting_on = self.host.read(park::waiting_on);
        self.host.record(
            None,
            None,
            RunEventKind::Run(RunLifecycle::Parked {
                reason: reason.clone(),
                waiting_on,
            }),
        )?;
        Ok(EngineExit::Parked(reason))
    }

    /// Shutdown is a pause that lets each session finish its tool call, then a park.
    async fn shut_down(&mut self) -> EngineResult<EngineExit> {
        self.commands.abort_all();
        while self.commands.join_next().await.is_some() {}
        commands::pause(&self.host, false).await?;
        self.park(ParkReason::Paused).await
    }

    async fn stop_workflow(&mut self) {
        if let Some(workflow) = self.workflow.take() {
            workflow.abort();
            let _ = workflow.await;
        }
    }

    /// Stops everything beside the loop, so nothing records after the run parks or finishes,
    /// or after the loop returns.
    /// The workflow and the commands stop first. Sessions are told to stop, and they and the
    /// policy evaluations get a grace to end and record it; whatever is left then is halted.
    async fn quiesce(&mut self) {
        self.stop_workflow().await;
        self.commands.abort_all();
        while self.commands.join_next().await.is_some() {}
        let shared = self.host.shared();
        shared.live.stop_all();
        let deadline = Instant::now() + WIND_DOWN_GRACE;
        let ended = tokio::time::timeout_at(deadline, async {
            shared.live.wait_empty().await;
            shared.tasks.wait().await;
        })
        .await;
        if ended.is_err() {
            tracing::warn!(
                sessions = shared.live.len(),
                tasks = shared.tasks.len(),
                "halting what still runs as the run winds down"
            );
        }
        shared.live.clear();
        shared.halt.cancel();
        shared.tasks.wait().await;
    }

    async fn command(&mut self, command: EngineCommand) -> EngineResult<()> {
        // The pause begins at once, so a `CancelPause` right behind it finds it under way; only
        // the wait for its sessions runs beside the loop.
        if let EngineCommand::Pause { now } = command {
            match commands::begin_pause(&self.host, now) {
                Ok(true) => {
                    let host = self.host.clone();
                    self.commands.spawn(async move {
                        if let Err(err) = commands::finish_pause(&host).await {
                            report(&host, &err);
                        }
                    });
                }
                Ok(false) => {}
                Err(err @ EngineError::Journal(_)) => return Err(err),
                Err(err) => report(&self.host, &err),
            }
            return Ok(());
        }
        if commands::is_long(&command) {
            let host = self.host.clone();
            self.commands.spawn(async move {
                if let Err(err) = commands::handle(&host, command).await {
                    report(&host, &err);
                }
            });
            return Ok(());
        }
        match commands::handle(&self.host, command).await {
            Err(err @ EngineError::Journal(_)) => Err(err),
            Err(err) => {
                report(&self.host, &err);
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }

    /// Records a vendor's change of health or quota, and hands its cards back when it's back.
    fn vendor_event(&mut self, event: VendorEvent) -> EngineResult<()> {
        let vendor = match &event {
            VendorEvent::Quota(reading) => reading.vendor.clone(),
            VendorEvent::Health(health) => health.vendor.clone(),
            VendorEvent::Disabled { vendor, .. } => vendor.clone(),
        };
        let now = self.host.now();
        let (was_out, news) = self.host.read(|state| {
            let run = &state.run;
            let news = match &event {
                VendorEvent::Quota(reading) => {
                    let reset = reading
                        .resets_at
                        .filter(|_| reading.status == QuotaStatus::Rejected)
                        .map(|at| at.unix_timestamp());
                    run.quota.get(&vendor) != Some(&reading.status)
                        || run.quota_resets.get(&vendor).copied() != reset
                }
                VendorEvent::Health(health) => {
                    run.health.get(&vendor) != Some(&health.state)
                        || run.disabled.get(&vendor) != health.disabled.as_ref()
                }
                VendorEvent::Disabled { reason, .. } => run.disabled.get(&vendor) != Some(reason),
            };
            (run.vendor_out_at(&vendor, now), news)
        });
        // Readings repeat often; only a change goes in the log.
        if !news {
            return Ok(());
        }
        self.host.record(None, None, RunEventKind::Vendor(event))?;
        if self
            .host
            .read(|state| state.run.vendor_out_at(&vendor, now))
        {
            self.returned.remove(&vendor);
        } else if was_out {
            self.vendor_back(vendor);
        }
        Ok(())
    }

    /// Acts on each quota that ran out and has reset since, with no word from its vendor: it
    /// can work again, as the handoff planner already counts it, so its cards' original is
    /// back. A vendor out again is forgotten as returned. Returns when the soonest reset still
    /// to come passes, to wake the loop then.
    fn watch_quota_resets(&mut self) -> Option<Instant> {
        let now = self.host.now();
        let (passed, next, out) = self.host.read(|state| {
            let run = &state.run;
            let out: Vec<VendorId> = self
                .returned
                .iter()
                .filter(|vendor| run.vendor_out_at(vendor, now))
                .cloned()
                .collect();
            (run.quota_reset_passed(now), run.next_quota_reset(now), out)
        });
        for vendor in out {
            self.returned.remove(&vendor);
        }
        for vendor in passed {
            self.vendor_back(vendor);
        }
        let at = OffsetDateTime::from_unix_timestamp(next?).ok()?;
        let wait = Duration::try_from(at - now).unwrap_or_default();
        Some(Instant::now() + wait)
    }

    /// Events were missed, so the boards are read again before anything routes.
    fn reread_vendors(&mut self) -> EngineResult<()> {
        let services = self.host.services().clone();
        for health in services.health.all() {
            self.vendor_event(VendorEvent::Health(health))?;
        }
        for reading in services.quota.all() {
            self.vendor_event(VendorEvent::Quota(reading))?;
        }
        Ok(())
    }

    fn vendor_back(&mut self, vendor: VendorId) {
        if !self.returned.insert(vendor.clone()) {
            return;
        }
        let host = self.host.clone();
        self.commands.spawn(async move {
            if let Err(err) = handoff::on_return::vendor_back(&host, &vendor).await {
                report(&host, &err);
            }
        });
    }

    fn note(&self, level: NoteLevel, text: &str) {
        note(&self.host, level, text);
    }
}

/// Every worker the run may use: the crew and the default agent.
fn crew(settings: &Settings) -> Vec<WorkerRef> {
    let mut workers = settings.crew.clone();
    if !workers.contains(&settings.default_agent) {
        workers.push(settings.default_agent.clone());
    }
    workers
}

/// A command or task that failed without stopping the run goes in the log for you.
fn report(host: &Host, err: &EngineError) {
    note(host, NoteLevel::Error, &err.to_string());
}

fn note(host: &Host, level: NoteLevel, text: &str) {
    let note = Note {
        level,
        text: text.to_owned(),
    };
    if let Err(err) = host.record(None, None, RunEventKind::Note(note)) {
        tracing::error!(%err, text, "note not recorded");
    }
}

async fn workflow_end(
    workflow: &mut Option<JoinHandle<EngineResult<RunOutcome>>>,
) -> Result<EngineResult<RunOutcome>, tokio::task::JoinError> {
    match workflow {
        Some(workflow) => workflow.await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crew_includes_the_default_agent_once() {
        let mut settings = Settings {
            crew: vec![WorkerRef::new("codex", None)],
            default_agent: WorkerRef::new("claude-code", None),
            ..Settings::default()
        };
        assert_eq!(crew(&settings).len(), 2);
        settings.crew.push(WorkerRef::new("claude-code", None));
        assert_eq!(crew(&settings).len(), 2);
    }
}
