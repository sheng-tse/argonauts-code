//! The engine: rebuild the state from the log, recover, then loop over commands, sessions,
//! policy outcomes, agent events, and shutdown while the run's workflow runs beside it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, MissedTickBehavior};

use argonauts_core::engine::{EngineCommand, EngineContext, EngineExit, RunEngine};
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome, WaitReason};
use argonauts_core::events::{AgentStatusEvent, RunEventKind};
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{ModelState, PlanUsageStatus};
use argonauts_core::log::{Note, NoteLevel};
use argonauts_core::names::{AgentId, ModelRef, WorkflowId};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::settings::Settings;

use crate::ENGINE_NOT_BUILT;
use crate::engine::host::Host;
use crate::engine::shared::{Admission, PolicyResult, Shared, lock};
use crate::error::{EngineError, EngineResult};
use crate::git::GitError;
use crate::goal::pursue;
use crate::state::RunState;
use crate::state::ids;
use crate::workflow::{Flow, change, naming, runner, shape};
use crate::{approvals, closing, fallback, run_review};

pub mod commands;
pub mod host;
pub(crate) mod job_fallback;
pub mod journal;
pub mod live;
pub mod outside_writes;
pub mod recover;
pub mod run_meter;
pub mod shared;
pub mod snapshot;
pub mod tasks;
pub mod wait;

/// How often held text and reasoning deltas are checked for being due.
const DELTA_TICK: Duration = Duration::from_millis(100);

/// How long sessions and policy evaluations still running when the run waits or finishes get
/// to end and record it.
const WIND_DOWN_GRACE: Duration = Duration::from_secs(10);

/// One engine for every workflow: Captain, the crews, and the baselines are different only in
/// settings.
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
            match sink.record(None, None, "argonauts-code", finished) {
                Ok(_) => EngineExit::Finished(RunOutcome::Failed),
                Err(err) => EngineExit::Crashed(err.to_string()),
            }
        })
    }
}

/// Rebuild the state from `history`, recover, start or re-enter the run's workflow, then loop
/// until the run finishes or waits. A finished run ends at once, unless it was stopped and you
/// reopened it.
async fn drive(engine: CrewEngine, mut context: EngineContext) -> EngineResult<EngineExit> {
    let state = RunState::rebuild(&context.history)?;
    let reopened = context.reopen && state.run.finished == Some(RunOutcome::Stopped);
    // A pull request the run opened can have a round of its follow-up to work.
    let follow_up = crate::follow::round::due(context.repo.state_dir(), &context.run).is_some();
    if let Some(outcome) = state.run.finished.filter(|_| !reopened && !follow_up) {
        let host = Host::new(Arc::new(Shared::new(&context, state)?));
        while let Ok(command) = context.inbox.try_recv() {
            commands::after_finish(&host, command);
        }
        return Ok(EngineExit::Finished(outcome));
    }
    let mut inbox = std::mem::replace(&mut context.inbox, mpsc::channel(1).1);
    // The requests the run host saved while the run waited are here already. Whether one of
    // them answers an approval tells an answer's wake from one of your commands.
    let mut queued = Vec::new();
    while let Ok(command) = inbox.try_recv() {
        queued.push(command);
    }
    let answers = queued
        .iter()
        .any(|command| matches!(command, EngineCommand::Answer { .. }));
    let fresh = context.agent_events.resubscribe();
    let agent_events = std::mem::replace(&mut context.agent_events, fresh);
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
    recover::recover(&host, answers).await?;
    change::apply_decided(&host)?;
    pursue::on_resume(&host)?;

    // A reset that passed before this process started was acted on then, or by recovery.
    let returned = host.read(|state| state.run.plan_usage_reset_passed(host.now()));
    let mut driver = Loop {
        workflow: Some(tokio::spawn(root_workflow(host.clone()))),
        host,
        commands: JoinSet::new(),
        wait_at: None,
        reenter: false,
        returned: returned.into_iter().collect(),
    };
    let exit = driver.run(queued, inbox, agent_events, policy).await;
    driver.quiesce(Cut::ForGood).await;
    exit
}

/// How long the sessions a winding-down run stops are stopped for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    /// The run is over, or its loop is.
    ForGood,
    /// The run waits and goes on later.
    UntilTheRunWakes,
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

/// Runs the run's own workflow from where the log stopped, follows switches to another workflow,
/// then runs the run review and writes the closing answer unless you stopped the run. A round
/// of its pull request's follow-up, once due, runs in its place. A new run's
/// workflow is the main agent's design when `workflows.orchestrator_designs` is on, else the
/// chosen workflow as it is. A failed run review leaves the outcome as it is.
async fn root_workflow(host: Host) -> EngineResult<RunOutcome> {
    if let Some(followed) = crate::follow::round::due(host.repo().state_dir(), host.run_id()) {
        return crate::follow::round::run(host, followed).await;
    }
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
        run_review::run_or_note(&host).await?;
        closing::record(&host, outcome, None).await?;
    }
    Ok(outcome)
}

/// Starts the main agent's design, or the chosen workflow, as code shapes it for the run:
/// Understand first when it runs, the checks, and a goal's tries (`shape`). `Err` holds how the
/// run ends before any workflow starts.
async fn first_workflow(host: &Host) -> EngineResult<Result<WorkflowId, Flow>> {
    naming::name_from_intake(host)?;
    // A workflow with steps of its own is run by the runner, as one step before the merge.
    if crate::script::for_run(host)?.is_some() {
        let (from, workflow) = crate::script::phases(host)?;
        return Ok(Ok(runner::start(host, Some(from), workflow, None)?));
    }
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
    /// `None` after a pause cut the workflow short, until the run waits.
    workflow: Option<JoinHandle<EngineResult<RunOutcome>>>,
    /// Long commands, run beside the loop.
    commands: JoinSet<()>,
    /// When the run waits if nothing changes, after `run_host.wait_grace`.
    wait_at: Option<Instant>,
    /// A pause or a stop cut the workflow short; it's entered again once admission reopens.
    reenter: bool,
    /// Agents whose return was acted on, until they go out again, so a plan usage reset passing and
    /// the health board's later word don't both announce it.
    returned: BTreeSet<AgentId>,
}

impl Loop {
    async fn run(
        &mut self,
        queued: Vec<EngineCommand>,
        mut inbox: mpsc::Receiver<EngineCommand>,
        mut agent_events: broadcast::Receiver<AgentStatusEvent>,
        mut policy: mpsc::UnboundedReceiver<PolicyResult>,
    ) -> EngineResult<EngineExit> {
        for command in queued {
            self.command(command).await?;
        }
        let host = self.host.clone();
        let shared = host.shared();
        let shutdown = shared.shutdown.clone();
        let mut inbox_open = true;
        let mut agents_open = true;
        let mut deltas = tokio::time::interval(DELTA_TICK);
        deltas.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut wait_due = false;
        loop {
            if let Some(exit) = self.settle(wait_due).await? {
                return Ok(exit);
            }
            self.reenter_if_open();
            wait_due = false;
            let wait_at = self.wait_at;
            let plan_usage_reset = self.watch_plan_usage_resets();
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
                        approvals::policy_outcome(&self.host, &id, outcome)?;
                    }
                }
                event = agent_events.recv(), if agents_open => match event {
                    Ok(event) => self.agent_event(event)?,
                    Err(broadcast::error::RecvError::Lagged(_)) => self.reread_agents()?,
                    Err(broadcast::error::RecvError::Closed) => agents_open = false,
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
                () = sleep_until(wait_at), if wait_at.is_some() => wait_due = true,
                // Only wakes the loop: the next turn sees the reset passed.
                () = sleep_until(plan_usage_reset), if plan_usage_reset.is_some() => {}
            }
        }
    }

    /// Finishes a stopped run, has a paused one wait, or arms or fires the wait timer.
    async fn settle(&mut self, wait_due: bool) -> EngineResult<Option<EngineExit>> {
        let host = &self.host;
        let shared = host.shared();
        let all_out = shared.services.limits.all_out(&crew(host.settings()));
        // A goal's check you asked for holds the wait the way a session at work does, and so
        // does an approval the policy is still weighing.
        let at_work =
            shared.live.len() + lock(&shared.checking_goals).len() + shared.desk.weighing();
        let (stopping, reason) = host.read(|state| {
            (
                state.run.stop_requested,
                wait::reason(state, at_work, all_out),
            )
        });
        if stopping {
            if shared.live.is_empty() && self.commands.is_empty() {
                return self.finish(RunOutcome::Stopped, None).await.map(Some);
            }
            return Ok(None);
        }
        match reason {
            // A paused run waits as soon as the pause's own task has committed and recorded
            // `Paused`, and every session has ended.
            Some(WaitReason::Paused) if self.commands.is_empty() && shared.live.is_empty() => {
                self.wait(WaitReason::Paused).await.map(Some)
            }
            Some(WaitReason::Paused) => Ok(None),
            Some(reason) if wait_due => self.wait(reason).await.map(Some),
            Some(_) => {
                if self.wait_at.is_none() {
                    let grace = host
                        .settings()
                        .run_host
                        .wait_grace
                        .parse()
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    self.wait_at = Some(Instant::now() + grace);
                }
                Ok(None)
            }
            None => {
                self.wait_at = None;
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
            Ok(outcome) => {
                let detail = self.host.read(|state| why_it_ended(state, outcome));
                self.finish(outcome, detail).await.map(Some)
            }
            Err(EngineError::Interrupted) => {
                self.reenter = !self.host.read(|state| state.run.stop_requested);
                Ok(None)
            }
            Err(
                err @ (EngineError::NotBuilt(_) | EngineError::Journal(_) | EngineError::Io(_)),
            ) => Err(err),
            Err(EngineError::Git(GitError::NotBuilt(what))) => Err(EngineError::NotBuilt(what)),
            // Nothing can take a job the run needs. Waiting can't bring a model or a login
            // back, so the run stops fully, its work kept, and only `/resume` carries it on.
            Err(EngineError::Stalled(stall)) => {
                // Where a setting of yours is all that keeps the job on its CLI, the question
                // about the CLI offers to move it.
                if let Some(kept) = &stall.kept {
                    crate::fallback::down::job_waits(&self.host, kept)?;
                    // An agent you picked while the job was still trying its CLI takes it now,
                    // and the workflow goes on. Freed to the agent that just stopped, it waits.
                    let freed_to_another = self.host.read(|state| {
                        let waits = state.run.waiting_jobs.iter().any(|w| w.job == kept.job);
                        let to = state.run.freed(&kept.job).flatten();
                        !waits && to.is_some_and(|to| *to != kept.stopped)
                    });
                    if freed_to_another {
                        self.reenter = true;
                        return Ok(None);
                    }
                }
                // A review or a ruling that only its builder's provider is left to take waits
                // for the agent it was asked of instead, and its question asks you.
                if let Some(agent_id) = stall.waits_for_its_agent() {
                    self.note(NoteLevel::Error, &stall.line);
                    let tries = self.host.read(|state| state.run.fallback_waits);
                    let retry = job_fallback::retry_after(self.host.settings(), tries);
                    let until = stall
                        .resets_at
                        .or_else(|| retry.map(|retry| self.host.now() + retry));
                    return self
                        .wait(WaitReason::WaitingForAgent { agent_id, until })
                        .await
                        .map(Some);
                }
                let stop = Some(Box::new(stall.run_stop()));
                let detail = stall.detail.clone();
                self.wait(WaitReason::NoFallback {
                    detail,
                    until: None,
                    stop,
                })
                .await
                .map(Some)
            }
            Err(err) => self
                .finish(RunOutcome::Failed, Some(err.run_detail()))
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
        self.quiesce(Cut::ForGood).await;
        closing::record_unasked(&self.host, outcome, detail.as_deref()).await?;
        crate::merge::copies::remove_at_end(&self.host, outcome).await;
        self.host.record(
            None,
            None,
            RunEventKind::Run(RunLifecycle::Finished { outcome, detail }),
        )?;
        Ok(EngineExit::Finished(outcome))
    }

    /// Nothing can happen until something outside changes. The workflow task is dropped; the
    /// next run host re-enters it from the log.
    async fn wait(&mut self, reason: WaitReason) -> EngineResult<EngineExit> {
        self.quiesce(Cut::UntilTheRunWakes).await;
        pursue::on_wait(&self.host, &reason)?;
        let waiting_on = self.host.read(wait::waiting_on);
        self.host.record(
            None,
            None,
            RunEventKind::Run(RunLifecycle::Waiting {
                reason: reason.clone(),
                waiting_on,
            }),
        )?;
        Ok(EngineExit::Waiting(reason))
    }

    /// Shutdown is a pause that lets each session finish its tool call, then a wait.
    async fn shut_down(&mut self) -> EngineResult<EngineExit> {
        self.commands.abort_all();
        while self.commands.join_next().await.is_some() {}
        commands::pause(&self.host, false).await?;
        self.wait(WaitReason::Paused).await
    }

    async fn stop_workflow(&mut self) {
        if let Some(workflow) = self.workflow.take() {
            workflow.abort();
            let _ = workflow.await;
        }
    }

    /// Stops everything beside the loop, so nothing records after the run waits or finishes,
    /// or after the loop returns.
    /// The workflow and the commands stop first. Sessions are told to stop, and they and the
    /// policy evaluations get a grace to end and record it; whatever is left then is halted.
    ///
    /// A session still at work when the run begins to wait, as one is beside a job nobody can
    /// take, is cut short by the wait and by nothing of its own. It ends `Paused`, so its step
    /// has it go on when the run wakes instead of reading the stop as its answer.
    async fn quiesce(&mut self, cut: Cut) {
        self.stop_workflow().await;
        self.commands.abort_all();
        while self.commands.join_next().await.is_some() {}
        let shared = self.host.shared();
        if cut == Cut::UntilTheRunWakes {
            lock(&shared.pausing).extend(shared.live.keys());
        }
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

    /// Records an agent's change of health or plan usage, and gives its tasks back when it's back.
    fn agent_event(&mut self, event: AgentStatusEvent) -> EngineResult<()> {
        let agent_id = match &event {
            AgentStatusEvent::PlanUsage(reading) => reading.agent_id.clone(),
            AgentStatusEvent::Health(health) => health.agent_id.clone(),
            AgentStatusEvent::Disabled { agent_id, .. } => agent_id.clone(),
            AgentStatusEvent::Model(health) => health.model.agent_id(),
        };
        let now = self.host.now();
        let (was_out, news) = self.host.read(|state| {
            let run = &state.run;
            let news = match &event {
                AgentStatusEvent::PlanUsage(reading) => {
                    let reset = reading
                        .resets_at
                        .filter(|_| reading.status == PlanUsageStatus::Rejected)
                        .map(|at| at.unix_timestamp());
                    run.plan_usage.get(&agent_id) != Some(&reading.status)
                        || run.plan_usage_resets.get(&agent_id).copied() != reset
                }
                AgentStatusEvent::Health(health) => {
                    run.health.get(&agent_id) != Some(&health.state)
                        || run.disabled.get(&agent_id) != health.disabled.as_ref()
                }
                AgentStatusEvent::Disabled { reason, .. } => {
                    run.disabled.get(&agent_id) != Some(reason)
                }
                AgentStatusEvent::Model(health) => {
                    let known = run.model_health.get(&health.model).copied();
                    known.unwrap_or(ModelState::Healthy) != health.state
                }
            };
            (run.agent_out_at(&agent_id, now), news)
        });
        // Readings repeat often; only a change goes in the log.
        if !news {
            return Ok(());
        }
        // A model back from being out, on a CLI that went on working: its tasks are offered
        // back as a CLI's are. Where the CLI was out too, its own return covers its models.
        let model_back = match &event {
            AgentStatusEvent::Model(health) if health.state == ModelState::Healthy => {
                let known = self
                    .host
                    .read(|state| state.run.model_health.get(&health.model).copied());
                matches!(known, Some(ModelState::Out | ModelState::Testing))
                    .then(|| health.model.clone())
            }
            _ => None,
        };
        self.host
            .record(None, None, RunEventKind::AgentStatus(event))?;
        if let Some(model) = model_back.filter(|_| !was_out) {
            self.model_back(model);
        }
        if self
            .host
            .read(|state| state.run.agent_out_at(&agent_id, now))
        {
            self.returned.remove(&agent_id);
        } else if was_out {
            self.agent_back(agent_id);
        }
        Ok(())
    }

    /// Acts on each agent whose plan usage ran out and has reset since, with no word from it: it
    /// can work again, as the fallback planner already counts it, so its tasks' original is
    /// back. An agent out again is forgotten as returned. Returns when the soonest reset still
    /// to come passes, to wake the loop then.
    fn watch_plan_usage_resets(&mut self) -> Option<Instant> {
        let now = self.host.now();
        let (passed, next, out) = self.host.read(|state| {
            let run = &state.run;
            let out: Vec<AgentId> = self
                .returned
                .iter()
                .filter(|agent_id| run.agent_out_at(agent_id, now))
                .cloned()
                .collect();
            (
                run.plan_usage_reset_passed(now),
                run.next_plan_usage_reset(now),
                out,
            )
        });
        for agent_id in out {
            self.returned.remove(&agent_id);
        }
        for agent_id in passed {
            self.agent_back(agent_id);
        }
        let at = OffsetDateTime::from_unix_timestamp(next?).ok()?;
        let wait = Duration::try_from(at - now).unwrap_or_default();
        Some(Instant::now() + wait)
    }

    /// Events were missed, so the boards are read again before anything routes.
    fn reread_agents(&mut self) -> EngineResult<()> {
        let services = self.host.services().clone();
        for health in services.health.all() {
            self.agent_event(AgentStatusEvent::Health(health))?;
        }
        for reading in services.plan_usage.all() {
            self.agent_event(AgentStatusEvent::PlanUsage(reading))?;
        }
        Ok(())
    }

    fn agent_back(&mut self, agent_id: AgentId) {
        if !self.returned.insert(agent_id.clone()) {
            return;
        }
        let host = self.host.clone();
        self.commands.spawn(async move {
            if let Err(err) = fallback::on_return::agent_back(&host, &agent_id).await {
                report(&host, &err);
            }
        });
    }

    fn model_back(&mut self, model: ModelRef) {
        let host = self.host.clone();
        self.commands.spawn(async move {
            if let Err(err) = fallback::on_return::model_back(&host, &model).await {
                report(&host, &err);
            }
        });
    }

    fn note(&self, level: NoteLevel, text: &str) {
        note(&self.host, level, text);
    }
}

/// Every worker the run may use: the crew and the main agent.
/// Why a run that failed or was given up ended, in the words of the step that ended it, such
/// as the merge's "Nothing was merged, since no task passed: …". `None` for any other ending,
/// which says its own.
fn why_it_ended(state: &RunState, outcome: RunOutcome) -> Option<String> {
    if !matches!(outcome, RunOutcome::Failed | RunOutcome::Abandoned) {
        return None;
    }
    state
        .workflows
        .by_id
        .values()
        .flat_map(|record| &record.steps)
        .filter(|progress| progress.summary.is_some())
        .max_by_key(|progress| progress.ended_at)
        .and_then(|progress| progress.summary.clone())
}

fn crew(settings: &Settings) -> Vec<ModelRef> {
    let mut workers = settings.crew.members();
    if !workers.contains(&settings.crew.main) {
        workers.push(settings.crew.main.clone());
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
    fn the_crew_includes_the_main_agent_once() {
        let mut settings = Settings {
            crew: vec![ModelRef::new("codex", None)].into(),
            ..Settings::default()
        };
        assert_eq!(crew(&settings).len(), 2);
        settings.crew.add(ModelRef::new("claude-code", None));
        assert_eq!(crew(&settings).len(), 2);
    }
}
