//! A session run by the adapter's session driver, for agents that speak a two-way protocol. The
//! worker still owns the process: it spawns and sandboxes it, registers its group, logs every
//! frame both ways and every stderr line, meters and watches the events the driver sends, maps
//! controls to driver calls, and runs the stop plan once the driver is done or cancelled. A
//! message the driver never sent is reported.

use std::collections::HashMap;
use std::io;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::{Duration, Instant};

use argonauts_core::driver::{
    DriverControl, DriverEnd, DriverIo, DriverStart, DriverStop, RawFrame, SessionDriver,
};
use argonauts_core::events::session::{
    Delivery, EndReason, LaunchRecord, PromptVia, SessionEnd, SessionEvent, WrapUpVia,
};
use argonauts_core::future::BoxFuture;
use argonauts_core::layout::session_files;
use argonauts_core::worker::{SessionControl, SteerSupport};
use argonauts_core::{AgentEvent, BudgetDimension, MessageStage, StopSignal, write_private_file};
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant as TokioInstant, sleep_until};

use super::context::{Context, Launch};
use super::end::{CancelledBy, from_driver};
use super::group::Group;
use super::launch::input_event;
use super::observe::{Action, Observer};
use super::outbox::Outbox;
use super::request::{ReportFiles, session_server};
use super::session::{Running, announce_start, end_of, not_started, or_far, tick_every};
use super::steer::{Pending, announce};
use super::stop::{Purpose, StopCause, Stopping};
use crate::process::{AgentProcess, OutputLine, ProcessEvent, exit_parts};
use crate::rawlog::RawLog;
use crate::sandbox::permission_scope;

/// After `Cancel`, or after the driver returns, how long the agent gets to exit by itself
/// before the stop plan.
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// How long a driver may take to notice that its agent is gone.
const DRIVER_GRACE: Duration = Duration::from_secs(5);
const AFTER_KILL_GRACE: Duration = Duration::from_millis(500);
/// Stderr lines the driver may leave unread before newer ones skip it. They're logged anyway.
const STDERR_BUFFER: usize = 256;
const DRIVER_CONTROL_BUFFER: usize = 32;

/// A handoff to the driver, by its number, and how the driver says it went out.
type Delivering = BoxFuture<'static, (u64, Result<Delivery, RecvError>)>;

pub async fn run(running: Running, launch: Launch, driver: Arc<dyn SessionDriver>) -> SessionEnd {
    let Running {
        ctx,
        out,
        mut obs,
        raw,
        control,
        early,
        started,
    } = running;
    let raw_path = Some(raw.stdout_path());
    let start = match driver_start(&ctx, &launch) {
        Ok(start) => start,
        Err(message) => {
            return not_started(EndReason::StartFailed { message }, started, raw_path);
        }
    };
    let (process, pipes) = match AgentProcess::spawn_piped(&launch.command) {
        Ok(spawned) => spawned,
        Err(err) => {
            let reason = EndReason::StartFailed {
                message: err.to_string(),
            };
            return not_started(reason, started, raw_path);
        }
    };
    let record = LaunchRecord {
        prompt_via: PromptVia::StreamInput,
        ..launch.record.clone()
    };
    out.session(input_event(
        record.clone(),
        ctx.spec.prompt.clone(),
        Vec::new(),
    ));
    let group = match Group::register(&ctx, &process, &out).await {
        Ok(group) => group,
        Err(message) => {
            drop(process);
            return end_of(
                &obs,
                EndReason::StartFailed { message },
                (None, None),
                started,
                raw_path,
            );
        }
    };
    announce_start(&ctx, &out, &mut obs, &launch);

    let (events_tx, events) = mpsc::unbounded_channel();
    let (frames_tx, frames) = mpsc::unbounded_channel();
    let (stderr_tx, stderr_rx) = mpsc::channel(STDERR_BUFFER);
    let (to_driver, driver_control) = mpsc::channel(DRIVER_CONTROL_BUFFER);
    let io = DriverIo {
        stdin: Box::new(pipes.stdin),
        stdout: Box::new(pipes.stdout),
        stderr_lines: stderr_rx,
        events: events_tx,
        control: driver_control,
        frames: frames_tx,
    };
    let mut driven = Driven {
        ctx,
        out,
        obs,
        raw,
        raw_failed: false,
        control,
        control_open: true,
        process,
        process_open: true,
        exited: false,
        exit: None,
        drain_until: None,
        leftovers_killed: false,
        group: Some(group),
        launch: record,
        to_driver,
        stderr_tx,
        events,
        frames,
        deliveries: FuturesUnordered::new(),
        handed: HashMap::new(),
        handoffs: 0,
        driver_end: None,
        stop_cause: None,
        cancelled_by: None,
        finished_before_stop: false,
        wrap_up_sent: false,
        pause_by_plan: false,
        stopping: None,
        stop_plan_at: None,
        driver_gone_at: None,
    };
    for control in early {
        driven.on_control(Some(control));
    }
    let run = driver.run(io, start);
    driven.run_loop(run).await;
    driven.finish(started).await
}

/// The start the driver gets: the prompt, the status server (and the report tool when the step
/// has a schema), and the permission scope that mirrors the sandbox.
fn driver_start(ctx: &Context, launch: &Launch) -> Result<DriverStart, String> {
    let spec = &ctx.spec;
    let report_file = spec.session_dir.join(session_files::REPORT);
    let schema_file = spec.session_dir.join(session_files::SCHEMA);
    if let Some(schema) = &spec.json_schema {
        let text = serde_json::to_vec_pretty(schema).map_err(|err| err.to_string())?;
        write_private_file(&schema_file, &text)
            .map_err(|err| format!("could not write the step's schema: {err}"))?;
    }
    let report = spec.json_schema.as_ref().map(|_| ReportFiles {
        schema: &schema_file,
        report: &report_file,
    });
    let mcp_servers = if ctx.capabilities.status_tool {
        let argonauts = std::env::current_exe()
            .map_err(|err| format!("could not find the argonauts binary: {err}"))?;
        session_server(&argonauts, spec.status_file.as_deref(), report, None)
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    let forbidden = ctx
        .settings
        .agents
        .get(&spec.worker.worker)
        .map(|agent| agent.exclude_paths.clone())
        .unwrap_or_default();
    Ok(DriverStart {
        cwd: spec.workdir.clone(),
        prompt: launch.request.prompt.clone(),
        model: launch.request.model.clone(),
        effort: launch.request.effort.clone(),
        access: spec.access,
        resume: spec.resume.clone(),
        mcp_servers,
        scope: permission_scope(&launch.profile, &spec.workdir, spec.access, &forbidden),
        json_schema: spec.json_schema.clone(),
        report_file: spec.json_schema.as_ref().map(|_| report_file.clone()),
        asking: spec
            .asking
            .as_ref()
            .filter(|asking| asking.asks != argonauts_core::Asks::Never)
            .map(|asking| argonauts_core::asking::AskTarget {
                endpoint: asking.endpoint.clone(),
                token: asking.token.clone(),
            }),
    })
}

struct Driven {
    ctx: Arc<Context>,
    out: Outbox,
    obs: Observer,
    raw: RawLog,
    raw_failed: bool,
    control: mpsc::Receiver<SessionControl>,
    control_open: bool,
    process: AgentProcess,
    /// Until the process has exited and its stderr is read.
    process_open: bool,
    exited: bool,
    exit: Option<ExitStatus>,
    drain_until: Option<TokioInstant>,
    leftovers_killed: bool,
    group: Option<Group>,
    launch: LaunchRecord,
    to_driver: mpsc::Sender<DriverControl>,
    stderr_tx: mpsc::Sender<String>,
    events: mpsc::UnboundedReceiver<AgentEvent>,
    frames: mpsc::UnboundedReceiver<RawFrame>,
    deliveries: FuturesUnordered<Delivering>,
    /// What the driver was handed and hasn't answered for, by handoff number.
    handed: HashMap<u64, Pending>,
    handoffs: u64,
    driver_end: Option<DriverEnd>,
    stop_cause: Option<StopCause>,
    cancelled_by: Option<CancelledBy>,
    finished_before_stop: bool,
    wrap_up_sent: bool,
    /// The driver couldn't take a pause, so the stop plan runs once no tool call is open.
    pause_by_plan: bool,
    stopping: Option<Stopping>,
    /// When the stop plan starts, after a cancel or after the driver returned.
    stop_plan_at: Option<TokioInstant>,
    /// When to stop waiting for a driver whose agent is gone.
    driver_gone_at: Option<TokioInstant>,
}

impl Driven {
    async fn run_loop(&mut self, mut run: BoxFuture<'static, DriverEnd>) {
        let every = tick_every(&self.ctx.spec.limits);
        let mut next_tick = TokioInstant::now() + every;
        while self.driver_end.is_none() || self.process_open {
            // The plan goes on after the leader exits, for what's left of its group.
            let stop_at = self.stopping.as_ref().and_then(Stopping::next_at);
            let stop_plan_at = self.stop_plan_at;
            let drain_at = self.drain_until;
            let gone_at = self.driver_gone_at.filter(|_| self.driver_end.is_none());
            tokio::select! {
                end = &mut run, if self.driver_end.is_none() => self.driver_ended(end),
                Some(event) = self.events.recv() => {
                    let actions = self.obs.event(event);
                    self.act(actions);
                }
                Some(frame) = self.frames.recv() => self.frame(&frame),
                event = self.process.next_event(), if self.process_open => match event {
                    Some(ProcessEvent::Line(line)) => self.stderr(line),
                    Some(ProcessEvent::Exited(status)) => self.exited(status),
                    None => {
                        self.process_open = false;
                        self.driver_gone_at = Some(TokioInstant::now() + DRIVER_GRACE);
                    }
                },
                control = self.control.recv(), if self.control_open => self.on_control(control),
                Some((handoff, done)) = self.deliveries.next(), if !self.deliveries.is_empty() => {
                    self.delivered(handoff, done);
                }
                () = sleep_until(or_far(stop_at)), if stop_at.is_some() => self.stop_step().await,
                () = sleep_until(or_far(stop_plan_at)), if stop_plan_at.is_some() => {
                    self.run_stop_plan();
                }
                () = sleep_until(or_far(drain_at)), if drain_at.is_some() => {
                    self.drain_timeout().await;
                }
                () = sleep_until(or_far(gone_at)), if gone_at.is_some() => {
                    self.driver_ended(DriverEnd {
                        vendor_session: None,
                        stop: DriverStop::Disconnected,
                    });
                }
                () = sleep_until(next_tick) => {
                    next_tick = TokioInstant::now() + every;
                    let cpu = self.group.as_ref().and_then(Group::last_busy);
                    let actions = self.obs.tick(cpu);
                    self.act(actions);
                }
            }
        }
        drop(run);
        // What the driver sent just before it returned.
        while let Ok(frame) = self.frames.try_recv() {
            self.frame(&frame);
        }
        while let Ok(event) = self.events.try_recv() {
            let _ = self.obs.event(event);
        }
        // With the driver gone, every answer it still owed has come or never will.
        while let Some(Some((handoff, done))) = self.deliveries.next().now_or_never() {
            self.delivered(handoff, done);
        }
        let never_sent: Vec<Pending> = self.handed.drain().map(|(_, item)| item).collect();
        for item in &never_sent {
            self.not_sent(item, "the session ended before it could go out");
        }
    }

    async fn finish(mut self, started: Instant) -> SessionEnd {
        // What the agent left running goes before the owed diffs are worked out.
        if let Some(group) = self.group.take() {
            group.release(&self.ctx.spec.key, &self.out).await;
        }
        self.obs.finish().await;
        let exit = self.exit.as_ref().map_or((None, None), exit_parts);
        let reason = self.reason();
        end_of(
            &self.obs,
            reason,
            exit,
            started,
            Some(self.raw.stdout_path()),
        )
    }

    /// The worker's own stop wins unless the turn had already ended; a pause or stop the
    /// driver acknowledged reads from its end.
    fn reason(&self) -> EndReason {
        let end = self.driver_end.as_ref();
        match (&self.stop_cause, end) {
            (Some(cause), end) if !self.finished_before_stop => match (cause, end) {
                (StopCause::Paused | StopCause::Stopped, Some(end))
                    if end.stop != DriverStop::Disconnected =>
                {
                    from_driver(&end.stop, self.cancelled_by)
                }
                _ => cause.reason(),
            },
            (_, Some(end)) => from_driver(&end.stop, self.cancelled_by),
            (Some(cause), None) => cause.reason(),
            (None, None) => EndReason::Crashed {
                message: "the session driver never finished".to_owned(),
            },
        }
    }

    fn driver_ended(&mut self, end: DriverEnd) {
        if let Some(id) = &end.vendor_session {
            self.obs.vendor_session(id.clone());
        }
        self.driver_end = Some(end);
        if !self.exited && self.stopping.is_none() && self.stop_plan_at.is_none() {
            self.stop_plan_at = Some(TokioInstant::now() + EXIT_GRACE);
        }
    }

    fn run_stop_plan(&mut self) {
        self.stop_plan_at = None;
        if self.exited || self.stopping.is_some() {
            return;
        }
        let cause = self.stop_cause.clone().unwrap_or(StopCause::Stopped);
        self.stopping = Some(Stopping::begin(
            Purpose::End(cause),
            self.ctx.adapter.stop_plan(),
        ));
    }

    /// The stop plan's next signal, once the tree it reaches is noted.
    async fn stop_step(&mut self) {
        if let Some(group) = self.group.as_mut()
            && let Err(err) = group.note_tree().await
        {
            self.out.notice(err);
        }
        if let Some(stopping) = self.stopping.as_mut()
            && let Err(err) = stopping.step(&self.process)
        {
            self.out
                .notice(format!("could not signal the agent's process group: {err}"));
        }
    }

    fn frame(&mut self, frame: &RawFrame) {
        let logged = self.raw.frame(frame);
        self.logged(logged);
        if !frame.outgoing {
            self.obs.output();
        }
    }

    fn stderr(&mut self, line: OutputLine) {
        let logged = self.raw.stderr_line(&line.text);
        self.logged(logged);
        self.obs.output();
        self.obs.stderr(&line.text);
        // The driver may not read stderr; the line is logged either way.
        let _ = self.stderr_tx.try_send(line.text);
    }

    fn logged(&mut self, result: io::Result<()>) {
        if let Err(err) = result
            && !self.raw_failed
        {
            self.raw_failed = true;
            self.out
                .notice(format!("could not write the session's raw log: {err}"));
        }
    }

    fn exited(&mut self, status: io::Result<ExitStatus>) {
        self.exited = true;
        match status {
            Ok(status) => self.exit = Some(status),
            Err(err) => self
                .out
                .notice(format!("lost track of the agent's exit: {err}")),
        }
        self.stop_plan_at = None;
        self.drain_until = Some(TokioInstant::now() + EXIT_GRACE);
    }

    async fn drain_timeout(&mut self) {
        if self.leftovers_killed {
            self.process.stop_reading();
            self.drain_until = None;
        } else {
            if let Some(group) = self.group.as_mut()
                && let Err(err) = group.note_tree().await
            {
                self.out.notice(err);
            }
            if let Err(err) = self.process.signal(StopSignal::Kill) {
                self.out.notice(format!(
                    "could not kill processes the agent left behind: {err}"
                ));
            }
            self.leftovers_killed = true;
            self.drain_until = Some(TokioInstant::now() + AFTER_KILL_GRACE);
        }
    }

    fn on_control(&mut self, control: Option<SessionControl>) {
        match control {
            Some(SessionControl::Deliver(message)) => {
                let now = message.delivery == Delivery::Steer;
                if !now {
                    self.out.session(SessionEvent::Queued(message.clone()));
                }
                let via = if now {
                    Delivery::Steer
                } else {
                    Delivery::NextTurn
                };
                self.send(Pending::message(&message, via), now);
            }
            Some(SessionControl::WrapUp) => self.wrap_up(None),
            Some(SessionControl::StopAfterTool) => {
                if self.stop_cause.is_none() {
                    self.set_cause(StopCause::Paused);
                    self.cancelled_by = Some(CancelledBy::Pause);
                    let told =
                        self.driver_end.is_none() && self.tell_driver(DriverControl::StopAfterTool);
                    if !told {
                        self.pause_by_plan = true;
                        self.pause_when_idle();
                    }
                }
            }
            // A driven CLI reports nothing it runs in the background, so there is nothing a
            // turn's stop could spare.
            Some(SessionControl::Stop | SessionControl::StopTurn) => {
                self.cancel(StopCause::Stopped);
            }
            // Only a CLI that says it can take these gets them, and none driven here does.
            Some(SessionControl::Send { uuid, .. }) => self.out.agent(AgentEvent::MessageState {
                uuid,
                state: MessageStage::NotSent,
            }),
            Some(
                SessionControl::SendNow { .. }
                | SessionControl::TakeBack { .. }
                | SessionControl::Screen(_)
                | SessionControl::AskContext(_)
                | SessionControl::WatchContext(_)
                | SessionControl::StopSubagent { .. },
            ) => {}
            None => {
                self.control_open = false;
                self.cancel(StopCause::Stopped);
            }
        }
    }

    fn set_cause(&mut self, cause: StopCause) {
        if self.stop_cause.is_none() {
            self.finished_before_stop = self.driver_end.is_some();
            self.stop_cause = Some(cause);
        }
    }

    /// `Cancel`, which answers pending permission requests `cancelled`, then a short wait
    /// before the stop plan.
    fn cancel(&mut self, cause: StopCause) {
        self.set_cause(cause);
        self.cancelled_by = Some(CancelledBy::Stop);
        if self.driver_end.is_none() {
            self.tell_driver(DriverControl::Cancel);
        }
        if self.stopping.is_none() && self.stop_plan_at.is_none() && !self.exited {
            self.stop_plan_at = Some(TokioInstant::now() + EXIT_GRACE);
        }
    }

    /// The stop plan in place of a pause the driver couldn't take, once no tool call is open.
    fn pause_when_idle(&mut self) {
        let idle = self.obs.facts.open_tools.is_empty();
        if self.pause_by_plan && idle && !self.exited && self.stopping.is_none() {
            self.pause_by_plan = false;
            self.stop_plan_at = Some(TokioInstant::now());
        }
    }

    /// Whether the driver has `control` now.
    fn tell_driver(&self, control: DriverControl) -> bool {
        match self.to_driver.try_send(control) {
            Ok(()) => true,
            Err(err) => {
                self.out
                    .notice(format!("could not reach the session driver: {err}"));
                false
            }
        }
    }

    fn send(&mut self, item: Pending, now: bool) {
        if self.driver_end.is_some() {
            self.not_sent(&item, "the session driver had finished");
            return;
        }
        let (done, delivered) = oneshot::channel();
        let text = item.part.text.clone();
        match self
            .to_driver
            .try_send(DriverControl::Deliver { text, now, done })
        {
            Ok(()) => {
                self.handoffs += 1;
                let handoff = self.handoffs;
                self.handed.insert(handoff, item);
                self.deliveries
                    .push(Box::pin(async move { (handoff, delivered.await) }));
            }
            Err(err) => {
                let why = format!("could not hand it to the session driver: {err}");
                self.not_sent(&item, &why);
            }
        }
    }

    fn delivered(&mut self, handoff: u64, done: Result<Delivery, RecvError>) {
        let Some(mut item) = self.handed.remove(&handoff) else {
            return;
        };
        // A dropped `done` means it never went out.
        let Ok(via) = done else {
            self.not_sent(&item, "the session driver dropped it");
            return;
        };
        if let Some((_, delivery)) = item.message.as_mut() {
            *delivery = via;
        }
        let parts = vec![item.part.clone()];
        announce(&self.out, &[item], parts, self.launch.clone(), Vec::new());
    }

    fn not_sent(&self, item: &Pending, why: &str) {
        match &item.message {
            Some((message, _)) => self.out.undelivered(&message.id, why),
            None => self
                .out
                .notice(format!("the {} didn't go out: {why}", item.part.label)),
        }
    }

    fn wrap_up(&mut self, dimension: Option<BudgetDimension>) {
        if self.wrap_up_sent {
            return;
        }
        self.wrap_up_sent = true;
        let via = match self.ctx.capabilities.steer {
            SteerSupport::Input => WrapUpVia::Input,
            SteerSupport::InterruptResume | SteerSupport::NextTurnOnly => {
                WrapUpVia::InterruptResume
            }
        };
        self.send(Pending::wrap_up(dimension, via), true);
    }

    fn act(&mut self, actions: Vec<Action>) {
        self.pause_when_idle();
        let idle = self.driver_end.is_some();
        for action in actions {
            match action {
                Action::WrapUp(_) | Action::Nudge { .. } if idle => {}
                Action::Stop(StopCause::Limit(_) | StopCause::Rotate { .. }) if idle => {}
                Action::WrapUp(dimension) => self.wrap_up(dimension),
                Action::Nudge { call, times } => self.send(Pending::note(call, times), true),
                Action::Stop(cause) => self.cancel(cause),
                Action::AskContext(_) => {}
            }
        }
    }
}
