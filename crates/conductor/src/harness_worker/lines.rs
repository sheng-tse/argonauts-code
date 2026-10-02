//! A session read line by line through the adapter's parser. Messages go in through the open
//! input where the adapter makes input lines; otherwise the worker waits for the running tool
//! call, interrupts, and resumes the same vendor session with the message and every flag again.
//! Open input closes once every turn has its result, so the harness exits; a message that comes
//! after that resumes the vendor session once the harness is done. A message that never goes
//! out is reported. A message sent mid-turn is a turn of its own unless the adapter says it
//! joins the running one.

use std::io;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Instant;

use argonauts_core::events::session::{
    Delivery, EndReason, LaunchRecord, PromptVia, QueuedMessage, SessionEnd, SessionEvent,
    WrapUpVia,
};
use argonauts_core::worker::{SessionControl, SteerSupport};
use argonauts_core::{BudgetDimension, StopSignal, StreamParser};
use tokio::sync::mpsc;
use tokio::time::{Instant as TokioInstant, sleep_until};

use super::context::{Context, Launch};
use super::end::{EndFacts, classify};
use super::group::Group;
use super::launch::{input_event, prompt_text};
use super::observe::{Action, Observer, oversized_line};
use super::outbox::Outbox;
use super::session::{Running, announce_start, end_of, not_started, or_far, tick_every};
use super::steer::{Pending, announce, joined_parts};
use super::stop::{Purpose, StopCause, Stopping};
use crate::process::{AgentProcess, OutputLine, ProcessEvent, Stream, exit_parts};
use crate::rawlog::RawLog;

/// How long output may keep coming after the agent exits, from something it started.
const EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
const AFTER_KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

pub async fn run(running: Running, launch: Launch) -> SessionEnd {
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
    let process = match AgentProcess::spawn_logged(&launch.command, raw.output_copies()) {
        Ok(process) => process,
        Err(err) => {
            let reason = EndReason::StartFailed {
                message: err.to_string(),
            };
            return not_started(reason, started, raw_path);
        }
    };
    out.session(input_event(
        launch.record.clone(),
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
    let live = Live::new(&ctx, process, group, &launch);
    let mut session = Lines {
        steer: ctx.capabilities.steer,
        ctx,
        out,
        obs,
        raw,
        control,
        control_open: true,
        live,
        stopping: None,
        stop_cause: None,
        finished_before_stop: false,
        stop_after_tool: false,
        wrap_up_sent: false,
        wrap_up_refused: false,
        next_turn: Vec::new(),
        steer_after_tool: Vec::new(),
        resume_with: Vec::new(),
        failure: None,
        exit: (None, None),
    };
    for control in early {
        session.on_control(Some(control));
    }
    session.run_loop().await;
    session.finish(started).await
}

/// One process of the session. A resume replaces it.
struct Live {
    process: AgentProcess,
    parser: Box<dyn StreamParser>,
    group: Option<Group>,
    launch: LaunchRecord,
    input_open: bool,
    /// The prompt and every input line since that started a turn of its own.
    turns_sent: u32,
    exited: bool,
    exit: Option<ExitStatus>,
    drain_until: Option<TokioInstant>,
    leftovers_killed: bool,
}

impl Live {
    fn new(ctx: &Context, process: AgentProcess, group: Group, launch: &Launch) -> Self {
        Self {
            process,
            parser: ctx.adapter.parser(),
            group: Some(group),
            launch: launch.record.clone(),
            input_open: launch.command.keep_stdin_open,
            turns_sent: 1,
            exited: false,
            exit: None,
            drain_until: None,
            leftovers_killed: false,
        }
    }
}

struct Lines {
    ctx: Arc<Context>,
    out: Outbox,
    obs: Observer,
    /// The running process's readers copy its output here.
    raw: RawLog,
    control: mpsc::Receiver<SessionControl>,
    control_open: bool,
    steer: SteerSupport,
    live: Live,
    stopping: Option<Stopping>,
    stop_cause: Option<StopCause>,
    finished_before_stop: bool,
    stop_after_tool: bool,
    wrap_up_sent: bool,
    /// The wrap-up couldn't go out, and that was reported.
    wrap_up_refused: bool,
    /// Messages for the end of the current turn.
    next_turn: Vec<QueuedMessage>,
    /// Steering that waits for the running tool call to finish.
    steer_after_tool: Vec<QueuedMessage>,
    /// What waits for an interrupt and a resume.
    resume_with: Vec<Pending>,
    /// A resume that couldn't start.
    failure: Option<EndReason>,
    exit: (Option<i32>, Option<i32>),
}

impl Lines {
    async fn run_loop(&mut self) {
        let every = tick_every(&self.ctx.spec.limits);
        let mut next_tick = TokioInstant::now() + every;
        loop {
            // The plan goes on after the leader exits, for what's left of its group.
            let stop_at = self.stopping.as_ref().and_then(Stopping::next_at);
            let drain_at = self.live.drain_until;
            tokio::select! {
                event = self.live.process.next_event() => match event {
                    Some(ProcessEvent::Line(line)) => self.line(line),
                    Some(ProcessEvent::Exited(status)) => self.exited(status),
                    None => {
                        if !self.process_ended().await {
                            break;
                        }
                    }
                },
                control = self.control.recv(), if self.control_open => self.on_control(control),
                () = sleep_until(or_far(stop_at)), if stop_at.is_some() => self.stop_step().await,
                () = sleep_until(or_far(drain_at)), if drain_at.is_some() => {
                    self.drain_timeout().await;
                }
                () = sleep_until(next_tick) => {
                    next_tick = TokioInstant::now() + every;
                    let cpu = self.live.group.as_ref().and_then(Group::last_busy);
                    let actions = self.obs.tick(cpu);
                    self.after_events(actions);
                }
            }
        }
    }

    async fn finish(mut self, started: Instant) -> SessionEnd {
        // What the agent left running goes before the owed diffs are worked out.
        self.release_group().await;
        let waiting = std::mem::take(&mut self.resume_with)
            .into_iter()
            .filter_map(|item| item.message.map(|(message, _)| message))
            .chain(std::mem::take(&mut self.steer_after_tool))
            .chain(std::mem::take(&mut self.next_turn));
        for message in waiting {
            self.out
                .undelivered(&message.id, "the session ended before it could go out");
        }
        self.obs.finish().await;
        let facts = EndFacts {
            stopped_by: self.stop_cause.clone(),
            finished_before_stop: self.finished_before_stop,
            exit_code: self.exit.0,
            signal: self.exit.1,
            ..self.obs.facts.end_facts()
        };
        let reason = self.failure.take().unwrap_or_else(|| classify(&facts));
        end_of(
            &self.obs,
            reason,
            self.exit,
            started,
            Some(self.raw.stdout_path()),
        )
    }

    async fn release_group(&mut self) {
        if let Some(group) = self.live.group.take() {
            group.release(&self.ctx.spec.key, &self.out).await;
        }
    }

    /// The stop plan's next signal, once the tree it reaches is noted.
    async fn stop_step(&mut self) {
        if let Some(group) = self.live.group.as_mut()
            && let Err(err) = group.note_tree().await
        {
            self.out.notice(err);
        }
        if let Some(stopping) = self.stopping.as_mut()
            && let Err(err) = stopping.step(&self.live.process)
        {
            self.out
                .notice(format!("could not signal the agent's process group: {err}"));
        }
    }

    fn line(&mut self, line: OutputLine) {
        self.obs.output();
        let events = match line.stream {
            Stream::Stdout => {
                if line.dropped > 0 {
                    vec![oversized_line(&line.text, line.dropped)]
                } else {
                    self.live.parser.parse_stdout_line(&line.text)
                }
            }
            Stream::Stderr => {
                self.obs.stderr(&line.text);
                self.live.parser.parse_stderr_line(&line.text)
            }
        };
        let mut actions = Vec::new();
        for event in events {
            actions.extend(self.obs.event(event));
        }
        self.after_events(actions);
    }

    fn exited(&mut self, status: io::Result<ExitStatus>) {
        self.live.exited = true;
        match status {
            Ok(status) => self.live.exit = Some(status),
            Err(err) => self
                .out
                .notice(format!("lost track of the agent's exit: {err}")),
        }
        self.live.drain_until = Some(TokioInstant::now() + EXIT_GRACE);
    }

    /// Something the agent started still holds its output open: kill the group, then stop
    /// reading.
    async fn drain_timeout(&mut self) {
        if self.live.leftovers_killed {
            self.live.process.stop_reading();
            self.live.drain_until = None;
        } else {
            if let Some(group) = self.live.group.as_mut()
                && let Err(err) = group.note_tree().await
            {
                self.out.notice(err);
            }
            if let Err(err) = self.live.process.signal(StopSignal::Kill) {
                self.out.notice(format!(
                    "could not kill processes the agent left behind: {err}"
                ));
            }
            self.live.leftovers_killed = true;
            self.live.drain_until = Some(TokioInstant::now() + AFTER_KILL_GRACE);
        }
    }

    /// The process is gone and its output read. Returns whether the session goes on in a
    /// resumed process.
    async fn process_ended(&mut self) -> bool {
        self.exit = self.live.exit.as_ref().map_or((None, None), exit_parts);
        for event in self.live.parser.finish(self.exit.0) {
            // The process is gone, so limits have nothing left to act on.
            let _ = self.obs.event(event);
        }
        // No tool call will finish now, so held steering waits for the next turn.
        for message in std::mem::take(&mut self.steer_after_tool) {
            self.queue_next_turn(message);
        }
        let items = self.resume_items();
        if items.is_empty() {
            return false;
        }
        self.resume(items).await
    }

    fn resume_items(&mut self) -> Vec<Pending> {
        let can_resume = self.ctx.capabilities.resume
            && self.stop_cause.is_none()
            && self.obs.facts.vendor_session.is_some();
        let interrupted = self
            .stopping
            .as_ref()
            .is_some_and(|stopping| *stopping.purpose() == Purpose::Relaunch);
        let clean = self.obs.facts.result_is_error == Some(false);
        // Open input takes every message while the harness runs, except one that came after
        // the input closed at the last result.
        let waiting = self.steer != SteerSupport::Input || !self.next_turn.is_empty();
        if !can_resume || !(interrupted || clean) || !waiting {
            return Vec::new();
        }
        let mut items = std::mem::take(&mut self.resume_with);
        if !interrupted {
            // The turn finished on its own, so a wrap-up or a note has nothing left to change.
            items.retain(|item| item.message.is_some());
        }
        items.extend(
            self.next_turn
                .drain(..)
                .map(|message| Pending::message(&message, Delivery::NextTurn)),
        );
        items
    }

    /// Resumes the same vendor session in a new process with `items` as the prompt.
    async fn resume(&mut self, items: Vec<Pending>) -> bool {
        self.stopping = None;
        self.release_group().await;
        let Some(vendor_session) = self.obs.facts.vendor_session.clone() else {
            self.undelivered(
                &items,
                "the vendor session is unknown, so it can't be resumed",
            );
            return false;
        };
        let parts = joined_parts(&items);
        let mut spec = self.ctx.spec.clone();
        spec.resume = Some(vendor_session);
        spec.prompt = parts.clone();
        let could_not = |why: String| EndReason::Crashed {
            message: format!("could not resume the session to deliver a message: {why}"),
        };
        let launch = match self.ctx.launch(&spec) {
            Ok(launch) => launch,
            Err(why) => {
                self.undelivered(&items, "the session couldn't be resumed");
                self.failure = Some(could_not(why));
                return false;
            }
        };
        let process = match AgentProcess::spawn_logged(&launch.command, self.raw.output_copies()) {
            Ok(process) => process,
            Err(err) => {
                self.undelivered(&items, "the session couldn't be resumed");
                self.failure = Some(could_not(err.to_string()));
                return false;
            }
        };
        announce(
            &self.out,
            &items,
            parts,
            launch.record.clone(),
            self.queued(),
        );
        let group = match Group::register(&self.ctx, &process, &self.out).await {
            Ok(group) => group,
            Err(message) => {
                drop(process);
                self.failure = Some(EndReason::StartFailed { message });
                return false;
            }
        };
        self.live = Live::new(&self.ctx, process, group, &launch);
        self.obs.resumed();
        true
    }

    fn undelivered(&self, items: &[Pending], why: &str) {
        for (message, _) in items.iter().filter_map(|item| item.message.as_ref()) {
            self.out.undelivered(&message.id, why);
        }
    }

    fn on_control(&mut self, control: Option<SessionControl>) {
        match control {
            Some(SessionControl::Deliver(message)) => self.deliver(message),
            Some(SessionControl::WrapUp) => self.wrap_up(None),
            Some(SessionControl::StopAfterTool) => {
                self.stop_after_tool = true;
                self.stop_after_tool_if_idle();
            }
            Some(SessionControl::Stop) => self.begin_stop(StopCause::Stopped),
            None => {
                // Every sender is gone: nobody can stop it later, so stop it now.
                self.control_open = false;
                self.begin_stop(StopCause::Stopped);
            }
        }
    }

    fn deliver(&mut self, message: QueuedMessage) {
        let resumable = self.ctx.capabilities.resume;
        match (self.steer, message.delivery) {
            // The agent reads its input only between tool calls, so steering waits for the
            // running one, which the journal then shows.
            (SteerSupport::Input, Delivery::Steer) if !self.obs.facts.open_tools.is_empty() => {
                self.steer_after_tool.push(message);
            }
            (SteerSupport::Input, Delivery::Steer) => self.steer_now(vec![message]),
            (SteerSupport::InterruptResume, Delivery::Steer) if resumable => {
                self.out.session(SessionEvent::Queued(message.clone()));
                self.resume_with
                    .push(Pending::message(&message, Delivery::Steer));
                self.try_interrupt();
            }
            _ => {
                self.queue_next_turn(message);
                if self.steer == SteerSupport::Input && self.turn_done() {
                    self.flush_next_turn();
                }
            }
        }
    }

    /// Writes steering into the open input, or keeps it for the next turn when that's closed.
    fn steer_now(&mut self, messages: Vec<QueuedMessage>) {
        let items = messages
            .iter()
            .map(|message| Pending::message(message, Delivery::Steer))
            .collect();
        if !self.write_input(items) {
            for message in messages {
                self.queue_next_turn(message);
            }
        }
    }

    fn queue_next_turn(&mut self, message: QueuedMessage) {
        self.out.session(SessionEvent::Queued(message.clone()));
        self.next_turn.push(message);
    }

    /// Messages still waiting, for each `Input`.
    fn queued(&self) -> Vec<QueuedMessage> {
        self.resume_with
            .iter()
            .filter_map(|item| item.message.as_ref().map(|(message, _)| message.clone()))
            .chain(self.steer_after_tool.iter().cloned())
            .chain(self.next_turn.iter().cloned())
            .collect()
    }

    /// Every turn sent so far has its result.
    fn turn_done(&self) -> bool {
        self.obs.facts.results >= self.live.turns_sent
    }

    /// Sends `items` through the open input: a turn of their own, or a part of the running turn
    /// where the adapter says mid-turn input joins it.
    fn write_input(&mut self, items: Vec<Pending>) -> bool {
        if !self.live.input_open || self.stop_cause.is_some() {
            return false;
        }
        let parts = joined_parts(&items);
        let Some(mut line) = self.ctx.adapter.input_line(&prompt_text(&parts)) else {
            return false;
        };
        if !line.ends_with('\n') {
            line.push('\n');
        }
        let new_turn =
            self.turn_done() || !self.ctx.adapter.capabilities().input_joins_running_turn;
        if !self.live.process.send_input(line) {
            self.live.input_open = false;
            return false;
        }
        if new_turn {
            self.live.turns_sent += 1;
        }
        let launch = LaunchRecord {
            prompt_via: PromptVia::StreamInput,
            ..self.live.launch.clone()
        };
        announce(&self.out, &items, parts, launch, self.queued());
        true
    }

    fn flush_next_turn(&mut self) {
        let waiting = std::mem::take(&mut self.next_turn);
        let items = waiting
            .iter()
            .map(|message| Pending::message(message, Delivery::NextTurn))
            .collect();
        if !self.write_input(items) {
            self.next_turn = waiting;
        }
    }

    fn wrap_up(&mut self, dimension: Option<BudgetDimension>) {
        if self.wrap_up_sent {
            return;
        }
        match self.steer {
            SteerSupport::Input => {
                if self.write_input(vec![Pending::wrap_up(dimension, WrapUpVia::Input)]) {
                    self.wrap_up_sent = true;
                } else if !self.wrap_up_refused {
                    // Not marked sent, so a later wrap-up can still go out.
                    self.wrap_up_refused = true;
                    self.out
                        .notice("the agent's input is closed, so it didn't get the wrap-up");
                }
            }
            SteerSupport::InterruptResume if self.ctx.capabilities.resume => {
                self.wrap_up_sent = true;
                self.resume_with
                    .push(Pending::wrap_up(dimension, WrapUpVia::InterruptResume));
                self.try_interrupt();
            }
            _ => {
                self.wrap_up_sent = true;
                self.out.notice(format!(
                    "{} can take no message mid-session, so it gets no wrap-up",
                    self.ctx.adapter.display_name()
                ));
            }
        }
    }

    fn nudge(&mut self, call: String, times: u32) {
        let note = Pending::note(call, times);
        let taken = match self.steer {
            SteerSupport::Input => self.write_input(vec![note]),
            SteerSupport::InterruptResume if self.ctx.capabilities.resume => {
                self.resume_with.push(note);
                self.try_interrupt();
                true
            }
            // The stuck watchdog stops it instead.
            _ => true,
        };
        if !taken {
            self.out
                .notice("the agent's input is closed, so it didn't get the repeat note");
        }
    }

    /// Interrupts for a resume once no tool call is running and the vendor session is known.
    fn try_interrupt(&mut self) {
        let ready = !self.resume_with.is_empty()
            && self.stopping.is_none()
            && self.stop_cause.is_none()
            && !self.live.exited
            && self.obs.facts.open_tools.is_empty()
            && self.obs.facts.vendor_session.is_some();
        if ready {
            self.stopping = Some(Stopping::begin(
                Purpose::Relaunch,
                self.ctx.adapter.stop_plan(),
            ));
        }
    }

    fn stop_after_tool_if_idle(&mut self) {
        if self.stop_after_tool && self.obs.facts.open_tools.is_empty() {
            self.begin_stop(StopCause::Paused);
        }
    }

    /// Runs the stop plan for `cause`. The first cause stands.
    fn begin_stop(&mut self, cause: StopCause) {
        if self.stop_cause.is_some() {
            return;
        }
        self.finished_before_stop = self.turn_done();
        self.stop_cause = Some(cause.clone());
        if self.live.input_open {
            self.live.process.close_input();
            self.live.input_open = false;
        }
        match self.stopping.as_mut() {
            Some(stopping) => stopping.end_instead(cause),
            None if !self.live.exited => {
                self.stopping = Some(Stopping::begin(
                    Purpose::End(cause),
                    self.ctx.adapter.stop_plan(),
                ));
            }
            None => {}
        }
    }

    fn after_events(&mut self, actions: Vec<Action>) {
        let idle = self.turn_done() && self.next_turn.is_empty() && self.resume_with.is_empty();
        for action in actions {
            match action {
                // The work is done and the harness is ending on its own.
                Action::WrapUp(_) | Action::Nudge { .. } if idle => {}
                Action::Stop(StopCause::Limit(_) | StopCause::Rotate { .. }) if idle => {}
                Action::WrapUp(dimension) => self.wrap_up(dimension),
                Action::Nudge { call, times } => self.nudge(call, times),
                Action::Stop(cause) => self.begin_stop(cause),
            }
        }
        self.stop_after_tool_if_idle();
        self.try_interrupt();
        if !self.steer_after_tool.is_empty() && self.obs.facts.open_tools.is_empty() {
            let held = std::mem::take(&mut self.steer_after_tool);
            self.steer_now(held);
        }
        let input_turn_over =
            self.steer == SteerSupport::Input && self.turn_done() && self.stop_cause.is_none();
        if input_turn_over {
            if !self.next_turn.is_empty() {
                self.flush_next_turn();
            } else if self.live.input_open {
                self.live.process.close_input();
                self.live.input_open = false;
            }
        }
    }
}
