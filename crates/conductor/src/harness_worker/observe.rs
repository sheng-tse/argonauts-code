//! Everything the worker does with an agent event, whoever parsed it: send it on, fill the
//! context size in, pass quota and health to the boards, diff changed files, feed the meter and
//! the watchdogs, and remember what the end needs. What to do about a limit comes back as
//! [`Action`]s, since steering differs between line sessions and driven ones. The boards and the
//! diffs work on tasks of their own, so nothing here waits on a file lock or on git.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use argonauts_core::events::VendorEvent;
use argonauts_core::events::session::{LimitEvent, SessionEvent};
use argonauts_core::limits::{HealthSignal, QuotaStatus};
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
use argonauts_core::{
    AgentEvent, BudgetDimension, BudgetStatus, ErrorCategory, Spend, TweakEffect,
};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::boards::Boards;
use super::context::Context;
use super::diffs::Diffs;
use super::end::{EndFacts, SeenError};
use super::outbox::Outbox;
use super::stop::StopCause;
use crate::doctor::{self, versions::in_tested_range};
use crate::filediff::{FileDiffer, changed_paths};
use crate::meter::{self, MeterSetup, MeterVerdict, SessionMeter};
use crate::watchdogs::{WatchdogVerdict, Watchdogs};

/// Budget readings go out at most this often.
const BUDGET_EVERY: Duration = Duration::from_secs(1);

/// Unknown events past this many, and more than half of all events, mean the adapter no longer
/// understands the CLI.
const UNKNOWN_PILE_UP: u32 = 25;

const STDERR_TAIL_LINES: usize = 5;
const STDERR_LINE_CHARS: usize = 300;

/// What a limit or watchdog asks the session to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send the fixed wrap-up message. The dimension is set when the meter asked.
    WrapUp(Option<BudgetDimension>),
    /// Send the repeat note.
    Nudge {
        call: String,
        times: u32,
    },
    Stop(StopCause),
}

/// What the end needs to know.
#[derive(Debug, Default)]
pub struct Facts {
    pub vendor_session: Option<String>,
    pub result_text: Option<String>,
    pub structured: Option<Value>,
    pub result_is_error: Option<bool>,
    /// Results of the current process, one per finished turn.
    pub results: u32,
    pub last_message: Option<String>,
    pub fatal_error: Option<SeenError>,
    pub last_error: Option<SeenError>,
    pub quota_rejected: Option<Option<OffsetDateTime>>,
    /// Tool calls started and not finished.
    pub open_tools: BTreeSet<String>,
    /// The model the session asked for, else the one its CLI reported.
    pub model: Option<String>,
    stderr_tail: VecDeque<String>,
}

impl Facts {
    pub fn end_facts(&self) -> EndFacts {
        let tail: Vec<&str> = self.stderr_tail.iter().map(String::as_str).collect();
        EndFacts {
            result_is_error: self.result_is_error,
            result_text: self.result_text.clone(),
            fatal_error: self.fatal_error.clone(),
            last_error: self.last_error.clone(),
            quota_rejected: self.quota_rejected,
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
    pub facts: Facts,
    started: Instant,
    budget_sent: Option<(BudgetStatus, Instant)>,
    stalled: bool,
    events: u32,
    unknown: u32,
    /// Events the adapter understood, leaving out the errors and results it makes up itself
    /// when a stream ends badly.
    understood: u32,
    broken_reported: bool,
}

impl Observer {
    pub fn new(ctx: Arc<Context>, out: Outbox, started: Instant) -> Self {
        let spec = &ctx.spec;
        let context_window = ctx
            .registry
            .as_ref()
            .and_then(|registry| registry.get(&spec.worker))
            .and_then(|entry| entry.context_window);
        let rotate_at = ctx
            .settings
            .sessions
            .rotate_at
            .tokens(context_window)
            .unwrap_or_else(|err| {
                out.notice(format!("sessions.{err}, so the session won't rotate"));
                None
            });
        let mut meter = SessionMeter::new(MeterSetup {
            limits: spec.limits.clone(),
            pricing: meter::pricing(
                &spec.worker,
                &ctx.settings,
                ctx.registry.as_ref(),
                ctx.adapter.model_base_url().as_deref(),
            ),
            capabilities: ctx.adapter.capabilities(),
            budgets_enabled: ctx.settings.budget.enabled,
            rotate_at,
            context_window,
        });
        meter.resumed_from(meter::spent_before(spec, ctx.adapter.id()));
        let watchdogs =
            Watchdogs::new(&spec.limits, fanout_cap(&ctx)).with_heartbeat(ctx.adapter.heartbeat());
        let diffs = Diffs::start(FileDiffer::new(spec.workdir.clone()), out.clone());
        let boards = Boards::start(ctx.clone(), out.clone());
        let asked_for = spec.worker.model.clone().or_else(|| {
            let agent = ctx.settings.agents.get(&spec.worker.worker);
            agent.and_then(|agent| agent.model.clone())
        });
        Self {
            ctx,
            out,
            meter,
            watchdogs,
            diffs,
            boards,
            context_window,
            facts: Facts {
                model: asked_for,
                ..Facts::default()
            },
            started,
            budget_sent: None,
            stalled: false,
            events: 0,
            unknown: 0,
            understood: 0,
            broken_reported: false,
        }
    }

    pub fn spend(&self) -> Spend {
        self.meter.spend()
    }

    pub fn tool_calls(&self) -> u64 {
        self.meter.tool_calls()
    }

    /// Emits `Session(VendorSession)` the first time an id is known, and again if it changes.
    pub fn vendor_session(&mut self, id: String) {
        if id.is_empty() || self.facts.vendor_session.as_deref() == Some(id.as_str()) {
            return;
        }
        self.facts.vendor_session = Some(id.clone());
        self.out.session(SessionEvent::VendorSession {
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

    /// The same vendor session goes on in a new process: only the difference in its totals
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
        if let AgentEvent::ContextFill { size, .. } = &mut event
            && size.is_none()
        {
            *size = self.context_window;
        }
        self.events += 1;
        if !matches!(
            event,
            AgentEvent::Unknown { .. } | AgentEvent::Error { .. } | AgentEvent::Result { .. }
        ) {
            self.understood += 1;
        }
        self.out.agent(event.clone());
        self.note(&event);
        for path in changed_paths(&event) {
            self.diffs.changed(path);
        }
        let mut actions = Vec::new();
        let verdict = self.meter.observe(&event);
        actions.extend(meter_action(verdict));
        if matches!(event, AgentEvent::Usage { .. }) {
            self.record_spend();
        }
        let verdict = self.watchdogs.observe(&event, Instant::now());
        actions.extend(self.watchdog_action(verdict));
        self.budget(false);
        actions
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
        self.budget(false);
        actions
    }

    /// The diffs still owed, what the boards still have to take, and the latest budget
    /// reading, before the end.
    pub async fn finish(&mut self) {
        self.check_untested_adapter().await;
        self.diffs.flush(&self.out).await;
        if !self.boards.settle().await {
            self.out
                .notice("the quota and health boards didn't take this session's last readings");
        }
        self.record_spend();
        self.budget(true);
    }

    /// Keeps what the vendor session has spent so far, for a later session that resumes it.
    fn record_spend(&self) {
        let spec = &self.ctx.spec;
        let Some(id) = self
            .facts
            .vendor_session
            .as_deref()
            .or(spec.resume.as_deref())
        else {
            return;
        };
        meter::record_vendor_session_spend(
            &spec.workdir,
            self.ctx.adapter.id(),
            id,
            self.meter.total_spend(),
        );
    }

    fn note(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init {
                session_id, model, ..
            } => {
                if let Some(id) = session_id {
                    self.vendor_session(id.clone());
                }
                if self.facts.model.is_none() {
                    self.facts.model.clone_from(model);
                }
            }
            AgentEvent::Message { text, parent: None } => {
                self.facts.last_message = Some(text.clone());
            }
            AgentEvent::ToolStarted { id, .. } => {
                self.facts.open_tools.insert(id.clone());
            }
            AgentEvent::ToolFinished { id, .. } => {
                self.facts.open_tools.remove(id);
            }
            AgentEvent::Quota { reading } => {
                if reading.status == QuotaStatus::Rejected {
                    self.facts.quota_rejected = Some(reading.resets_at);
                }
                self.boards.quota(reading.clone());
                self.out.vendor(VendorEvent::Quota(reading.clone()));
            }
            AgentEvent::Retry { reason, .. } => self.signal(HealthSignal::Retry {
                vendor: self.ctx.vendor.clone(),
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
                if about_the_vendor(*category) {
                    self.signal(HealthSignal::Error {
                        vendor: self.ctx.vendor.clone(),
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
                if structured.is_some() {
                    facts.structured = structured.clone();
                }
                if !is_error {
                    self.signal(HealthSignal::Success {
                        vendor: self.ctx.vendor.clone(),
                        session: self.ctx.spec.key.clone(),
                    });
                }
            }
            AgentEvent::Unknown { .. } => {
                self.unknown += 1;
                self.check_adapter();
            }
            _ => {}
        }
    }

    /// Tells the health board, which sends on any change of the vendor's state it causes.
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
            vendor: self.ctx.vendor.clone(),
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
            WatchdogVerdict::SoftStop(_) => Some(Action::WrapUp(None)),
            WatchdogVerdict::Stuck(watchdog) => Some(Action::Stop(StopCause::Stuck(watchdog))),
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

    /// `Limit(Budget)` when the reading changed, at most once a second unless `now`.
    fn budget(&mut self, now: bool) {
        let status = self.meter.status();
        let at = Instant::now();
        match self.budget_sent {
            Some((sent, _)) if sent == status => return,
            Some((_, sent_at)) if !now && at.duration_since(sent_at) < BUDGET_EVERY => return,
            _ => {}
        }
        self.budget_sent = Some((status, at));
        self.out
            .session(SessionEvent::Limit(LimitEvent::Budget(status)));
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

/// Errors that say something about the vendor rather than this session's request.
fn about_the_vendor(category: ErrorCategory) -> bool {
    !matches!(
        category,
        ErrorCategory::ContextFull
            | ErrorCategory::InvalidRequest
            | ErrorCategory::Permission
            | ErrorCategory::Budget
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

#[cfg(test)]
mod tests {
    use argonauts_core::events::session::{EndReason, Watchdog};
    use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
    use argonauts_core::worker::{SessionHandle, SessionLimits, SessionSpec, Worker, WorkerEvent};

    use super::*;
    use crate::harness_worker::capabilities::CLAUDE_CODE;
    use crate::harness_worker::outbox;
    use crate::harness_worker::testing::{
        Harness, TestAdapter, collect, event_line, session_events, spec_in, unique_key,
    };
    use crate::health::is_vendor_trouble;

    const MINUTE: Duration = Duration::from_secs(60);

    /// An observer of one session, with what it needs kept alive.
    struct Watched {
        obs: Observer,
        handle: SessionHandle,
        started: Instant,
        _harness: Harness,
        _dir: tempfile::TempDir,
    }

    fn watched(adapter: TestAdapter, limits: SessionLimits) -> Watched {
        let harness = Harness::new(adapter);
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.limits = limits;
        let (handle, out, _control) = outbox::open(spec.key.clone());
        let home = harness.worker.home().unwrap();
        let ctx = Arc::new(Context::new(&harness.worker, spec, home));
        let started = Instant::now();
        Watched {
            obs: Observer::new(ctx, out, started),
            handle,
            started,
            _harness: harness,
            _dir: dir,
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
                if let WorkerEvent::Agent(AgentEvent::Error {
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
            [Action::Stop(StopCause::Stuck(Watchdog::Inactivity))]
        );
    }

    #[tokio::test]
    async fn minutes_run_on_across_a_resume() {
        let h = Harness::new(TestAdapter::new("true"));
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "hi");
        spec.limits.max_minutes = Some(60.0);
        let (_handle, out, _control) = outbox::open(spec.key.clone());
        let ctx = Arc::new(Context::new(&h.worker, spec, h.worker.home().unwrap()));
        let started = Instant::now();
        let mut obs = Observer::new(ctx, out, started);

        let actions = obs.tick_at(started + MINUTE * 51, None);
        assert!(
            actions.contains(&Action::WrapUp(Some(BudgetDimension::Minutes))),
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
            session_id: Some("vendor-7".into()),
            model: None,
            tools: Vec::new(),
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
        let h = Harness::new(adapter);
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().canonicalize().unwrap();
        let mut spec = spec_in(&work, &work.join("first"), "start");
        spec.limits.budget = Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 2.0,
        });
        let (_, first) = collect(h.worker.start(spec.clone()).await.unwrap()).await;
        assert_eq!(first.vendor_session.as_deref(), Some("vendor-7"));
        assert!((first.spend.cost_usd - 0.6).abs() < 1e-9);

        let resumed = SessionSpec {
            key: unique_key(),
            session_dir: work.join("second"),
            resume: Some("vendor-7".into()),
            ..spec
        };
        let (events, end) = collect(h.worker.start(resumed).await.unwrap()).await;
        assert!((end.spend.cost_usd - 0.4).abs() < 1e-9, "{:?}", end.spend);
        assert!(!matches!(end.reason, EndReason::BudgetHit { .. }));
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
            ErrorCategory::Budget,
            ErrorCategory::Timeout,
            ErrorCategory::Interrupted,
            ErrorCategory::Cli,
            ErrorCategory::Unknown,
        ];
        for category in all {
            if is_vendor_trouble(category) {
                assert!(about_the_vendor(category), "{category:?}");
            }
        }
    }
}
