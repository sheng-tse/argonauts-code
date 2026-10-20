//! Everything the runner does with an agent event, whoever parsed it: send it on, fill the
//! context size in, pass plan usage and health to the boards, diff changed files, feed the meter
//! and the watchdogs, and remember what the end needs. What to do about a limit comes back as
//! [`Action`]s, since steering is different between line sessions and driven ones. The boards and
//! the diffs work on tasks of their own, so nothing here waits on a file lock or on git.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use argonauts_core::answer_format;
use argonauts_core::events::AgentStatusEvent;
use argonauts_core::events::context::{ContextDetail, ContextUsage};
use argonauts_core::events::session::{LimitEvent, SessionEvent, Watchdog, loaded_words};
use argonauts_core::layout::session_files;
use argonauts_core::limits::{HealthSignal, PlanUsageStatus};
use argonauts_core::paths::write_private_file;
use argonauts_core::session_runner::SessionLimits;
use argonauts_core::settings::approvals::IsolatedLeak;
use argonauts_core::settings::fallback::WrapUp;
use argonauts_core::settings::limit::OnHit;
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
use argonauts_core::steps::is_report_tool;
use argonauts_core::{AgentEvent, ErrorCategory, LimitDimension, Spend, SpendStatus, TweakEffect};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::boards::Boards;
use super::context::Context;
use super::context_watch::ContextWatch;
use super::diffs::Diffs;
use super::end::{EndFacts, SeenError};
use super::handed_in::HandedIn;
use super::outbox::Outbox;
use super::stop::StopCause;
use crate::cli_versions::{CliVersions, event_kind, startup_lacks};
use crate::doctor::{self, versions::in_tested_range};
use crate::filediff::{FileDiffs, changed_paths};
use crate::meter::{self, MeterSetup, MeterVerdict, SessionMeter};
use crate::watchdogs::{WatchdogVerdict, Watchdogs};

/// Limit readings go out at most this often.
const LIMIT_EVERY: Duration = Duration::from_secs(1);

/// Unknown events past this many, and more than half of all events, mean the adapter no longer
/// understands the CLI.
const UNKNOWN_PILE_UP: u32 = 25;

const STDERR_TAIL_LINES: usize = 5;
/// A CLI's start-up message is among its first lines of output, so only these are looked at.
const STARTUP_WITHIN: u8 = 40;
const STDERR_LINE_CHARS: usize = 300;

/// What a limit or watchdog asks the session to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send the fixed wrap-up message. The dimension is set when the meter asked.
    WrapUp(Option<LimitDimension>),
    /// Send the repeat note.
    Nudge {
        call: String,
        times: u32,
    },
    /// Send the wrap-up for a later try whose calls changed nothing, which asks it to run the
    /// checks once and report.
    CheckAndReport,
    Stop(StopCause),
    /// Ask the CLI for its context by category.
    AskContext(ContextDetail),
}

/// What the end needs to know.
#[derive(Debug, Default)]
pub struct Facts {
    pub cli_session: Option<String>,
    pub result_text: Option<String>,
    pub structured: Option<Value>,
    pub result_is_error: Option<bool>,
    /// Results of the current process, one per finished turn.
    pub results: u32,
    pub last_message: Option<String>,
    pub fatal_error: Option<SeenError>,
    pub last_error: Option<SeenError>,
    pub plan_usage_rejected: Option<Option<OffsetDateTime>>,
    /// Tool calls started and not finished.
    pub open_tools: BTreeSet<String>,
    /// The model the session asked for, else the one its CLI reported.
    pub model: Option<String>,
    stderr_tail: VecDeque<String>,
    /// What the session's last report call sent, which a missed answer is mended from.
    last_report: Option<Value>,
}

/// How the line saying a report was mended ends, after `<CLI>'s report`.
const MENDED: &str = "was mended: fields that had run on inside its answer are fields again.";
const MENDED_AFTER_MISS: &str =
    "was mended from its last try: its fields had run on inside its answer.";

impl Facts {
    /// The CLI gave up on the answer format.
    fn missed_the_answer(&self) -> bool {
        [&self.fatal_error, &self.last_error]
            .into_iter()
            .flatten()
            .any(|error| error.category == ErrorCategory::AnswerMissed)
    }

    pub fn end_facts(&self) -> EndFacts {
        let tail: Vec<&str> = self.stderr_tail.iter().map(String::as_str).collect();
        EndFacts {
            result_is_error: self.result_is_error,
            result_text: self.result_text.clone(),
            fatal_error: self.fatal_error.clone(),
            last_error: self.last_error.clone(),
            plan_usage_rejected: self.plan_usage_rejected,
            stderr_tail: (!tail.is_empty()).then(|| tail.join(" / ")),
            model: self.model.clone(),
            ..EndFacts::default()
        }
    }
}

pub struct Observer {
    ctx: Arc<Context>,
    out: Outbox,
    meter: SessionMeter,
    watchdogs: Watchdogs,
    diffs: Diffs,
    boards: Boards,
    context_window: Option<u64>,
    /// `sessions.context` in tokens, when it lowers the window.
    context_cap: Option<u64>,
    pub facts: Facts,
    started: Instant,
    limit_sent: Option<(SpendStatus, Instant)>,
    stalled: bool,
    events: u32,
    unknown: u32,
    /// Events the adapter understood, leaving out the errors and results it makes up itself
    /// when a stream ends badly.
    understood: u32,
    broken_reported: bool,
    /// The CLI's version, when its first line says it.
    cli_version: Option<String>,
    /// Kinds of events the adapter didn't read, with counts, kept per version for `/doctor`.
    unread: BTreeMap<String, u64>,
    /// What this session's start-up message lacked of the fields the adapter reads, once it
    /// came. Kept per version beside the unread kinds.
    startup_lacked: Option<Vec<&'static str>>,
    /// Output lines looked at for the start-up message so far.
    startup_lines: u8,
    wrap_up_stopped: bool,
    plan_limit_stopped: bool,
    context_watch: ContextWatch,
    handed_in: HandedIn,
    /// The parser's figure for the requests since the agent's last totals, as of the line
    /// being read.
    billed: Option<f64>,
    /// The session's first context breakdown came, which says what its first request held.
    opening_weighed: bool,
}

impl Observer {
    pub fn new(ctx: Arc<Context>, out: Outbox, started: Instant) -> Self {
        let spec = &ctx.spec;
        let sessions = &ctx.settings.sessions;
        let context_cap = sessions.context.cap().unwrap_or_else(|err| {
            out.notice(format!("sessions.{err}, so the largest window is used"));
            None
        });
        // The window the model is held to, held in turn to `sessions.context`.
        let context_window = sessions.context.window(
            ctx.registry
                .as_ref()
                .and_then(|registry| registry.get(&spec.model))
                .and_then(|entry| entry.session_window()),
        );
        let rotate_at = match sessions.rotate_at.tokens(context_window) {
            Ok(_) => Some(sessions.rotate_at.clone()),
            Err(err) => {
                out.notice(format!("sessions.{err}, so the session won't rotate"));
                None
            }
        };
        let mut meter = SessionMeter::new(MeterSetup {
            limits: spec.limits.clone(),
            pricing: meter::pricing(
                &spec.model,
                &ctx.settings,
                ctx.registry.as_ref(),
                ctx.adapter.model_base_url().as_deref(),
            ),
            capabilities: ctx.adapter.capabilities(),
            rotate_at,
            context_window,
            context_cap,
        });
        meter.resumed_from(meter::spent_before(spec, ctx.adapter.id()));
        let watchdogs =
            Watchdogs::new(&spec.limits, fanout_cap(&ctx)).with_heartbeat(ctx.adapter.heartbeat());
        let diffs = Diffs::start(FileDiffs::new(spec.workdir.clone()), out.clone());
        if counts_no_progress(&spec.limits) {
            diffs.read_worktree();
        }
        let boards = Boards::start(ctx.clone(), out.clone());
        let asked_for = spec.model.model.clone().or_else(|| {
            let agent = ctx.settings.agents.get(&spec.model.agent);
            agent.and_then(|agent| agent.model.clone())
        });
        let context_watch = match spec.run {
            Some(_) => ContextWatch::of_crew(),
            None => ContextWatch::default(),
        };
        Self {
            ctx,
            out,
            meter,
            watchdogs,
            diffs,
            boards,
            context_window,
            context_cap,
            facts: Facts {
                model: asked_for,
                ..Facts::default()
            },
            started,
            limit_sent: None,
            stalled: false,
            events: 0,
            unknown: 0,
            understood: 0,
            broken_reported: false,
            cli_version: None,
            unread: BTreeMap::new(),
            startup_lacked: None,
            startup_lines: 0,
            wrap_up_stopped: false,
            plan_limit_stopped: false,
            context_watch,
            handed_in: HandedIn::default(),
            billed: None,
            opening_weighed: false,
        }
    }

    /// What the parser made of the requests since the agent's last totals, after the line
    /// whose events come next.
    pub fn billed_since_totals(&mut self, usd: Option<f64>) {
        self.billed = usd;
    }

    /// What the process said it spent at least, once it's gone and no totals can follow. Taken
    /// any sooner, it would stop the session before a result that brings the exact figure.
    pub fn spent_at_least(&mut self, usd: Option<f64>) {
        // The process is gone, so the verdict has nothing left to stop.
        let _ = self.meter.spent_at_least(usd);
    }

    /// Something started or stopped showing this session's context.
    pub fn watch_context(&mut self, detail: Option<ContextDetail>) {
        self.context_watch.set(detail);
    }

    /// The breakdown was asked for outright.
    /// The session's last count, whole, in its folder, replaced whole so a reader never finds
    /// half of one.
    fn keep_whole_count(&self, usage: &ContextUsage) {
        let dir = &self.ctx.spec.session_dir;
        let path = dir.join(session_files::CONTEXT);
        let partial = dir.join(format!("{}.partial", session_files::CONTEXT));
        let kept = serde_json::to_vec(usage)
            .map_err(std::io::Error::other)
            .and_then(|json| write_private_file(&partial, &json))
            .and_then(|()| std::fs::rename(&partial, &path));
        if let Err(err) = kept {
            self.out.notice(format!(
                "could not keep the session's count in {}: {err}",
                path.display()
            ));
        }
    }

    pub fn asked_context(&mut self) {
        self.context_watch.asked(Instant::now());
    }

    pub fn spend(&self) -> Spend {
        self.meter.spend()
    }

    pub fn tool_calls(&self) -> u64 {
        self.meter.tool_calls()
    }

    /// Emits `Session(AgentSession)` the first time an id is known, and again if it changes.
    pub fn cli_session(&mut self, id: String) {
        if id.is_empty() || self.facts.cli_session.as_deref() == Some(id.as_str()) {
            return;
        }
        self.facts.cli_session = Some(id.clone());
        self.out.session(SessionEvent::CliSession {
            id,
            transcript: None,
        });
    }

    /// Any output at all counts as activity, and ends a silence.
    pub fn output(&mut self) {
        self.watchdogs.output(Instant::now());
        self.stalled = false;
    }

    pub fn stderr(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if self.facts.stderr_tail.len() == STDERR_TAIL_LINES {
            self.facts.stderr_tail.pop_front();
        }
        self.facts
            .stderr_tail
            .push_back(line.chars().take(STDERR_LINE_CHARS).collect());
    }

    /// The same CLI session goes on in a new process: only the difference in its totals
    /// counts from here, and the new turn's result and errors start fresh.
    pub fn resumed(&mut self) {
        self.meter.resumed();
        let facts = &mut self.facts;
        facts.results = 0;
        facts.result_is_error = None;
        facts.fatal_error = None;
        facts.last_error = None;
        facts.open_tools.clear();
    }

    pub fn event(&mut self, mut event: AgentEvent) -> Vec<Action> {
        if let AgentEvent::ContextBreakdown { usage } = &event
            && let Some(run) = &self.ctx.spec.run
            && !std::mem::replace(&mut self.opening_weighed, true)
            && let Some(note) =
                super::opening::heavy_opening(run, self.ctx.adapter.display_name(), usage)
        {
            self.out.notice(note);
        }
        // A run's journal keeps how full its agent's session is and with what. The whole count,
        // with the grid and the lists, goes beside it for the session's own view.
        if let AgentEvent::ContextBreakdown { usage } = &mut event
            && self.ctx.spec.run.is_some()
        {
            self.keep_whole_count(usage);
            **usage = usage.summary();
        }
        if let AgentEvent::ContextFill { size, .. } = &mut event {
            // The window the session is held to: what its CLI reports, else the registry's,
            // and never more than `sessions.context` allows.
            *size = match (size.or(self.context_window), self.context_cap) {
                (Some(window), Some(cap)) => Some(window.min(cap)),
                (window, _) => window,
            };
        }
        self.events += 1;
        if !matches!(
            event,
            AgentEvent::Unknown { .. } | AgentEvent::Error { .. } | AgentEvent::Result { .. }
        ) {
            self.understood += 1;
        }
        // The earlier sessions' rows show what they spent, so a resumed one shows only its own.
        self.out.agent(self.meter.without_earlier_turns(&event));
        self.note(&event);
        for path in changed_paths(&event) {
            self.diffs.changed(path);
        }
        let mut actions: Vec<Action> = self.isolation(&event).into_iter().collect();
        actions.extend(self.report_handed_in(&event));
        if matches!(event, AgentEvent::ContextFill { .. })
            && let Some(detail) = self.context_watch.after_fill(Instant::now())
        {
            actions.push(Action::AskContext(detail));
        }
        actions.extend(self.wrap_up_stop(&event));
        actions.extend(self.plan_limit_stop(&event));
        let verdict = self.meter.observe(&event);
        actions.extend(meter_action(verdict));
        if matches!(event, AgentEvent::Usage { .. }) {
            // Before the reading goes out, so even the first one prices the request as billed.
            let verdict = self.meter.bill_since_totals(self.billed);
            actions.extend(meter_action(verdict));
            self.record_spend();
        }
        self.watch_the_worktree(&event);
        let verdict = self.watchdogs.observe(&event, Instant::now());
        actions.extend(self.watchdog_action(verdict));
        self.limit(false);
        actions
    }

    /// For a later try whose calls are counted while they change nothing, the worktree is read
    /// after each of the main agent's calls ends, and a change found resets the count before
    /// the next call counts, whatever made it.
    fn watch_the_worktree(&mut self, event: &AgentEvent) {
        if !counts_no_progress(&self.ctx.spec.limits) {
            return;
        }
        match event {
            AgentEvent::ToolFinished { parent: None, .. } => self.diffs.read_worktree(),
            AgentEvent::ToolStarted { parent: None, .. } if self.diffs.worktree_changed() => {
                self.watchdogs.progress_made();
            }
            _ => {}
        }
    }

    /// A call from the session to Argonauts Code opened (`open`) or was answered, as the
    /// watchdogs count it.
    pub fn asking(&mut self, open: bool) {
        self.watchdogs.asking(open, Instant::now());
    }

    /// The wall clock, inactivity, and stalls. `cpu_busy` is when the process tree last used
    /// CPU, from the shared sampler.
    pub fn tick(&mut self, cpu_busy: Option<Instant>) -> Vec<Action> {
        self.tick_at(Instant::now(), cpu_busy)
    }

    fn tick_at(&mut self, now: Instant, cpu_busy: Option<Instant>) -> Vec<Action> {
        let mut actions = Vec::new();
        let verdict = self.meter.tick(now.saturating_duration_since(self.started));
        actions.extend(meter_action(verdict));
        let verdict = self.watchdogs.tick(now, cpu_busy);
        actions.extend(self.watchdog_action(verdict));
        self.limit(false);
        actions
    }

    /// The diffs still owed, what the boards still have to take, and the latest limit
    /// reading, before the end.
    pub async fn finish(&mut self) {
        self.check_untested_adapter().await;
        self.keep_unread().await;
        self.diffs.flush(&self.out).await;
        if !self.boards.settle().await {
            self.out.notice(
                "the plan usage and health boards didn't take this session's last readings",
            );
        }
        self.record_spend();
        self.limit(true);
    }

    /// Keeps what the CLI session has spent so far, for a later session that resumes it.
    fn record_spend(&self) {
        let spec = &self.ctx.spec;
        // A fork that never said its own id leaves what the original spent as it was.
        let resumed = spec.resume.as_deref().filter(|_| !spec.fork);
        let Some(id) = self.facts.cli_session.as_deref().or(resumed) else {
            return;
        };
        meter::record_cli_session_spend(
            &spec.workdir,
            self.ctx.adapter.id(),
            id,
            self.meter.total_spend(),
        );
    }

    fn note(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init {
                session_id,
                model,
                version,
                ..
            } => {
                if let Some(id) = session_id {
                    self.cli_session(id.clone());
                }
                if self.facts.model.is_none() {
                    self.facts.model.clone_from(model);
                }
                if let Some(version) = version
                    && self.cli_version.is_none()
                {
                    self.cli_version = Some(version.clone());
                }
            }
            AgentEvent::Message { text, parent: None } => {
                self.facts.last_message = Some(text.clone());
            }
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => {
                self.facts.open_tools.insert(id.clone());
                if parent.is_none() && is_report_tool(name) {
                    self.facts.last_report = Some(input.clone());
                }
            }
            AgentEvent::ToolFinished { id, .. } => {
                self.facts.open_tools.remove(id);
            }
            AgentEvent::PlanUsage { reading } => {
                if reading.status == PlanUsageStatus::Rejected {
                    self.facts.plan_usage_rejected = Some(reading.resets_at);
                }
                self.boards.plan_usage(reading.clone());
                self.out
                    .agent_id(AgentStatusEvent::PlanUsage(reading.clone()));
            }
            AgentEvent::Plan { plan } => self.boards.plan(plan.clone()),
            AgentEvent::Retry { reason, .. } => self.signal(HealthSignal::Retry {
                agent_id: self.ctx.agent_id.clone(),
                model: self.facts.model.clone(),
                session: self.ctx.spec.key.clone(),
                reason: reason.clone(),
            }),
            AgentEvent::Error {
                category,
                message,
                retryable,
            } => {
                let seen = SeenError {
                    category: *category,
                    message: message.clone(),
                };
                if !retryable {
                    self.facts.fatal_error = Some(seen.clone());
                }
                self.facts.last_error = Some(seen);
                if about_the_agent(*category) {
                    self.signal(HealthSignal::Error {
                        agent_id: self.ctx.agent_id.clone(),
                        model: self.facts.model.clone(),
                        session: self.ctx.spec.key.clone(),
                        category: *category,
                    });
                }
            }
            AgentEvent::Result {
                text,
                structured,
                is_error,
            } => {
                let facts = &mut self.facts;
                facts.results += 1;
                facts.result_is_error = Some(*is_error);
                if text.is_some() {
                    facts.result_text = text.clone();
                }
                if structured.is_none()
                    && super::report_tool::reports_by_tool(&self.ctx.spec)
                    && let Some(report) = super::report_tool::read(&self.ctx.spec)
                {
                    facts.structured = Some(report);
                }
                let format = self.ctx.spec.json_schema.as_ref();
                let mut mended = None;
                if let Some(answer) = structured {
                    let answer = match format {
                        Some(format) => answer_format::as_written(format, answer.clone()),
                        None => answer.clone(),
                    };
                    let fixed =
                        format.and_then(|format| answer_format::mended_to_fit(format, &answer));
                    mended = fixed.is_some().then_some(MENDED);
                    facts.structured = Some(fixed.unwrap_or(answer));
                } else if *is_error
                    && facts.missed_the_answer()
                    && let Some(format) = format
                    && let Some(last) = facts.last_report.take()
                    && let Some(report) = answer_format::mended_to_fit(
                        format,
                        &answer_format::as_written(format, last),
                    )
                {
                    // The fields were all there, run on inside one string: the session ends
                    // done with them.
                    facts.structured = Some(report);
                    facts.result_is_error = Some(false);
                    facts.fatal_error = None;
                    facts.last_error = None;
                    mended = Some(MENDED_AFTER_MISS);
                }
                if let Some(words) = mended {
                    self.out.notice(format!(
                        "{}'s report {words}",
                        self.ctx.adapter.display_name()
                    ));
                }
                if !is_error {
                    self.signal(HealthSignal::Success {
                        agent_id: self.ctx.agent_id.clone(),
                        model: self.facts.model.clone(),
                        session: self.ctx.spec.key.clone(),
                    });
                }
            }
            AgentEvent::Unknown { raw } => {
                self.unknown += 1;
                if let Some(kind) = event_kind(raw) {
                    *self.unread.entry(kind).or_default() += 1;
                }
                self.check_adapter();
            }
            _ => {}
        }
    }

    /// An isolated session that loaded your plugins or synced skills is stopped before its first
    /// tool call, unless `sandbox.isolated_leak` lets it run on with a notice. A built-in of a
    /// later version is the CLI's own, not yours: it's learned, so later sessions turn it off.
    fn isolation(&mut self, event: &AgentEvent) -> Option<Action> {
        let AgentEvent::IsolationLeak {
            plugins,
            skills,
            builtins,
        } = event
        else {
            return None;
        };
        if !self.ctx.spec.isolated {
            return None;
        }
        if !builtins.is_empty() {
            self.learn_builtins(builtins);
        }
        if plugins.is_empty() && skills.is_empty() {
            return None;
        }
        match self.ctx.rules.isolated_leak {
            IsolatedLeak::Stop => Some(Action::Stop(StopCause::IsolationBroken {
                plugins: plugins.clone(),
                skills: skills.clone(),
            })),
            IsolatedLeak::Warn => {
                self.out.notice(format!(
                    "{} loaded {}, which a blind role mustn't see. It runs on, since \
                     sandbox.isolated_leak is \"warn\".",
                    self.ctx.adapter.display_name(),
                    loaded_words(plugins, skills)
                ));
                None
            }
        }
    }

    fn learn_builtins(&mut self, names: &[String]) {
        let path = self.ctx.home.cli_versions();
        let mut file = CliVersions::load(&path);
        if !file.learn_builtins(self.ctx.adapter.id(), names) {
            return;
        }
        if let Err(err) = file.save(&path) {
            tracing::debug!("couldn't keep the built-ins learned: {err}");
        }
        self.out.notice(format!(
            "{} loaded built-in plugins Argonauts Code doesn't know yet ({}). This blind session keeps \
             them, and later ones turn them off.",
            self.ctx.adapter.display_name(),
            names.join(", ")
        ));
    }

    /// Tells the health board, which sends on any change of the agent's state it causes.
    fn signal(&mut self, signal: HealthSignal) {
        self.boards.health(signal);
    }

    fn check_adapter(&mut self) {
        let piled_up = self.unknown >= UNKNOWN_PILE_UP && self.unknown * 2 > self.events;
        if !piled_up || self.broken_reported {
            return;
        }
        self.report_broken(None);
    }

    /// A few unknown events are enough from a CLI version the adapter was never tested with,
    /// once they outnumber the understood ones. The version is read only then, so a healthy
    /// session never runs the CLI an extra time.
    async fn check_untested_adapter(&mut self) {
        if self.broken_reported || self.unknown == 0 || self.unknown <= self.understood {
            return;
        }
        let adapter = self.ctx.adapter.clone();
        let binary = self
            .ctx
            .settings
            .agents
            .get(adapter.id())
            .and_then(|agent| agent.binary.clone());
        let Some(version) = doctor::installed_version(adapter.as_ref(), binary.as_deref()).await
        else {
            return;
        };
        if !in_tested_range(&version, adapter.tested_versions()) {
            self.report_broken(Some(&version));
        }
    }

    /// A line of the CLI's output, as it came. Until the start-up message has been seen it is
    /// looked for among the first lines, and what it lacks of the fields the adapter reads is
    /// kept for Doctor. Nothing is said now: a session never speaks of versions.
    pub fn stdout_line(&mut self, line: &str) {
        if self.startup_lacked.is_some() || self.startup_lines >= STARTUP_WITHIN {
            return;
        }
        self.startup_lines += 1;
        if let Some(lacked) = startup_lacks(self.ctx.adapter.as_ref(), line) {
            self.startup_lacked = Some(lacked.iter().map(|field| field.pointer).collect());
        }
    }

    /// The kinds of events this session's CLI sent that the adapter didn't read, and what its
    /// start-up message lacked, added to what `/doctor` lists for its version. Only kinds,
    /// counts and field names, never content.
    async fn keep_unread(&mut self) {
        if self.unread.is_empty() && self.startup_lacked.is_none() {
            return;
        }
        let version = match self.cli_version.clone() {
            Some(version) => version,
            None => {
                let adapter = self.ctx.adapter.clone();
                let binary = self
                    .ctx
                    .settings
                    .agents
                    .get(adapter.id())
                    .and_then(|agent| agent.binary.clone());
                let Some(version) =
                    doctor::installed_version(adapter.as_ref(), binary.as_deref()).await
                else {
                    return;
                };
                version
            }
        };
        let path = self.ctx.home.cli_versions();
        let mut file = CliVersions::load(&path);
        let cli = self.ctx.adapter.id();
        let unread = std::mem::take(&mut self.unread);
        let mut changed = !unread.is_empty();
        file.add_unread(cli, &version, &unread);
        if let Some(lacked) = self.startup_lacked.take() {
            changed |= file.saw_startup(cli, &version, &lacked);
        }
        if !changed {
            return;
        }
        if let Err(err) = file.save(&path) {
            tracing::debug!("couldn't keep the unread event kinds: {err}");
        }
    }

    /// The session's work is done once a tool call hands its report in, on a CLI that can't
    /// hold the session to the format, unless it reports through Argonauts Code's own tool,
    /// which holds a report sent too early.
    fn report_handed_in(&mut self, event: &AgentEvent) -> Option<Action> {
        let schema = self.ctx.spec.json_schema.as_ref()?;
        if self.ctx.adapter.holds_answer_format()
            || self.facts.structured.is_some()
            || super::report_tool::reports_by_tool(&self.ctx.spec)
        {
            return None;
        }
        let report = self.handed_in.report(event, schema)?;
        self.facts.structured = Some(report);
        Some(Action::Stop(StopCause::Reported))
    }

    /// With `fallback.wrap_up = "fallback"`, the first sign of the wrap-up allowance ends the
    /// turn, so the allowance is saved and the work moves on at once.
    fn wrap_up_stop(&mut self, event: &AgentEvent) -> Option<Action> {
        let AgentEvent::PlanUsage { reading } = event else {
            return None;
        };
        // A role's own table has the top-level value unless it sets one.
        let fallback = &self.ctx.settings.fallback;
        let wanted = fallback
            .for_role(self.ctx.spec.role)
            .map_or(fallback.wrap_up, |role| role.wrap_up);
        if !reading.wrap_up
            || wanted != WrapUp::Fallback
            || std::mem::replace(&mut self.wrap_up_stopped, true)
        {
            return None;
        }
        Some(Action::Stop(StopCause::WrapUp {
            resets_at: reading.resets_at,
        }))
    }

    /// A plan window as full as your `limits.plan_usage` ends the turn once. `when_reached`
    /// says how: as a plan that ran out, so the task falls back, or as a reached limit,
    /// so the task stops.
    fn plan_limit_stop(&mut self, event: &AgentEvent) -> Option<Action> {
        let AgentEvent::PlanUsage { reading } = event else {
            return None;
        };
        let limits = &self.ctx.settings.limits.spend;
        let limit = limits.plan_usage?;
        if reading.used.is_none_or(|used| used < limit)
            || std::mem::replace(&mut self.plan_limit_stopped, true)
        {
            return None;
        }
        Some(Action::Stop(match limits.when_reached {
            OnHit::Fallback => StopCause::PlanLimit {
                resets_at: reading.resets_at,
            },
            OnHit::Stop => StopCause::Limit(LimitDimension::Spend),
        }))
    }

    fn report_broken(&mut self, version: Option<&str>) {
        self.broken_reported = true;
        let adapter = &self.ctx.adapter;
        let cli = match version {
            Some(version) => format!("{} {version}", adapter.display_name()),
            None => adapter.display_name().to_owned(),
        };
        let detail = format!(
            "{} of {} events from {cli} weren't understood, so its output format may have \
             changed. Pin a tested version: {}",
            self.unknown,
            self.events,
            adapter.tested_versions().join(", ")
        );
        self.signal(HealthSignal::AdapterBroken {
            agent_id: self.ctx.agent_id.clone(),
            detail,
        });
    }

    fn watchdog_action(&mut self, verdict: WatchdogVerdict) -> Option<Action> {
        match verdict {
            WatchdogVerdict::Quiet => {
                self.stalled = false;
                None
            }
            WatchdogVerdict::Stalled { silent_for } => {
                self.stalled_once(|cli| {
                    format!(
                        "{cli} has sent nothing for {} seconds, not even its heartbeat",
                        silent_for.as_secs()
                    )
                });
                None
            }
            WatchdogVerdict::Quieting {
                quiet_for,
                stops_after,
            } => {
                self.stalled_once(|cli| {
                    format!(
                        "{cli} has been quiet for {} and stops after {} of quiet",
                        spelled(quiet_for),
                        spelled(stops_after)
                    )
                });
                None
            }
            WatchdogVerdict::Nudge { call, times } => Some(Action::Nudge { call, times }),
            WatchdogVerdict::SoftStop(Watchdog::NoProgress) => Some(Action::CheckAndReport),
            WatchdogVerdict::SoftStop(_) => Some(Action::WrapUp(None)),
            WatchdogVerdict::Stuck { watchdog, calls } => {
                Some(Action::Stop(StopCause::Stuck { watchdog, calls }))
            }
            WatchdogVerdict::FanoutExceeded { running, max } => {
                Some(Action::Stop(StopCause::Fanout { running, max }))
            }
        }
    }

    /// One note per silence, whichever watchdog sees it first.
    fn stalled_once(&mut self, note: impl FnOnce(&str) -> String) {
        if !self.stalled {
            self.stalled = true;
            self.out.stalled(note(self.ctx.adapter.display_name()));
        }
    }

    /// `Limit(Limit)` when the reading changed, at most once a second unless `now`.
    fn limit(&mut self, now: bool) {
        let status = self.meter.status();
        let at = Instant::now();
        match self.limit_sent {
            Some((sent, _)) if sent == status => return,
            Some((_, sent_at)) if !now && at.duration_since(sent_at) < LIMIT_EVERY => return,
            _ => {}
        }
        self.limit_sent = Some((status, at));
        self.out
            .session(SessionEvent::Limit(LimitEvent::Spend(status)));
    }
}

fn meter_action(verdict: MeterVerdict) -> Option<Action> {
    match verdict {
        MeterVerdict::Continue => None,
        MeterVerdict::WrapUp(dimension) => Some(Action::WrapUp(Some(dimension))),
        MeterVerdict::Stop(dimension) => Some(Action::Stop(StopCause::Limit(dimension))),
        MeterVerdict::Rotate { used, cap } => Some(Action::Stop(StopCause::Rotate { used, cap })),
    }
}

/// Errors that say something about the agent rather than this session's request.
fn about_the_agent(category: ErrorCategory) -> bool {
    !matches!(
        category,
        ErrorCategory::ContextFull
            | ErrorCategory::SessionLost
            | ErrorCategory::InvalidRequest
            | ErrorCategory::Permission
            | ErrorCategory::Limit
            | ErrorCategory::Interrupted
    )
}

/// The subagent cap the watchdogs count against, only where the adapter leaves it to them.
fn fanout_cap(ctx: &Context) -> Option<u32> {
    let setting = ctx.spec.native_fanout;
    let tweak = ctx.adapter.native_fanout(setting);
    if tweak.effect != TweakEffect::EnforcedByConductor {
        return None;
    }
    match setting {
        NativeFanout::Limit { max } => Some(max),
        NativeFanout::Switch(FanoutSwitch::Off) => Some(0),
        NativeFanout::Switch(FanoutSwitch::On) => None,
    }
}

/// `7 minutes`, or under a minute `30 seconds`.
fn spelled(duration: Duration) -> String {
    let secs = duration.as_secs();
    let (count, unit) = match secs / 60 {
        0 => (secs, "second"),
        minutes => (minutes, "minute"),
    };
    let s = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{s}")
}

/// A line longer than the reader keeps can't be parsed; it's reported instead.
pub fn oversized_line(text: &str, dropped: usize) -> AgentEvent {
    let start: String = text.chars().take(200).collect();
    AgentEvent::Unknown {
        raw: json!({
            "oversized_line": {
                "kept_bytes": text.len(),
                "dropped_bytes": dropped,
                "start": start,
            }
        }),
    }
}

/// Whether the session's limits count a later try's calls that change nothing.
fn counts_no_progress(limits: &SessionLimits) -> bool {
    limits.no_progress_calls.is_some_and(|calls| calls > 0)
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::session::{EndReason, Watchdog};
    use argonauts_core::session_runner::{
        RunnerEvent, SessionHandle, SessionLimits, SessionRunner, SessionSpec,
    };
    use argonauts_core::settings::limit::{LimitAmount, LimitUnit};

    use super::*;
    use crate::cli_runner::capabilities::CLAUDE_CODE;
    use crate::cli_runner::outbox;
    use crate::cli_runner::testing::{
        TestAdapter, TestDriver, agent_events, collect, event_line, session_events, spec_in,
        unique_key,
    };
    use crate::health::is_agent_trouble;

    const MINUTE: Duration = Duration::from_secs(60);

    /// An observer of one session, with what it needs kept alive.
    struct Watched {
        obs: Observer,
        handle: SessionHandle,
        started: Instant,
        _cli: TestDriver,
        _dir: tempfile::TempDir,
    }

    fn watched(adapter: TestAdapter, limits: SessionLimits) -> Watched {
        let driver = TestDriver::new(adapter);
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.limits = limits;
        let (handle, out, _control) = outbox::open(spec.key.clone());
        let home = driver.worker.home().unwrap();
        let ctx = Arc::new(Context::new(&driver.worker, spec, home));
        let started = Instant::now();
        Watched {
            obs: Observer::new(ctx, out, started),
            handle,
            started,
            _cli: driver,
            _dir: dir,
        }
    }

    /// A later try that edits with its shell, as `sed -i` does, changes the worktree with calls
    /// no tool name tells apart from looking around, so only reading the worktree shows it.
    #[tokio::test]
    async fn a_later_try_that_edits_with_its_shell_is_never_wrapped_up() {
        for edits in [true, false] {
            let repo = crate::testutil::TestRepo::new();
            let driver = TestDriver::new(TestAdapter::new("exec sleep 30"));
            let mut spec = spec_in(&repo.root, &repo.root, "hi");
            spec.limits = SessionLimits {
                no_progress_calls: Some(3),
                ..SessionLimits::default()
            };
            let (_handle, out, _control) = outbox::open(spec.key.clone());
            let home = driver.worker.home().unwrap();
            let ctx = Arc::new(Context::new(&driver.worker, spec, home));
            let mut obs = Observer::new(ctx, out, Instant::now());
            let mut actions = Vec::new();
            for n in 0..6 {
                let id = format!("c{n}");
                let command = format!("sed -i '' 's/Test/Test {n}/' README.md");
                actions.extend(obs.event(AgentEvent::ToolStarted {
                    id: id.clone(),
                    name: "shell".into(),
                    input: serde_json::json!({ "command": command }),
                    parent: None,
                }));
                if edits {
                    repo.write("README.md", &format!("# Test {n}\n"));
                }
                actions.extend(obs.event(AgentEvent::ToolFinished {
                    id,
                    ok: true,
                    output: Some(String::new()),
                    parent: None,
                }));
                // The read runs in the background, as the next call would follow it.
                obs.diffs.flush(&obs.out).await;
            }
            let wrapped = actions.contains(&Action::CheckAndReport);
            assert_eq!(wrapped, !edits, "edits: {edits}, {actions:?}");
        }
    }

    impl Watched {
        fn at(&self, after: Duration) -> Instant {
            self.started + after
        }

        /// The stall notes sent so far.
        async fn stalls(&mut self) -> Vec<String> {
            let mut notes = Vec::new();
            let wait = Duration::from_millis(100);
            while let Ok(Some(event)) = tokio::time::timeout(wait, self.handle.events.recv()).await
            {
                if let RunnerEvent::Agent(AgentEvent::Error {
                    category: ErrorCategory::Stalled,
                    message,
                    ..
                }) = event
                {
                    notes.push(message);
                }
            }
            notes
        }
    }

    /// An observer of a build session, which answers in the report's format.
    fn reporting() -> Watched {
        let mut w = watched(TestAdapter::new("true"), SessionLimits::default());
        let mut spec = w.obs.ctx.spec.clone();
        spec.json_schema = Some(argonauts_core::steps::output_schema::<
            argonauts_core::steps::task::Build,
        >());
        let home = w._cli.worker.home().unwrap();
        let (handle, out, _control) = outbox::open(spec.key.clone());
        let ctx = Arc::new(Context::new(&w._cli.worker, spec, home));
        w.obs = Observer::new(ctx, out, w.started);
        w.handle = handle;
        w
    }

    /// A report whose required lists ran on inside its answer.
    fn leaked_report() -> Value {
        json!({
            "status": "done",
            "summary": "Due dates in the tasks CLI.",
            "tests_run": ["node --test: 11 pass"],
            "answer": "Done.</answer>\n<parameter name=\"concerns\">[\"Assumed: local dates.\"]\
                       </parameter>\n<parameter name=\"files_changed\">[\"src/cli.js\"]"
        })
    }

    async fn notices(handle: &mut SessionHandle) -> Vec<String> {
        let mut notes = Vec::new();
        let wait = Duration::from_millis(100);
        while let Ok(Some(event)) = tokio::time::timeout(wait, handle.events.recv()).await {
            if let RunnerEvent::Agent(AgentEvent::Error {
                category: ErrorCategory::Unknown,
                message,
                ..
            }) = event
            {
                notes.push(message);
            }
        }
        notes
    }

    /// Claude Code gave up on the format, but its last try held every field, run on inside
    /// the answer: mended, it is the session's report, and the session ends done.
    #[tokio::test]
    async fn a_missed_report_whose_fields_ran_into_its_answer_ends_done_mended() {
        let mut w = reporting();
        w.obs.event(AgentEvent::ToolStarted {
            id: "so-5".into(),
            name: "StructuredOutput".into(),
            input: leaked_report(),
            parent: None,
        });
        w.obs.event(AgentEvent::Error {
            category: ErrorCategory::AnswerMissed,
            message: "Failed to provide valid structured output".into(),
            retryable: false,
        });
        w.obs.event(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        });
        let report = w.obs.facts.structured.clone().expect("the mended report");
        assert_eq!(report["files_changed"], json!(["src/cli.js"]));
        assert_eq!(report["concerns"], json!(["Assumed: local dates."]));
        assert_eq!(report["answer"], json!("Done."));
        assert_eq!(
            super::super::end::classify(&w.obs.facts.end_facts()),
            EndReason::Completed
        );
        let notes = notices(&mut w.handle).await;
        assert!(
            notes
                .iter()
                .any(|note| note.contains("report was mended from its last try")),
            "{notes:?}"
        );
    }

    /// An answer Claude Code accepted with a list run on inside it gets that list back.
    #[tokio::test]
    async fn an_accepted_report_with_a_list_run_into_its_answer_is_mended() {
        let mut w = reporting();
        let mut accepted = leaked_report();
        accepted["concerns"] = json!([]);
        accepted["files_changed"] = json!(["src/store.js"]);
        accepted["answer"] = json!("Done.</answer>\n<parameter name=\"change_requests\">[]");
        w.obs.event(AgentEvent::Result {
            text: None,
            structured: Some(accepted),
            is_error: false,
        });
        let report = w.obs.facts.structured.clone().expect("the report");
        assert_eq!(report["answer"], json!("Done."));
        assert_eq!(report["change_requests"], json!([]));
        assert_eq!(report["files_changed"], json!(["src/store.js"]));
    }

    fn cargo_test() -> AgentEvent {
        AgentEvent::ToolStarted {
            id: "t1".into(),
            name: "Bash".into(),
            input: json!({ "command": "cargo test" }),
            parent: None,
        }
    }

    #[tokio::test]
    async fn a_stall_past_the_heartbeat_is_noted_once_per_silence() {
        let adapter = TestAdapter::new("true").with_heartbeat(Duration::from_secs(30));
        let mut w = watched(adapter, SessionLimits::default());
        w.obs.event(cargo_test());
        let mut actions = Vec::new();
        for secs in [46, 50] {
            actions.extend(w.obs.tick_at(w.at(Duration::from_secs(secs)), None));
        }
        // A heartbeat ends the silence, so the next one is noted again.
        w.obs.output();
        actions.extend(w.obs.tick_at(w.at(Duration::from_secs(100)), None));
        assert!(actions.is_empty(), "nothing acts on a stall: {actions:?}");
        let stalls = w.stalls().await;
        assert_eq!(stalls.len(), 2, "{stalls:?}");
        for stall in &stalls {
            assert!(
                stall.starts_with("Fake Agent has sent nothing for "),
                "{stall}"
            );
        }

        // Without a heartbeat, silence during a tool call is never a stall.
        let mut w = watched(TestAdapter::new("true"), SessionLimits::default());
        w.obs.event(cargo_test());
        for minutes in 1..=10 {
            w.obs.tick_at(w.at(MINUTE * minutes), None);
        }
        assert_eq!(w.stalls().await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn half_the_quiet_limit_says_when_the_stop_comes_once() {
        let limits = SessionLimits {
            inactivity_min: Some(15.0),
            ..SessionLimits::default()
        };
        let mut w = watched(TestAdapter::new("true"), limits);
        w.obs.event(cargo_test());
        let second = Duration::from_secs(1);
        let mut actions = Vec::new();
        for after in [MINUTE * 7, MINUTE * 8 + second, MINUTE * 9] {
            actions.extend(w.obs.tick_at(w.at(after), None));
        }
        assert!(actions.is_empty(), "{actions:?}");
        assert_eq!(
            w.stalls().await,
            ["Fake Agent has been quiet for 8 minutes and stops after 15 minutes of quiet"]
        );
        let actions = w.obs.tick_at(w.at(MINUTE * 15 + second), None);
        assert_eq!(
            actions,
            [Action::Stop(StopCause::Stuck {
                watchdog: Watchdog::Inactivity,
                calls: None
            })]
        );
    }

    #[tokio::test]
    async fn minutes_run_on_across_a_resume() {
        let h = TestDriver::new(TestAdapter::new("true"));
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.limits.max_minutes = Some(60.0);
        let (_handle, out, _control) = outbox::open(spec.key.clone());
        let ctx = Arc::new(Context::new(&h.worker, spec, h.worker.home().unwrap()));
        let started = Instant::now();
        let mut obs = Observer::new(ctx, out, started);

        let actions = obs.tick_at(started + MINUTE * 51, None);
        assert!(
            actions.contains(&Action::WrapUp(Some(LimitDimension::Minutes))),
            "{actions:?}"
        );
        // The wrap-up went out through an interrupt and a resume in a new process.
        obs.resumed();
        let actions = obs.tick_at(started + MINUTE * 51 + Duration::from_secs(6), None);
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::Stop(_))),
            "{actions:?}"
        );
        assert!((obs.meter.status().work.minutes - 51.1).abs() < 1e-6);
    }

    #[tokio::test]
    async fn a_resumed_session_counts_only_what_it_adds() {
        let usage = |cost: f64| {
            event_line(&AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 10,
                cached_tokens: 0,
                cost_usd: Some(cost),
                cumulative: true,
            })
        };
        let init = event_line(&AgentEvent::Init {
            session_id: Some("cli-7".into()),
            model: None,
            tools: Vec::new(),
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        });
        let print = |line: &str| format!("printf '%s\\n' '{line}'");
        // The resumed run's totals include the first run's $0.60.
        let script = format!(
            "IFS= read -r first; if [ -z \"$2\" ]; then {}; {}; else {}; fi",
            print(&init),
            print(&usage(0.6)),
            print(&usage(1.0)),
        );
        let adapter = TestAdapter::new(&script)
            .with_id(CLAUDE_CODE)
            .with_input_lines();
        let h = TestDriver::new(adapter);
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().canonicalize().unwrap();
        let mut spec = spec_in(&work, &work.join("first"), "start");
        spec.limits.limit = Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount: 2.0,
        });
        let (_, first) = collect(h.worker.start(spec.clone()).await.unwrap()).await;
        assert_eq!(first.cli_session.as_deref(), Some("cli-7"));
        assert!((first.spend.cost_usd - 0.6).abs() < 1e-9);

        let resumed = SessionSpec {
            key: unique_key(),
            session_dir: work.join("second"),
            resume: Some("cli-7".into()),
            ..spec
        };
        let (events, end) = collect(h.worker.start(resumed).await.unwrap()).await;
        assert!((end.spend.cost_usd - 0.4).abs() < 1e-9, "{:?}", end.spend);
        // Its usage reaches you as its own, so the first session's $0.60 shows once.
        let reported = agent_events(&events)
            .into_iter()
            .find_map(|event| match event {
                AgentEvent::Usage {
                    cost_usd,
                    input_tokens,
                    ..
                } => Some((*cost_usd, *input_tokens)),
                _ => None,
            });
        let (cost, tokens) = reported.unwrap_or_default();
        assert!((cost.unwrap_or_default() - 0.4).abs() < 1e-9, "{cost:?}");
        assert_eq!(tokens, 0);
        assert!(!matches!(end.reason, EndReason::LimitHit { .. }));
        let cap = session_events(&events)
            .into_iter()
            .find_map(|event| match event {
                SessionEvent::Limit(LimitEvent::NativeCap { usd }) => Some(*usd),
                _ => None,
            });
        assert_eq!(cap, Some(1.6), "only what's left of $2.20");
    }

    #[test]
    fn every_error_the_breaker_counts_reaches_it() {
        let all = [
            ErrorCategory::Auth,
            ErrorCategory::UsageLimit,
            ErrorCategory::RateLimit,
            ErrorCategory::Billing,
            ErrorCategory::Overloaded,
            ErrorCategory::Server,
            ErrorCategory::Network,
            ErrorCategory::ContextFull,
            ErrorCategory::ModelUnavailable,
            ErrorCategory::InvalidRequest,
            ErrorCategory::Permission,
            ErrorCategory::Limit,
            ErrorCategory::Timeout,
            ErrorCategory::Interrupted,
            ErrorCategory::Cli,
            ErrorCategory::Unknown,
        ];
        for category in all {
            if is_agent_trouble(category) {
                assert!(about_the_agent(category), "{category:?}");
            }
        }
    }
}
