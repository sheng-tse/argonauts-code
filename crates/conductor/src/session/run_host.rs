//! Hosts one agent session. It starts the adapter's command, turns its output into events,
//! records them, feeds the spend meter, and stops the process group when told to.

use std::collections::VecDeque;
use std::process::ExitStatus;
use std::time::Duration;

use anyhow::Context as _;
use argonauts_core::{
    Adapter, AgentEvent, CommandSpec, ErrorCategory, MeterAction, MeterLimit, Note, NoteLevel,
    Report, RunEventKind, SessionEnded, SessionRequest, SessionRole, SessionStarted, SpendMeter,
    StopCause, StopPlan, StopSignal, StopStep, StreamParser, TaskId, WRAP_UP_MESSAGE,
    builtin_price,
};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep_until};

use crate::limits::control::{ConductorSettings, StopHandle};
use crate::process::{AgentProcess, MAX_LINE_BYTES, OutputLine, ProcessEvent, Stream, exit_parts};
use crate::session::recorder::{CONDUCTOR_SOURCE, Recorder};

/// Streamed text kept for finding a report when no final message arrives, in bytes.
const STREAMED_TEXT_CAP: usize = 256 * 1024;

const TIME_CHECK: Duration = Duration::from_secs(1);

/// How much of an over-long output line is kept in the log, in bytes.
const OVERSIZED_PREFIX: usize = 2048;

const STDERR_TAIL_LINES: usize = 20;

const AFTER_KILL_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapUpDelivery {
    Input,
    /// Sent as SIGINT, because the CLI takes no input mid-run.
    Interrupt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinalResult {
    pub text: Option<String>,
    /// When a schema was enforced.
    pub structured: Option<Value>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub duration: Duration,
    pub stopped_by: Option<StopCause>,
    /// CLI session id, used to resume.
    pub session_id: Option<String>,
    pub result: Option<FinalResult>,
    pub last_message: Option<String>,
    /// Top-level streamed text since the last complete message, capped.
    pub streamed_text: String,
    pub wrap_up: Option<WrapUpDelivery>,
    pub start_error: Option<String>,
    pub last_error: Option<String>,
    pub stderr_tail: Vec<String>,
}

impl SessionOutcome {
    /// Whether the session ran to a clean finish on its own.
    pub fn succeeded(&self) -> bool {
        self.start_error.is_none()
            && self.stopped_by.is_none()
            && self.exit_code == Some(0)
            && !self.result.as_ref().is_some_and(|r| r.is_error)
    }

    /// The agent's report, from structured output if there was any, else from its final text.
    pub fn report(&self) -> Option<Report> {
        if let Some(result) = &self.result {
            if let Some(report) = result
                .structured
                .as_ref()
                .and_then(|v| Report::from_value(v).ok())
            {
                return Some(report);
            }
            if let Some(report) = result.text.as_deref().and_then(Report::from_text) {
                return Some(report);
            }
        }
        self.last_message
            .as_deref()
            .and_then(Report::from_text)
            .or_else(|| Report::from_text(&self.streamed_text))
    }

    pub fn failure(&self) -> Option<String> {
        if let Some(error) = &self.start_error {
            return Some(error.clone());
        }
        if let Some(cause) = self.stopped_by {
            return Some(
                match cause {
                    StopCause::Limit => "stopped at its limit",
                    StopCause::User => "stopped by you",
                    StopCause::Timeout => "stopped because the session time limit passed",
                }
                .to_owned(),
            );
        }
        let why = self
            .last_error
            .clone()
            .or_else(|| self.stderr_tail.last().cloned());
        let with_why = |head: String| match &why {
            Some(why) => format!("{head}: {why}"),
            None => head,
        };
        if self.result.as_ref().is_some_and(|r| r.is_error) {
            return Some(with_why("the agent reported an error".to_owned()));
        }
        match (self.exit_code, self.signal) {
            (Some(0), _) => None,
            (Some(code), _) => Some(with_why(format!("the agent exited with code {code}"))),
            (None, Some(signal)) => {
                Some(with_why(format!("the agent was killed by signal {signal}")))
            }
            (None, None) => Some(with_why(
                "the agent ended without an exit status".to_owned(),
            )),
        }
    }
}

pub struct RunHost<'a> {
    adapter: &'a dyn Adapter,
    task: &'a TaskId,
    recorder: &'a mut Recorder,
    meter: &'a mut SpendMeter,
    stop: &'a StopHandle,
    settings: &'a ConductorSettings,
}

impl<'a> RunHost<'a> {
    pub fn new(
        adapter: &'a dyn Adapter,
        task: &'a TaskId,
        recorder: &'a mut Recorder,
        meter: &'a mut SpendMeter,
        stop: &'a StopHandle,
        settings: &'a ConductorSettings,
    ) -> Self {
        Self {
            adapter,
            task,
            recorder,
            meter,
            stop,
            settings,
        }
    }

    /// Runs one session to the end, even when the agent couldn't start. An error means
    /// Argonauts Code itself failed, such as the event log becoming unwritable, and the group is
    /// killed.
    pub async fn run_session(
        &mut self,
        request: &SessionRequest,
    ) -> anyhow::Result<SessionOutcome> {
        let started = Instant::now();
        if let Some(refusal) = self.refusal(request) {
            return self.start_failed(refusal, started);
        }
        let spec = match self.adapter.command(request) {
            Ok(spec) => spec,
            Err(err) => {
                let message = format!("could not build the agent command: {err:#}");
                return self.start_failed(message, started);
            }
        };
        self.record_start(request, &spec)?;
        let process = match AgentProcess::spawn(&spec) {
            Ok(process) => process,
            Err(err) => return self.start_failed(err.to_string(), started),
        };
        self.note_end_of_turn_usage()?;

        let mut live = LiveSession {
            process,
            parser: self.adapter.session_parser(request),
            state: SessionState {
                limit_level: self.meter.level(),
                ..SessionState::default()
            },
            stopping: None,
        };
        let exit_status = self.host(&mut live, started).await?;
        self.end_session(live, exit_status, started)
    }

    fn refusal(&self, request: &SessionRequest) -> Option<String> {
        match request.role {
            SessionRole::ReadOnly | SessionRole::NoTools
                if !self.adapter.capabilities().enforces_read_only =>
            {
                Some(format!(
                    "{} can't keep a session read-only, so Argonauts Code won't start one",
                    self.adapter.display_name()
                ))
            }
            SessionRole::Build | SessionRole::Direct
                if request
                    .goal_hash
                    .as_deref()
                    .is_none_or(|hash| hash.trim().is_empty()) =>
            {
                Some("a build session needs the approved goal's hash".to_owned())
            }
            _ => None,
        }
    }

    fn start_failed(
        &mut self,
        message: String,
        started: Instant,
    ) -> anyhow::Result<SessionOutcome> {
        self.record_agent(AgentEvent::Error {
            category: ErrorCategory::Cli,
            message: message.clone(),
            retryable: false,
        })?;
        self.record(RunEventKind::SessionEnded(SessionEnded {
            exit_code: None,
            signal: None,
            duration_ms: millis(started.elapsed()),
            stopped_by: None,
        }))?;
        Ok(SessionOutcome {
            duration: started.elapsed(),
            start_error: Some(message),
            ..SessionOutcome::default()
        })
    }

    fn record_start(&mut self, request: &SessionRequest, spec: &CommandSpec) -> anyhow::Result<()> {
        self.record(RunEventKind::SessionStarted(SessionStarted {
            agent: self.adapter.id().to_owned(),
            model: request.model.clone(),
            effort: request.effort.clone(),
            role: request.role,
            program: spec.program.clone(),
            args: spec.args.clone(),
            cwd: spec.cwd.clone(),
            prompt: request.prompt.clone(),
            goal_hash: request.goal_hash.clone(),
        }))
    }

    fn note_end_of_turn_usage(&mut self) -> anyhow::Result<()> {
        if self.adapter.capabilities().live_usage || self.meter.limit() == MeterLimit::None {
            return Ok(());
        }
        self.note(
            NoteLevel::Info,
            &format!(
                "{} reports usage only when its turn ends, so spend is checked then. The time and tool-call limits apply while it works.",
                self.adapter.display_name()
            ),
        )
    }

    async fn host(
        &mut self,
        live: &mut LiveSession,
        started: Instant,
    ) -> anyhow::Result<Option<ExitStatus>> {
        let stop = self.stop;
        let timeout_at = self.settings.session_timeout.map(|limit| started + limit);
        let time_check = self.time_check_interval();
        let mut next_time_check = time_check.map(|every| started + every);
        let mut exit_status = None;
        let mut drain_until: Option<Instant> = None;
        let mut leftovers_killed = false;

        loop {
            let running = !live.state.exited;
            let can_stop = running && live.stopping.is_none();
            let stop_step_at = live.stopping.as_ref().and_then(|s| s.next_step_at);
            let checking_time = next_time_check.is_some() && running && live.state.result.is_none();
            tokio::select! {
                event = live.process.next_event() => match event {
                    Some(ProcessEvent::Line(line)) => self.handle_line(&line, live)?,
                    Some(ProcessEvent::Exited(status)) => {
                        exit_status = Some(status.context("waiting for the agent to exit")?);
                        live.state.exited = true;
                        drain_until = Some(Instant::now() + self.settings.exit_grace);
                    }
                    None => break,
                },
                () = sleep_until(or_far(drain_until)), if drain_until.is_some() => {
                    if leftovers_killed {
                        break;
                    }
                    // Something the agent started still holds its output open.
                    kill_leftovers(&live.process);
                    leftovers_killed = true;
                    drain_until = Some(Instant::now() + AFTER_KILL_GRACE);
                }
                () = sleep_until(or_far(stop_step_at)), if stop_step_at.is_some() && running => {
                    if let Some(stopping) = live.stopping.as_mut() {
                        stopping.next_step(&live.process);
                    }
                }
                () = stop.stopped(), if can_stop => {
                    live.begin_stop(StopCause::User, self.adapter.stop_plan());
                }
                () = sleep_until(or_far(timeout_at)), if timeout_at.is_some() && can_stop => {
                    self.note(NoteLevel::Warn, "The session time limit passed. Stopping the agent.")?;
                    live.begin_stop(StopCause::Timeout, self.adapter.stop_plan());
                }
                () = sleep_until(or_far(next_time_check)), if checking_time => {
                    next_time_check = time_check.map(|every| Instant::now() + every);
                    let action = self.meter.observe_time(started.elapsed());
                    self.record_level_change(&mut live.state)?;
                    self.act_on_limit(action, live, false)?;
                }
            }
        }

        if !leftovers_killed {
            kill_leftovers(&live.process);
        }
        Ok(exit_status)
    }

    fn time_check_interval(&self) -> Option<Duration> {
        self.meter.work_limits().max_minutes.map(|minutes| {
            // A hundredth of the limit, so short limits still stop close to on time.
            let hundredth = Duration::from_secs_f64(minutes * 60.0 / 100.0);
            TIME_CHECK.min(hundredth.max(Duration::from_millis(10)))
        })
    }

    fn end_session(
        &mut self,
        mut live: LiveSession,
        exit_status: Option<ExitStatus>,
        started: Instant,
    ) -> anyhow::Result<SessionOutcome> {
        while let Some(line) = live.process.try_next_line() {
            self.handle_line(&line, &mut live)?;
        }
        live.process.stop_reading();

        let (exit_code, signal) = exit_status.as_ref().map_or((None, None), exit_parts);
        for event in live.parser.finish(exit_code) {
            self.handle_event(event, &mut live, true)?;
        }

        let duration = started.elapsed();
        self.meter.observe_time(duration);
        self.record_level_change(&mut live.state)?;
        let stopped_by = live.stopping.map(|s| s.cause);
        self.record(RunEventKind::SessionEnded(SessionEnded {
            exit_code,
            signal,
            duration_ms: millis(duration),
            stopped_by,
        }))?;
        Ok(live
            .state
            .into_outcome(exit_code, signal, duration, stopped_by))
    }

    fn handle_line(&mut self, line: &OutputLine, live: &mut LiveSession) -> anyhow::Result<()> {
        if line.dropped > 0 {
            return self.oversized_line(line, &mut live.state);
        }
        let events = match line.stream {
            Stream::Stdout => live.parser.parse_stdout_line(&line.text),
            Stream::Stderr => {
                live.state.push_stderr(&line.text);
                live.parser.parse_stderr_line(&line.text)
            }
        };
        // Usage that comes with the final result arrives after the work is done.
        let ends_turn = events
            .iter()
            .any(|event| matches!(event, AgentEvent::Result { .. }));
        for event in events {
            self.handle_event(event, live, ends_turn)?;
        }
        Ok(())
    }

    /// Keeps the start of a line too long to parse, and says that its event was lost.
    fn oversized_line(
        &mut self,
        line: &OutputLine,
        session: &mut SessionState,
    ) -> anyhow::Result<()> {
        let mut cut = OVERSIZED_PREFIX.min(line.text.len());
        while !line.text.is_char_boundary(cut) {
            cut -= 1;
        }
        let stream = match line.stream {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        };
        self.record_agent(AgentEvent::Unknown {
            raw: json!({
                "oversized_line": stream,
                "bytes": line.text.len() + line.dropped,
                "start": &line.text[..cut],
            }),
        })?;
        if !session.oversized_noted {
            session.oversized_noted = true;
            let limit = MAX_LINE_BYTES / (1024 * 1024);
            self.note(
                NoteLevel::Warn,
                &format!(
                    "The agent printed a line longer than {limit} MiB, so it wasn't read and the event it carried is lost."
                ),
            )?;
        }
        Ok(())
    }

    /// `ends_turn` means the event came with the session's final result, so there is nothing left
    /// to stop.
    fn handle_event(
        &mut self,
        event: AgentEvent,
        live: &mut LiveSession,
        ends_turn: bool,
    ) -> anyhow::Result<()> {
        live.state.observe(&event);
        let model = match &event {
            AgentEvent::Init {
                model: Some(model), ..
            } => Some(model.clone()),
            _ => None,
        };
        let action = self.meter.observe(&event);
        let is_usage = matches!(event, AgentEvent::Usage { .. });
        self.record_agent(event)?;
        if let Some(model) = model {
            self.learn_price(&model)?;
        }
        if is_usage {
            live.state.limit_level = self.meter.level();
            self.record(RunEventKind::Spend(self.meter.status()))?;
        } else {
            self.record_level_change(&mut live.state)?;
        }
        let settled = ends_turn || live.state.exited || live.state.result.is_some();
        self.act_on_limit(action, live, settled)
    }

    /// Records the meter when its level moved without a usage event, such as from tool calls.
    fn record_level_change(&mut self, session: &mut SessionState) -> anyhow::Result<()> {
        let level = self.meter.level();
        if level != session.limit_level {
            session.limit_level = level;
            self.record(RunEventKind::Spend(self.meter.status()))?;
        }
        Ok(())
    }

    fn act_on_limit(
        &mut self,
        action: MeterAction,
        live: &mut LiveSession,
        settled: bool,
    ) -> anyhow::Result<()> {
        if live.stopping.is_some() {
            return Ok(());
        }
        match action {
            MeterAction::Continue => Ok(()),
            MeterAction::Stop if settled => {
                if live.state.over_limit_noted {
                    return Ok(());
                }
                live.state.over_limit_noted = true;
                let limit = self.meter.status().limit_text();
                self.note(
                    NoteLevel::Warn,
                    &format!("The agent finished, but it used up {limit}."),
                )
            }
            MeterAction::WrapUp if settled => Ok(()),
            MeterAction::WrapUp => self.wrap_up(live),
            MeterAction::Stop => {
                let limit = self.meter.status().limit_text();
                self.note(
                    NoteLevel::Warn,
                    &format!("Stopping the agent: it used up {limit}."),
                )?;
                live.begin_stop(StopCause::Limit, self.adapter.stop_plan());
                Ok(())
            }
        }
    }

    /// Prices the session's model from the built-in table when no price is configured.
    fn learn_price(&mut self, model: &str) -> anyhow::Result<()> {
        if self.meter.has_price() {
            return Ok(());
        }
        if let Some(price) = builtin_price(model) {
            self.meter.set_price(Some(price));
            return Ok(());
        }
        if !matches!(self.meter.limit(), MeterLimit::Usd(_)) {
            return Ok(());
        }
        let name = self.adapter.display_name();
        let id = self.adapter.id();
        if self.adapter.capabilities().reports_cost {
            self.note(
                NoteLevel::Info,
                &format!(
                    "Argonauts Code has no price for {model}, so spend shows only once {name} reports its cost, and the limit can't ask for an early wrap-up."
                ),
            )
        } else {
            self.note(
                NoteLevel::Warn,
                &format!(
                    "{name} reports only tokens and Argonauts Code has no price for {model}, so the dollar limit can't count its spend. Set agents.{id}.price or use a token limit. The time and tool-call limits still apply."
                ),
            )
        }
    }

    fn wrap_up(&mut self, live: &mut LiveSession) -> anyhow::Result<()> {
        let used = self
            .meter
            .fraction_used()
            .map_or_else(|| "most".to_owned(), |f| format!("{:.0}%", f * 100.0));
        let limit = self.meter.status().limit_text();
        let sent = self
            .adapter
            .wrap_up_input(WRAP_UP_MESSAGE)
            .is_some_and(|text| live.process.send_input(text));
        let text = if sent {
            live.state.wrap_up = Some(WrapUpDelivery::Input);
            format!("{used} of {limit} is used. Asked the agent to wrap up.")
        } else {
            if let Err(err) = live.process.signal(StopSignal::Interrupt) {
                tracing::warn!("could not interrupt the agent: {err}");
            }
            live.state.wrap_up = Some(WrapUpDelivery::Interrupt);
            format!("{used} of {limit} is used. Interrupted the agent so it can wrap up.")
        };
        self.note(NoteLevel::Info, &text)
    }

    fn note(&mut self, level: NoteLevel, text: &str) -> anyhow::Result<()> {
        self.record(RunEventKind::Note(Note {
            level,
            text: text.to_owned(),
        }))
    }

    fn record_agent(&mut self, event: AgentEvent) -> anyhow::Result<()> {
        self.recorder.record(
            Some(self.task),
            self.adapter.id(),
            RunEventKind::Agent(event),
        )?;
        Ok(())
    }

    fn record(&mut self, kind: RunEventKind) -> anyhow::Result<()> {
        self.recorder
            .record(Some(self.task), CONDUCTOR_SOURCE, kind)?;
        Ok(())
    }
}

struct LiveSession {
    process: AgentProcess,
    parser: Box<dyn StreamParser>,
    state: SessionState,
    stopping: Option<Stopping>,
}

impl LiveSession {
    fn begin_stop(&mut self, cause: StopCause, plan: StopPlan) {
        self.stopping = Some(Stopping::begin(cause, plan, &self.process));
    }
}

#[derive(Debug)]
struct SessionState {
    session_id: Option<String>,
    result: Option<FinalResult>,
    last_message: Option<String>,
    streamed: String,
    wrap_up: Option<WrapUpDelivery>,
    last_error: Option<String>,
    stderr_tail: VecDeque<String>,
    exited: bool,
    limit_level: MeterAction,
    over_limit_noted: bool,
    oversized_noted: bool,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            session_id: None,
            result: None,
            last_message: None,
            streamed: String::new(),
            wrap_up: None,
            last_error: None,
            stderr_tail: VecDeque::new(),
            exited: false,
            limit_level: MeterAction::Continue,
            over_limit_noted: false,
            oversized_noted: false,
        }
    }
}

impl SessionState {
    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init {
                session_id: Some(id),
                ..
            } => self.session_id = Some(id.clone()),
            AgentEvent::TextDelta { text, parent: None } => self.push_streamed(text),
            AgentEvent::Message { text, parent: None } => {
                self.last_message = Some(text.clone());
                self.streamed.clear();
            }
            AgentEvent::Result {
                text,
                structured,
                is_error,
            } => {
                self.result = Some(FinalResult {
                    text: text.clone(),
                    structured: structured.clone(),
                    is_error: *is_error,
                });
            }
            // A stall is a warning the agent may yet answer, not why it ended.
            AgentEvent::Error {
                category: ErrorCategory::Stalled,
                ..
            } => {}
            AgentEvent::Error { message, .. } => self.last_error = Some(message.clone()),
            _ => {}
        }
    }

    fn push_streamed(&mut self, text: &str) {
        self.streamed.push_str(text);
        if self.streamed.len() > STREAMED_TEXT_CAP * 2 {
            let mut cut = self.streamed.len() - STREAMED_TEXT_CAP;
            while !self.streamed.is_char_boundary(cut) {
                cut += 1;
            }
            self.streamed.drain(..cut);
        }
    }

    fn push_stderr(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.stderr_tail.len() == STDERR_TAIL_LINES {
            self.stderr_tail.pop_front();
        }
        self.stderr_tail.push_back(line.to_owned());
    }

    fn into_outcome(
        self,
        exit_code: Option<i32>,
        signal: Option<i32>,
        duration: Duration,
        stopped_by: Option<StopCause>,
    ) -> SessionOutcome {
        SessionOutcome {
            exit_code,
            signal,
            duration,
            stopped_by,
            session_id: self.session_id,
            result: self.result,
            last_message: self.last_message,
            streamed_text: self.streamed,
            wrap_up: self.wrap_up,
            start_error: None,
            last_error: self.last_error,
            stderr_tail: self.stderr_tail.into(),
        }
    }
}

struct Stopping {
    cause: StopCause,
    steps: std::vec::IntoIter<StopStep>,
    next_step_at: Option<Instant>,
}

impl Stopping {
    fn begin(cause: StopCause, plan: StopPlan, process: &AgentProcess) -> Self {
        let mut stopping = Self {
            cause,
            steps: complete_plan(plan).into_iter(),
            next_step_at: None,
        };
        stopping.next_step(process);
        stopping
    }

    fn next_step(&mut self, process: &AgentProcess) {
        self.next_step_at = self.steps.next().map(|step| {
            if let Err(err) = process.signal(step.signal) {
                tracing::warn!("could not signal the agent: {err}");
            }
            Instant::now() + step.wait
        });
    }
}

/// The plan's steps, ending in SIGKILL so a stopped agent never keeps running.
fn complete_plan(plan: StopPlan) -> Vec<StopStep> {
    let mut steps = plan.steps;
    if steps
        .last()
        .is_none_or(|step| step.signal != StopSignal::Kill)
    {
        steps.push(StopStep {
            signal: StopSignal::Kill,
            wait: Duration::from_secs(2),
        });
    }
    steps
}

fn kill_leftovers(process: &AgentProcess) {
    if let Err(err) = process.signal(StopSignal::Kill) {
        tracing::warn!("could not kill processes the agent left behind: {err}");
    }
}

/// The deadline, or a year away for a `select!` branch that is disabled anyway.
fn or_far(deadline: Option<Instant>) -> Instant {
    deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(365 * 24 * 60 * 60))
}

pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ScriptAdapter, events_of, final_line, open_recorder, usage_line};
    use argonauts_core::{
        Capabilities, EventLog, MeterLimit, ReportStatus, RunEvent, SessionRole, WorkLimits,
    };

    struct TestDriver {
        dir: tempfile::TempDir,
        recorder: Recorder,
        meter: SpendMeter,
        stop: StopHandle,
        settings: ConductorSettings,
        task: TaskId,
    }

    impl TestDriver {
        fn new(limit: MeterLimit) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let recorder = open_recorder(dir.path());
            Self {
                dir,
                recorder,
                meter: SpendMeter::new(limit, 0.8),
                stop: StopHandle::new(),
                settings: ConductorSettings {
                    exit_grace: Duration::from_millis(300),
                    ..ConductorSettings::default()
                },
                task: TaskId::numbered(1),
            }
        }

        fn request(&self) -> SessionRequest {
            SessionRequest {
                workdir: self.dir.path().to_owned(),
                prompt: "do the thing".into(),
                model: Some("tiny".into()),
                role: SessionRole::Build,
                goal_hash: Some("goal-hash".into()),
                ..SessionRequest::default()
            }
        }

        async fn run(&mut self, adapter: &ScriptAdapter) -> SessionOutcome {
            let request = self.request();
            self.run_request(adapter, &request).await
        }

        async fn run_request(
            &mut self,
            adapter: &ScriptAdapter,
            request: &SessionRequest,
        ) -> SessionOutcome {
            RunHost::new(
                adapter,
                &self.task,
                &mut self.recorder,
                &mut self.meter,
                &self.stop,
                &self.settings,
            )
            .run_session(request)
            .await
            .unwrap()
        }

        fn events(&self) -> Vec<RunEvent> {
            EventLog::read(self.recorder.log_path()).unwrap().events
        }

        fn notes(&self) -> Vec<String> {
            self.events()
                .into_iter()
                .filter_map(|e| match e.kind {
                    RunEventKind::Note(note) => Some(note.text),
                    _ => None,
                })
                .collect()
        }
    }

    const REPORT_RESULT: &str = r#"{"type":"result","text":null,"structured":{"status":"done","summary":"ok","files_changed":[],"tests_run":[],"concerns":[]},"is_error":false}"#;

    #[test]
    fn a_stall_is_never_why_a_session_failed() {
        let error = |category, message: &str| AgentEvent::Error {
            category,
            message: message.into(),
            retryable: true,
        };
        let stall = "Fake Agent has sent nothing for 45 seconds, not even its heartbeat";
        let mut state = SessionState::default();
        state.observe(&error(ErrorCategory::Stalled, stall));
        let outcome = state.into_outcome(Some(1), None, Duration::ZERO, None);
        assert_eq!(outcome.last_error, None);
        let failure = outcome.failure().unwrap_or_default();
        assert!(!failure.contains("heartbeat"), "{failure}");

        // A real error stays the reason, though a stall comes after it.
        let mut state = SessionState::default();
        state.observe(&error(ErrorCategory::Server, "the server failed"));
        state.observe(&error(ErrorCategory::Stalled, stall));
        let outcome = state.into_outcome(Some(1), None, Duration::ZERO, None);
        assert_eq!(outcome.last_error.as_deref(), Some("the server failed"));
    }

    #[tokio::test]
    async fn records_the_session_and_its_events() {
        let mut h = TestDriver::new(MeterLimit::None);
        let adapter = ScriptAdapter::new(&format!(
            "echo '{{\"type\":\"init\",\"session_id\":\"s-1\",\"model\":\"tiny\",\"tools\":[]}}'
             echo 'not json'
             echo 'warning: something' >&2
             echo '{}'
             echo '{REPORT_RESULT}'",
            usage_line(10, 5, Some(0.01))
        ));
        let outcome = h.run(&adapter).await;
        assert!(outcome.succeeded(), "{outcome:?}");
        assert_eq!(outcome.failure(), None);
        assert_eq!(outcome.session_id.as_deref(), Some("s-1"));
        assert_eq!(outcome.report().unwrap().status, ReportStatus::Done);
        assert_eq!(outcome.stderr_tail, ["warning: something"]);

        let events = h.events();
        assert_eq!(
            events_of(&events),
            [
                "session_started",
                "agent:init",
                "agent:unknown",
                "agent:usage",
                "limit",
                "agent:result",
                "session_ended"
            ]
        );
        let RunEventKind::SessionStarted(started) = &events[0].kind else {
            panic!("expected session_started")
        };
        assert_eq!(started.prompt, "do the thing");
        assert_eq!(started.model.as_deref(), Some("tiny"));
        assert_eq!(events[1].source, "fake");
        assert_eq!(events[0].source, CONDUCTOR_SOURCE);
    }

    #[tokio::test]
    async fn limit_stop_steps_up_to_kill() {
        let mut h = TestDriver::new(MeterLimit::Usd(0.05));
        // Ignores SIGINT and SIGTERM, so only SIGKILL ends it.
        let adapter = ScriptAdapter::new(&format!(
            "trap '' INT TERM
             echo '{}'
             sleep 30",
            usage_line(1000, 100, Some(0.10))
        ))
        .with_stop_plan(StopPlan::stepped(
            Duration::from_millis(200),
            Duration::from_millis(200),
        ));
        let started = std::time::Instant::now();
        let outcome = h.run(&adapter).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(outcome.stopped_by, Some(StopCause::Limit));
        assert_eq!(outcome.signal, Some(9));
        assert!(!outcome.succeeded());
        assert_eq!(outcome.failure().unwrap(), "stopped at its limit");

        let events = h.events();
        let ended = events.iter().find_map(|e| match &e.kind {
            RunEventKind::SessionEnded(end) => Some(end.clone()),
            _ => None,
        });
        assert_eq!(ended.unwrap().stopped_by, Some(StopCause::Limit));
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            RunEventKind::Spend(status) if status.level == MeterAction::Stop
        )));
    }

    #[tokio::test]
    async fn plain_interrupt_is_enough_for_a_polite_cli() {
        let mut h = TestDriver::new(MeterLimit::Tokens(100));
        let adapter = ScriptAdapter::new(&format!(
            "echo '{}'
             sleep 30",
            usage_line(500, 10, None)
        ))
        .with_stop_plan(StopPlan::stepped(
            Duration::from_secs(5),
            Duration::from_secs(5),
        ));
        let started = std::time::Instant::now();
        let outcome = h.run(&adapter).await;
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(outcome.stopped_by, Some(StopCause::Limit));
        assert_eq!(outcome.signal, Some(2));
    }

    #[tokio::test]
    async fn wrap_up_goes_to_stdin_when_the_cli_accepts_it() {
        let mut h = TestDriver::new(MeterLimit::Usd(1.0));
        let adapter = ScriptAdapter::new(&format!(
            "echo '{}'
             read line
             printf '{{\"type\":\"message\",\"text\":\"%s\",\"parent\":null}}\\n' \"$line\"
             echo '{REPORT_RESULT}'",
            usage_line(10, 10, Some(0.9))
        ))
        .with_stdin_wrap_up();
        let outcome = h.run(&adapter).await;
        assert!(outcome.succeeded(), "{outcome:?}");
        assert_eq!(outcome.wrap_up, Some(WrapUpDelivery::Input));
        assert_eq!(outcome.last_message.as_deref(), Some(WRAP_UP_MESSAGE));
    }

    #[tokio::test]
    async fn wrap_up_interrupts_a_cli_without_input() {
        let mut h = TestDriver::new(MeterLimit::Usd(1.0));
        // Short sleeps, because sh runs a trap only after the current command finishes.
        let adapter = ScriptAdapter::new(&format!(
            "trap 'echo \"{{\\\"type\\\":\\\"message\\\",\\\"text\\\":\\\"interrupted\\\",\\\"parent\\\":null}}\"; exit 0' INT
             echo '{}'
             while :; do sleep 0.1; done",
            usage_line(10, 10, Some(0.9))
        ));
        let started = std::time::Instant::now();
        let outcome = h.run(&adapter).await;
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(outcome.wrap_up, Some(WrapUpDelivery::Interrupt));
        assert_eq!(outcome.stopped_by, None);
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.last_message.as_deref(), Some("interrupted"));
        assert!(h.events().iter().any(|e| matches!(
            &e.kind,
            RunEventKind::Note(note) if note.text.contains("Interrupted the agent")
        )));
    }

    #[tokio::test]
    async fn usage_that_comes_with_the_result_never_stops_a_finished_agent() {
        let mut h = TestDriver::new(MeterLimit::Usd(0.05));
        // Exits a little after its result, as real CLIs do.
        let adapter = ScriptAdapter::new(&format!(
            "echo '{{\"type\":\"init\",\"session_id\":\"s-1\",\"model\":\"tiny\",\"tools\":[]}}'
             echo '{last}'
             sleep 0.3",
            last = final_line(&usage_line(5000, 500, Some(0.40)), REPORT_RESULT),
        ))
        .with_stop_plan(StopPlan::stepped(
            Duration::from_secs(5),
            Duration::from_secs(5),
        ));
        let outcome = h.run(&adapter).await;
        assert!(outcome.succeeded(), "{outcome:?}");
        assert_eq!(outcome.stopped_by, None);
        assert_eq!(outcome.wrap_up, None);
        assert_eq!(outcome.exit_code, Some(0));
        let notes = h.notes();
        assert!(
            notes
                .iter()
                .any(|n| n == "The agent finished, but it used up the limit of $0.05."),
            "{notes:?}"
        );
        assert!(!notes.iter().any(|n| n.contains("Stopping")), "{notes:?}");
    }

    #[tokio::test]
    async fn final_usage_past_the_soft_ratio_does_not_interrupt() {
        let mut h = TestDriver::new(MeterLimit::Usd(1.0));
        let adapter = ScriptAdapter::new(&format!(
            "echo '{last}'; sleep 0.3",
            last = final_line(&usage_line(10, 10, Some(0.9)), REPORT_RESULT),
        ));
        let outcome = h.run(&adapter).await;
        assert!(outcome.succeeded(), "{outcome:?}");
        assert_eq!(outcome.wrap_up, None);
        assert!(h.notes().is_empty(), "{:?}", h.notes());
    }

    #[tokio::test]
    async fn the_tool_call_limit_stops_the_agent() {
        let mut h = TestDriver::new(MeterLimit::None);
        h.meter = SpendMeter::new(MeterLimit::None, 0.5).with_work_limits(WorkLimits {
            max_minutes: None,
            max_tool_calls: Some(2),
        });
        let tool = r#"{"type":"tool_started","id":"t","name":"Bash","input":{},"parent":null}"#;
        let adapter = ScriptAdapter::new(&format!(
            "trap 'exit 0' INT
             echo '{tool}'; sleep 0.2; echo '{tool}'
             while :; do sleep 0.1; done"
        ));
        let outcome = h.run(&adapter).await;
        assert_eq!(outcome.wrap_up, Some(WrapUpDelivery::Interrupt));
        assert_eq!(outcome.exit_code, Some(0));
        let notes = h.notes();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("the limit of 2 tool calls")),
            "{notes:?}"
        );
        assert!(h.events().iter().any(|e| matches!(
            &e.kind,
            RunEventKind::Spend(status) if status.level == MeterAction::WrapUp && status.work.tool_calls == 1
        )));
    }

    #[tokio::test]
    async fn the_time_limit_wraps_up_then_stops() {
        let mut h = TestDriver::new(MeterLimit::None);
        // 0.01 minutes is 0.6 s, so it wraps up at 0.3 s and stops at 0.6 s.
        h.meter = SpendMeter::new(MeterLimit::None, 0.5).with_work_limits(WorkLimits {
            max_minutes: Some(0.01),
            max_tool_calls: None,
        });
        let adapter =
            ScriptAdapter::new("trap '' INT; while :; do sleep 0.05; done").with_stop_plan(
                StopPlan::stepped(Duration::from_millis(200), Duration::from_millis(200)),
            );
        let started = std::time::Instant::now();
        let outcome = h.run(&adapter).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(outcome.wrap_up, Some(WrapUpDelivery::Interrupt));
        assert_eq!(outcome.stopped_by, Some(StopCause::Limit));
        let notes = h.notes();
        assert!(
            notes
                .iter()
                .any(|n| n == "Stopping the agent: it used up the 0.01-minute limit."),
            "{notes:?}"
        );
    }

    #[tokio::test]
    async fn build_sessions_need_a_goal_hash() {
        let mut h = TestDriver::new(MeterLimit::None);
        let request = SessionRequest {
            goal_hash: None,
            ..h.request()
        };
        let outcome = h.run_request(&ScriptAdapter::new("true"), &request).await;
        assert_eq!(
            outcome.start_error.as_deref(),
            Some("a build session needs the approved goal's hash")
        );
        assert!(
            !h.events()
                .iter()
                .any(|e| matches!(e.kind, RunEventKind::SessionStarted(_)))
        );
    }

    #[tokio::test]
    async fn read_only_sessions_need_an_adapter_that_enforces_them() {
        let mut h = TestDriver::new(MeterLimit::None);
        let adapter = ScriptAdapter::new("true").with_capabilities(Capabilities {
            enforces_read_only: false,
            ..Capabilities::default()
        });
        let request = SessionRequest {
            role: SessionRole::ReadOnly,
            ..h.request()
        };
        let outcome = h.run_request(&adapter, &request).await;
        assert!(
            outcome
                .failure()
                .unwrap()
                .contains("can't keep a session read-only"),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_request_the_adapter_rejects_is_a_start_error() {
        let mut h = TestDriver::new(MeterLimit::None);
        let request = SessionRequest {
            model: Some("--oops".into()),
            ..h.request()
        };
        let outcome = h.run_request(&ScriptAdapter::new("true"), &request).await;
        assert!(
            outcome
                .failure()
                .unwrap()
                .starts_with("could not build the agent command"),
            "{outcome:?}"
        );
        assert_eq!(events_of(&h.events()), ["agent:error", "session_ended"]);
    }

    #[tokio::test]
    async fn the_model_named_at_init_sets_the_price() {
        let mut h = TestDriver::new(MeterLimit::Usd(10.0));
        let adapter = ScriptAdapter::new(&format!(
            "echo '{{\"type\":\"init\",\"session_id\":\"s\",\"model\":\"gpt-6-luna\",\"tools\":[]}}'
             echo '{usage}'",
            usage = usage_line(1_000_000, 0, None),
        ));
        h.run(&adapter).await;
        let spend = h.meter.spend();
        assert!((spend.cost_usd - 0.10).abs() < 1e-9, "{spend:?}");
        assert!(spend.cost_estimated && !spend.cost_partial);
    }

    #[tokio::test]
    async fn an_unpriced_model_is_called_out() {
        let mut h = TestDriver::new(MeterLimit::Usd(10.0));
        let adapter = ScriptAdapter::new(
            "echo '{\"type\":\"init\",\"session_id\":\"s\",\"model\":\"mystery-1\",\"tools\":[]}'",
        )
        .with_capabilities(Capabilities::default());
        h.run(&adapter).await;
        let notes = h.notes();
        assert!(
            notes[0].starts_with(
                "Fake Agent reports only tokens and Argonauts Code has no price for mystery-1"
            ),
            "{notes:?}"
        );
    }

    #[tokio::test]
    async fn oversized_lines_are_reported_not_parsed() {
        let mut h = TestDriver::new(MeterLimit::None);
        let adapter = ScriptAdapter::new(&format!(
            "head -c {} /dev/zero | tr '\\0' 'a'; echo; echo '{REPORT_RESULT}'",
            MAX_LINE_BYTES + 10
        ));
        let outcome = h.run(&adapter).await;
        assert!(outcome.succeeded(), "{outcome:?}");
        let events = h.events();
        let raw = events
            .iter()
            .find_map(|e| match &e.kind {
                RunEventKind::Agent(AgentEvent::Unknown { raw }) => Some(raw.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(raw["bytes"], MAX_LINE_BYTES + 10);
        assert_eq!(raw["start"].as_str().unwrap().len(), OVERSIZED_PREFIX);
        assert!(h.notes()[0].contains("longer than 8 MiB"));
    }

    #[tokio::test]
    async fn end_of_turn_usage_is_announced() {
        let mut h = TestDriver::new(MeterLimit::Tokens(1_000));
        let adapter = ScriptAdapter::new("true").with_capabilities(Capabilities {
            live_usage: false,
            ..Capabilities::default()
        });
        h.run(&adapter).await;
        assert_eq!(
            h.notes(),
            [
                "Fake Agent reports usage only when its turn ends, so spend is checked then. The time and tool-call limits apply while it works."
            ]
        );
    }

    #[tokio::test]
    async fn user_stop_uses_the_stop_plan() {
        let mut h = TestDriver::new(MeterLimit::None);
        let adapter = ScriptAdapter::new("sleep 30");
        let stop = h.stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            stop.stop();
        });
        let outcome = h.run(&adapter).await;
        assert_eq!(outcome.stopped_by, Some(StopCause::User));
        assert_eq!(outcome.failure().unwrap(), "stopped by you");
    }

    #[tokio::test]
    async fn session_timeout_stops_the_agent() {
        let mut h = TestDriver::new(MeterLimit::None);
        h.settings.session_timeout = Some(Duration::from_millis(200));
        let outcome = h.run(&ScriptAdapter::new("sleep 30")).await;
        assert_eq!(outcome.stopped_by, Some(StopCause::Timeout));
    }

    #[tokio::test]
    async fn leftover_processes_holding_output_do_not_hang_the_session() {
        let mut h = TestDriver::new(MeterLimit::None);
        let pid_file = h.dir.path().join("bg.pid");
        let adapter = ScriptAdapter::new(&format!(
            "sleep 30 &
             echo $! > '{}'
             echo '{REPORT_RESULT}'",
            pid_file.display()
        ));
        let started = std::time::Instant::now();
        let outcome = h.run(&adapter).await;
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(outcome.succeeded(), "{outcome:?}");
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        crate::testing::wait_until_gone(pid).await;
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_not_raised() {
        let mut h = TestDriver::new(MeterLimit::None);
        let adapter = ScriptAdapter::new("true").with_program("argonauts-no-such-agent");
        let outcome = h.run(&adapter).await;
        assert!(!outcome.succeeded());
        assert!(
            outcome
                .failure()
                .unwrap()
                .contains("argonauts-no-such-agent")
        );
        let kinds: Vec<String> = events_of(&h.events());
        assert_eq!(kinds, ["session_started", "agent:error", "session_ended"]);
    }

    #[tokio::test]
    async fn nonzero_exit_explains_itself() {
        let mut h = TestDriver::new(MeterLimit::None);
        let adapter = ScriptAdapter::new("echo 'fatal: not logged in' >&2; exit 3");
        let outcome = h.run(&adapter).await;
        assert_eq!(outcome.exit_code, Some(3));
        assert_eq!(
            outcome.failure().unwrap(),
            "the agent exited with code 3: fatal: not logged in"
        );
    }

    #[test]
    fn reports_come_from_the_best_source() {
        let report =
            r#"{"status":"blocked","summary":"s","files_changed":[],"tests_run":[],"concerns":[]}"#;
        let mut outcome = SessionOutcome {
            streamed_text: format!("thinking... {report}"),
            ..SessionOutcome::default()
        };
        assert_eq!(outcome.report().unwrap().status, ReportStatus::Blocked);
        outcome.last_message = Some(report.replace("blocked", "failed"));
        assert_eq!(outcome.report().unwrap().status, ReportStatus::Failed);
        outcome.result = Some(FinalResult {
            text: None,
            structured: Some(serde_json::from_str(&report.replace("blocked", "done")).unwrap()),
            is_error: false,
        });
        assert_eq!(outcome.report().unwrap().status, ReportStatus::Done);
    }

    #[test]
    fn stop_plans_always_end_in_kill() {
        let plan = StopPlan {
            steps: vec![StopStep {
                signal: StopSignal::Interrupt,
                wait: Duration::from_secs(1),
            }],
        };
        let steps = complete_plan(plan);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].signal, StopSignal::Kill);
        assert_eq!(complete_plan(StopPlan { steps: vec![] }).len(), 1);
        assert_eq!(complete_plan(StopPlan::default()).len(), 3);
    }

    #[test]
    fn streamed_text_is_capped_on_char_boundaries() {
        let mut state = SessionState::default();
        for _ in 0..(STREAMED_TEXT_CAP / 2) {
            state.push_streamed("é€");
        }
        assert!(state.streamed.len() <= STREAMED_TEXT_CAP * 2);
        assert!(state.streamed.len() >= STREAMED_TEXT_CAP);
    }
}
