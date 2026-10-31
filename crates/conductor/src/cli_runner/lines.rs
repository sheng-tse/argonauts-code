//! A session read line by line through the adapter's parser. Messages go in through the open
//! input where the adapter makes input lines; otherwise the runner waits for the running tool
//! call, interrupts, and resumes the same CLI session with the message and every flag again.
//! Open input closes once every turn has its result and, from a CLI that can count, the count
//! asked as it ended, so the CLI exits; a message that comes after that resumes the CLI session
//! once the CLI is done. A message that never goes out is reported. A message sent mid-turn is
//! a turn of its own unless the adapter says it joins the running one.

use std::collections::BTreeSet;
use std::io;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Instant;

use argonauts_core::agents::session_runner::{SandboxProfile, SessionControl, SteerSupport};
use argonauts_core::look::pictures::Picture;
use argonauts_core::runs::events::context::ContextDetail;
use argonauts_core::runs::events::session::{
    Delivery, EndReason, LaunchRecord, PromptVia, QueuedMessage, SessionEnd, SessionEvent,
    WrapUpVia,
};
use argonauts_core::{
    AgentEvent, CommandSpec, ErrorCategory, LimitDimension, MessageStage, StopSignal, StreamParser,
};
use tokio::sync::mpsc;
use tokio::time::{Instant as TokioInstant, sleep_until};

use super::after_end;
use super::context::{Context, Launch};
use super::end::{EndFacts, classify};
use super::group::Group;
use super::launch::{input_event, keep_launch, prompt_text};
use super::observe::{Action, Observer, oversized_line};
use super::outbox::Outbox;
use super::report_tool;
use super::session::{Running, announce_start, end_of, not_started, or_far, tick_every};
use super::steer::{Pending, announce, joined_parts};
use super::stop::{Purpose, StopCause, Stopping};
use crate::process::{AgentProcess, OutputLine, ProcessEvent, Stream, exit_parts};
use crate::session::rawlog::RawLog;

/// How long output may keep coming after the agent exits, from something it started.
const EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
const AFTER_KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(500);
/// How long a finished turn's input stays open for what the CLI runs in the background. Once the
/// input closes, the CLI waits for that work only a while (Claude Code ten minutes) and then
/// stops it.
const BACKGROUND_WAIT: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// How long a finished turn's input stays open for the count it is asked for as it ends.
/// Claude Code answers a short count from what it already knows, in milliseconds.
const COUNT_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

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
    keep_launch(&ctx.spec.session_dir, &launch.record);
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
        started,
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
        turn_stopped: false,
        next_turn: Vec::new(),
        steer_after_tool: Vec::new(),
        joining: Vec::new(),
        resume_with: Vec::new(),
        failure: None,
        exit: (None, None),
        held_reports: report_tool::Held::default(),
        report_asked: false,
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
    /// The adapter's command and the sandbox it ran in, for what the parser asks to run once
    /// the process has ended.
    unwrapped: CommandSpec,
    profile: SandboxProfile,
    input_open: bool,
    /// The prompt and every input line since that started a turn of its own.
    turns_sent: u32,
    exited: bool,
    exit: Option<ExitStatus>,
    drain_until: Option<TokioInstant>,
    leftovers_killed: bool,
    /// Tasks the CLI reports running in the background, which it stops a while after the input
    /// closes.
    background: usize,
    /// When a finished turn stops waiting on them.
    background_until: Option<TokioInstant>,
    /// The CLI's own workflows that haven't ended, by id.
    workflows: BTreeSet<String>,
    /// The turn whose end its count was asked at, by `turns_sent` then.
    counted_turn: Option<u32>,
    /// While the input stays open for that count: when it closes without it.
    count_until: Option<TokioInstant>,
}

impl Live {
    fn new(ctx: &Context, process: AgentProcess, group: Group, launch: &Launch) -> Self {
        Self {
            process,
            parser: ctx.adapter.session_parser(&launch.request),
            group: Some(group),
            launch: launch.record.clone(),
            unwrapped: launch.unwrapped.clone(),
            profile: launch.profile.clone(),
            input_open: launch.command.keep_stdin_open,
            turns_sent: 1,
            exited: false,
            exit: None,
            drain_until: None,
            leftovers_killed: false,
            background: 0,
            background_until: None,
            workflows: BTreeSet::new(),
            counted_turn: None,
            count_until: None,
        }
    }
}

struct Lines {
    started: Instant,
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
    /// You stopped a turn alone, so a session whose last word is that interrupted turn was
    /// stopped by you and didn't fail.
    turn_stopped: bool,
    /// Messages for the end of the current turn.
    next_turn: Vec<QueuedMessage>,
    /// Steering that waits for the running tool call to finish.
    steer_after_tool: Vec<QueuedMessage>,
    /// Your messages written mid-turn that the model hasn't read yet, by uuid. One still
    /// waiting at the turn's end starts the next turn in the same process.
    joining: Vec<String>,
    /// What waits for an interrupt and a resume.
    resume_with: Vec<Pending>,
    /// A resume that couldn't start.
    failure: Option<EndReason>,
    exit: (Option<i32>, Option<i32>),
    /// Reports sent through the tool while its own workflow still ran.
    held_reports: report_tool::Held,
    /// The session was asked once for the report it ended without.
    report_asked: bool,
}

impl Lines {
    async fn run_loop(&mut self) {
        let every = tick_every(&self.ctx.spec.limits);
        let mut next_tick = TokioInstant::now() + every;
        loop {
            // The plan goes on after the head exits, for what's left of its group.
            let stop_at = self.stopping.as_ref().and_then(Stopping::next_at);
            let drain_at = self.live.drain_until;
            let background_at = self.live.background_until;
            let count_at = self.live.count_until;
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
                () = sleep_until(or_far(background_at)), if background_at.is_some() => {
                    self.background_wait_over();
                }
                () = sleep_until(or_far(count_at)), if count_at.is_some() => self.count_wait_over(),
                () = sleep_until(next_tick) => {
                    next_tick = TokioInstant::now() + every;
                    // A quiet wait on the background isn't the CLI going quiet mid-turn.
                    if self.live.background_until.is_some() {
                        self.obs.output();
                    }
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
        let reason = match reason {
            EndReason::CliError {
                category: ErrorCategory::Interrupted,
                ..
            } if self.turn_stopped => EndReason::Stopped,
            reason => reason,
        };
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
                    self.obs.stdout_line(&line.text);
                    let events = self.live.parser.parse_stdout_line(&line.text);
                    self.obs
                        .billed_since_totals(self.live.parser.billed_since_totals());
                    events
                }
            }
            Stream::Stderr => {
                self.obs.stderr(&line.text);
                self.live.parser.parse_stderr_line(&line.text)
            }
        };
        let mut actions = Vec::new();
        for event in events {
            if let AgentEvent::MessageState { uuid, state } = &event
                && matches!(state, MessageStage::Started | MessageStage::TakenBack)
            {
                self.joining.retain(|waiting| waiting != uuid);
            }
            if matches!(event, AgentEvent::ContextBreakdown { .. }) {
                self.live.count_until = None;
            }
            if let AgentEvent::BackgroundTasks { running, ended } = &event {
                self.background_changed(running.len(), *ended);
            }
            if let AgentEvent::Workflow { workflow } = &event {
                if workflow.ended() {
                    self.live.workflows.remove(&workflow.id);
                } else {
                    self.live.workflows.insert(workflow.id.clone());
                }
            }
            if self.holds_report(&event) {
                continue;
            }
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
        if let Some(command) = self.live.parser.after_end(&self.live.unwrapped) {
            let printed = after_end::run(&self.ctx, command, &self.live.profile).await;
            for event in self.live.parser.read_after_end(printed.as_deref()) {
                let _ = self.obs.event(event);
            }
        }
        for event in self.live.parser.finish(self.exit.0) {
            // The process is gone, so limits have nothing left to act on.
            let _ = self.obs.event(event);
        }
        self.obs.spent_at_least(self.live.parser.spent_at_least());
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
            && self.obs.facts.cli_session.is_some();
        let interrupted = self
            .stopping
            .as_ref()
            .is_some_and(|stopping| *stopping.purpose() == Purpose::Relaunch);
        let clean = self.obs.facts.result_is_error == Some(false);
        // Open input takes every message while the CLI runs, except one that came after
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

    /// Resumes the same CLI session in a new process with `items` as the prompt.
    async fn resume(&mut self, items: Vec<Pending>) -> bool {
        self.stopping = None;
        self.release_group().await;
        let Some(cli_session) = self.obs.facts.cli_session.clone() else {
            self.undelivered(
                &items,
                "the agent session is unknown, so it can't be resumed",
            );
            return false;
        };
        let parts = joined_parts(&items);
        let mut spec = self.ctx.spec.clone();
        // A session that began as a copy goes on as itself.
        spec.resume = Some(cli_session);
        spec.fork = false;
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
        keep_launch(&self.ctx.spec.session_dir, &launch.record);
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
            Some(SessionControl::Send {
                text,
                uuid,
                pictures,
            }) => self.send_message(&text, &pictures, uuid),
            // From the agent view, which names no message: what waits for the tool call.
            Some(SessionControl::SendNow { uuid }) if uuid.is_empty() => self.steer_held_now(),
            Some(SessionControl::SendNow { uuid }) => {
                let line = self.ctx.adapter.send_now_line(&uuid);
                self.write_control(line);
            }
            Some(SessionControl::TakeBack { uuid }) => {
                let line = self.ctx.adapter.cancel_line(&uuid);
                if !self.write_control(line) {
                    // Nothing can read it now, so it's as good as taken back.
                    self.joining.retain(|waiting| *waiting != uuid);
                    self.out.agent(AgentEvent::MessageState {
                        uuid,
                        state: MessageStage::TakenBack,
                    });
                }
            }
            Some(SessionControl::Screen(request)) => {
                let line = self.ctx.adapter.screen_line(&request);
                self.write_control(line);
            }
            Some(SessionControl::AskContext(detail)) => {
                self.obs.asked_context();
                self.ask_context(detail);
            }
            Some(SessionControl::WatchContext(detail)) => self.obs.watch_context(detail),
            Some(SessionControl::StopTurn) => self.stop_turn(),
            Some(SessionControl::Asking) => self.obs.asking(true),
            Some(SessionControl::Answered) => self.obs.asking(false),
            Some(SessionControl::StopSubagent { id }) => {
                let line = self.live.parser.stop_subagent_line(&id);
                if !self.write_control(line) {
                    self.out.notice(format!(
                        "{} can't stop that subagent on its own now. Stopping the agent stops \
                         everything it runs.",
                        self.ctx.adapter.display_name()
                    ));
                }
            }
            None => {
                // Every sender is gone: nobody can stop it later, so stop it now.
                self.control_open = false;
                self.begin_stop(StopCause::Stopped);
            }
        }
    }

    /// Esc: the running turn stops, and the session stays open for what its CLI runs in the
    /// background. A CLI with no line for that is stopped whole.
    fn stop_turn(&mut self) {
        let line = self.live.parser.stop_turn_line();
        if self.write_control(line) {
            self.turn_stopped = true;
        } else {
            self.begin_stop(StopCause::Stopped);
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

    /// Steering held for the running tool call goes in now with a uuid, and the CLI hands it to
    /// the model at once, the tool moving to the background (Claude Code's send now).
    fn steer_held_now(&mut self) {
        if self.steer_after_tool.is_empty() || !self.ctx.adapter.capabilities().send_now {
            return;
        }
        let held = std::mem::take(&mut self.steer_after_tool);
        let items = held
            .iter()
            .map(|message| Pending::message(message, Delivery::Steer))
            .collect();
        let uuid = uuid::Uuid::new_v4().to_string();
        if !self.write_input_as(items, Some(&uuid)) {
            self.steer_after_tool = held;
            return;
        }
        self.joining.push(uuid.clone());
        let line = self.ctx.adapter.send_now_line(&uuid);
        self.write_control(line);
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

    /// A task that ends between turns starts a turn of the CLI's own, to tell the model, and
    /// that turn's result follows like any other's.
    fn background_changed(&mut self, running: usize, ended: bool) {
        if ended && self.turn_done() && self.live.input_open && self.stop_cause.is_none() {
            self.live.turns_sent += 1;
        }
        self.live.background = running;
        if running == 0 {
            self.live.background_until = None;
        }
    }

    /// An hour from now, or the session's wall-clock limit when that comes first, since the
    /// session's time covers what it runs in the background too.
    fn background_deadline(&self) -> TokioInstant {
        let wait = TokioInstant::now() + BACKGROUND_WAIT;
        let wall = self
            .ctx
            .spec
            .limits
            .max_minutes
            .and_then(|minutes| std::time::Duration::try_from_secs_f64(minutes * 60.0).ok())
            .map(|limit| TokioInstant::from_std(self.started + limit));
        wall.map_or(wait, |wall| wall.min(wait))
    }

    /// The turn ended long ago and the background still runs: closing the input has the CLI
    /// stop it.
    fn background_wait_over(&mut self) {
        self.live.background_until = None;
        if self.live.background == 0 || !self.live.input_open || !self.turn_done() {
            return;
        }
        let tasks = match self.live.background {
            1 => "a task".to_owned(),
            n => format!("{n} tasks"),
        };
        self.out.notice(format!(
            "{} still ran {tasks} in the background when the wait for it ran out, so its input \
             closed and it stops them",
            self.ctx.adapter.display_name()
        ));
        self.live.process.close_input();
        self.live.input_open = false;
    }

    /// Every turn sent so far has its result.
    fn turn_done(&self) -> bool {
        self.obs.facts.results >= self.live.turns_sent
    }

    /// The turn is done and so is the work it started. A turn that starts the CLI's own
    /// workflow has its result as soon as the workflow runs in the background, so a session is
    /// still at work while that workflow runs, or while it owes the report only the tool takes.
    fn work_done(&self) -> bool {
        let workflow_running = self.live.background > 0 && !self.live.workflows.is_empty();
        let owes_its_report =
            report_tool::reports_by_tool(&self.ctx.spec) && self.obs.facts.structured.is_none();
        self.turn_done() && !workflow_running && !owes_its_report
    }

    /// Sends `items` through the open input: a turn of their own, or a part of the running turn
    /// where the adapter says mid-turn input joins it.
    fn write_input(&mut self, items: Vec<Pending>) -> bool {
        self.write_input_as(items, None)
    }

    /// As [`Self::write_input`], with the uuid the CLI reports the message's progress by.
    fn write_input_as(&mut self, items: Vec<Pending>, uuid: Option<&str>) -> bool {
        if !self.live.input_open || self.stop_cause.is_some() {
            return false;
        }
        let parts = joined_parts(&items);
        let text = prompt_text(&parts);
        let line = match uuid {
            Some(uuid) => self.ctx.adapter.message_line(&text, uuid),
            None => self.ctx.adapter.input_line(&text),
        };
        let Some(mut line) = line else {
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

    /// Your message, written at once for a CLI that holds it until the model can read it.
    /// Between turns it starts the next one; mid-turn it joins the running one. One that
    /// can't go out says so, and goes as the conversation's next turn instead.
    fn send_message(&mut self, text: &str, pictures: &[Picture], uuid: String) {
        let line = if pictures.is_empty() {
            self.ctx.adapter.message_line(text, &uuid)
        } else {
            self.ctx.adapter.picture_line(text, pictures, &uuid)
        };
        let between_turns = self.turn_done();
        if !self.write_control(line) {
            self.out.agent(AgentEvent::MessageState {
                uuid,
                state: MessageStage::NotSent,
            });
            return;
        }
        if between_turns {
            self.live.turns_sent += 1;
        } else {
            self.joining.push(uuid);
        }
    }

    /// A line the adapter made for the open input, such as a control request. False when it
    /// didn't go out.
    /// A CLI that can't say gets nothing, and one that answers with an error isn't asked
    /// again until the next fill.
    fn ask_context(&mut self, detail: ContextDetail) {
        let line = self.ctx.adapter.context_usage_line(detail);
        self.write_control(line);
    }

    fn write_control(&mut self, line: Option<String>) -> bool {
        let Some(mut line) = line else {
            return false;
        };
        if !self.live.input_open || self.stop_cause.is_some() {
            return false;
        }
        if !line.ends_with('\n') {
            line.push('\n');
        }
        if !self.live.process.send_input(line) {
            self.live.input_open = false;
            return false;
        }
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

    fn wrap_up(&mut self, dimension: Option<LimitDimension>) {
        self.send_wrap_up(|via| Pending::wrap_up(dimension, via));
    }

    /// The one wrap-up a session gets, whichever asked for it.
    fn send_wrap_up(&mut self, wrap_up: impl Fn(WrapUpVia) -> Pending) {
        if self.wrap_up_sent {
            return;
        }
        match self.steer {
            SteerSupport::Input => {
                if self.write_input(vec![wrap_up(WrapUpVia::Input)]) {
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
                self.resume_with.push(wrap_up(WrapUpVia::InterruptResume));
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

    /// Interrupts for a resume once no tool call is running and the CLI session is known.
    fn try_interrupt(&mut self) {
        let ready = !self.resume_with.is_empty()
            && self.stopping.is_none()
            && self.stop_cause.is_none()
            && !self.live.exited
            && self.obs.facts.open_tools.is_empty()
            && self.obs.facts.cli_session.is_some();
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
        self.finished_before_stop = self.work_done();
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
                // The work is done and the CLI is ending on its own.
                Action::WrapUp(_) | Action::CheckAndReport | Action::Nudge { .. } if idle => {}
                Action::Stop(StopCause::Limit(_) | StopCause::Rotate { .. }) if idle => {}
                Action::WrapUp(dimension) => self.wrap_up(dimension),
                Action::CheckAndReport => self.send_wrap_up(|_| Pending::check_and_report()),
                Action::Nudge { call, times } => self.nudge(call, times),
                Action::Stop(cause) => self.begin_stop(cause),
                Action::AskContext(detail) => self.ask_context(detail),
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
        // What the model hadn't read when the turn ended is the next turn, in this process.
        if input_turn_over && !self.joining.is_empty() {
            self.joining.clear();
            self.live.turns_sent += 1;
            return;
        }
        if input_turn_over {
            if !self.next_turn.is_empty() {
                self.flush_next_turn();
            } else if self.live.background > 0 {
                // The CLI stops what runs in the background soon after its input closes, so
                // the input stays open.
                if self.live.input_open && self.live.background_until.is_none() {
                    self.live.background_until = Some(self.background_deadline());
                }
            } else if self.owes_report() {
                self.report_asked = true;
                self.write_input(vec![Pending::report_asked()]);
            } else if self.live.input_open {
                self.close_input_after_count();
            }
        }
    }

    /// Every turn is counted once as it ends, in short, before its input closes, so the
    /// screen and the record keep what fills the session though nothing showed it while it
    /// ran. The input closes when the count comes, or when the wait for it is over. A CLI that
    /// can't count closes at once.
    fn close_input_after_count(&mut self) {
        if self.live.count_until.is_some() {
            return;
        }
        if self.live.counted_turn != Some(self.live.turns_sent) {
            self.live.counted_turn = Some(self.live.turns_sent);
            let line = self.ctx.adapter.context_usage_line(ContextDetail::Summary);
            if self.write_control(line) {
                self.obs.asked_context();
                self.live.count_until = Some(TokioInstant::now() + COUNT_WAIT);
                return;
            }
        }
        self.live.process.close_input();
        self.live.input_open = false;
    }

    /// The count asked at the turn's end didn't come in time, so the input closes without it.
    fn count_wait_over(&mut self) {
        self.live.count_until = None;
        self.after_events(Vec::new());
    }

    fn holds_report(&mut self, event: &AgentEvent) -> bool {
        report_tool::reports_by_tool(&self.ctx.spec)
            && self.held_reports.holds(
                event,
                self.live.background,
                &report_tool::report_file(&self.ctx.spec),
            )
    }

    /// Its work is done and the tool has no report it could keep, which it's asked for once.
    fn owes_report(&self) -> bool {
        self.live.input_open
            && !self.report_asked
            && report_tool::reports_by_tool(&self.ctx.spec)
            && self.obs.facts.structured.is_none()
    }
}
