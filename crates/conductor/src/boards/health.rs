//! The health board: a circuit breaker per agent, in `agents.json` beside the plan usage, and one
//! per model under it ([`crate::model_health`]). Errors or retries within `health.window` make an
//! agent Degraded. An overload or a server error counts against the model it came from, and two
//! models out at once take the whole agent Down; network errors and timeouts are nobody's model,
//! so `down_after` of them across `down_sessions` sessions take it Down too. After the cool-down
//! (or its reported reset) it goes to Testing. There the first session to report takes the one
//! test task, and its success or outage decides, within [`TEST_TASK_LIMIT`]. A broken adapter
//! disables the agent, and a billing cap disables its API-key models. Every change is broadcast.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::Paths;
use argonauts_core::base::clock::{Clock, SystemClock};
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::{AgentId, ModelRef, SessionKey};
use argonauts_core::models::limits::{
    AgentHealth, HealthBoard, HealthSignal, HealthState, ModelHealth, ModelState, NetState,
    PlanUsageStatus, TEST_TASK_SUCCEEDED, served_on_this_machine,
};
use argonauts_core::models::registry::{ModelRegistry, model_run};
use argonauts_core::runs::events::AgentStatusEvent;
use argonauts_core::settings::Span;
use argonauts_core::settings::sessions::HealthSettings;
use argonauts_core::{ErrorCategory, RetryReason};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::{broadcast, watch};

use crate::boards::agents_file::{AgentRecord, AgentsFile, AgentsStore, Holder};
use crate::boards::model_health::{self, MODELS_OUT_TO_DOWN};
use crate::boards::network::{Internet, Network};
use crate::boards::plan_usage::registered_prober;

/// Marks kept when `health.window` is `none`, so the file stays small.
const MAX_MARKS: usize = 256;

/// How long Testing waits for the test task's result before it counts as failed, so a test task
/// that stopped without one can't hold the agent in Testing for good.
pub const TEST_TASK_LIMIT: Duration = Duration::from_secs(60 * 60);

/// Why an agent went down when its models did.
pub const MODELS_OUT: &str = "two of its models are out";

/// Starts every billing disable. `AgentHealth` carries a disable as text alone, so this is how
/// readers tell its scope.
const BILLING_CAP: &str = "the billing cap was reached";

/// Which of an agent's models a disable keeps from new tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisableScope {
    Every,
    /// Only those billed to an API key, after a billing cap.
    ApiKey,
}

pub fn disable_scope(reason: &str) -> DisableScope {
    if reason.starts_with(BILLING_CAP) {
        DisableScope::ApiKey
    } else {
        DisableScope::Every
    }
}

/// The disable a billing cap sets. It lasts until a session on the agent succeeds, such as the
/// retry after you top up, or until you enable the agent.
pub fn billing_disable() -> String {
    format!("{BILLING_CAP}. Tasks billed to its API key go to other agents until a retry works")
}

/// One error or retry the breaker counts, persisted so every process counts the same ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthMark {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The session, prefixed by its process, since two runs each have an `s-001`.
    pub session: String,
    /// Overload or a server error, which its model's own breaker counts toward out.
    #[serde(default)]
    pub outage: bool,
    /// A network error or a timeout, which is no model's doing, so it counts toward the whole
    /// agent's Down.
    #[serde(default)]
    pub network: bool,
    /// From a session that went out through the sandbox's proxy, whose network may fail where
    /// the agent's own doesn't.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub proxied: bool,
}

/// The sessions of this process that go out through the sandbox's proxy.
static PROXIED: Mutex<BTreeSet<SessionKey>> = Mutex::new(BTreeSet::new());

/// Notes that `session` goes out through the sandbox's proxy, so its network errors are told
/// apart from the agent's own.
pub fn goes_through_proxy(session: &SessionKey) {
    PROXIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session.clone());
}

fn through_proxy(signal: &HealthSignal) -> bool {
    let session = match signal {
        HealthSignal::Retry { session, .. }
        | HealthSignal::Error { session, .. }
        | HealthSignal::Success { session, .. } => session,
        HealthSignal::AdapterBroken { .. } => return false,
    };
    PROXIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(session)
}

/// One agent's breaker as plain state, so its rules are tested without files or clocks.
#[derive(Debug, Clone)]
pub struct Breaker {
    settings: HealthSettings,
    state: HealthState,
    since: OffsetDateTime,
    reason: Option<String>,
    next_test_at: Option<OffsetDateTime>,
    reported_reset: Option<OffsetDateTime>,
    left_down_at: Option<OffsetDateTime>,
    marks: Vec<HealthMark>,
    /// A test task is running, while Testing.
    testing_task: bool,
    /// When a session that went straight out last finished its work.
    direct_success_at: Option<OffsetDateTime>,
    /// What the network check knows, so an outage of the internet blames nobody.
    internet: Internet,
}

impl Breaker {
    pub fn new(settings: &HealthSettings) -> Self {
        Self {
            settings: settings.clone(),
            state: HealthState::Healthy,
            since: OffsetDateTime::UNIX_EPOCH,
            reason: None,
            next_test_at: None,
            reported_reset: None,
            left_down_at: None,
            marks: Vec::new(),
            testing_task: false,
            direct_success_at: None,
            internet: Internet::Unchecked,
        }
    }

    /// A breaker as `agents.json` last recorded it.
    pub fn restore(settings: &HealthSettings, entry: &AgentRecord) -> Self {
        let mut breaker = Self::new(settings);
        if let Some(health) = &entry.health {
            breaker.state = health.state;
            breaker.since = health.since;
            breaker.reason = health.reason.clone();
            breaker.next_test_at = health.next_test_at;
        }
        breaker.left_down_at = entry.left_down_at;
        breaker.marks = entry.marks.clone();
        breaker.direct_success_at = entry.direct_success_at;
        breaker.testing_task = breaker.state == HealthState::Testing && entry.test_task.is_some();
        breaker
    }

    pub fn state(&self) -> HealthState {
        self.state
    }

    pub fn since(&self) -> OffsetDateTime {
        self.since
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn next_test_at(&self) -> Option<OffsetDateTime> {
        self.next_test_at
    }

    pub fn left_down_at(&self) -> Option<OffsetDateTime> {
        self.left_down_at
    }

    pub fn marks(&self) -> &[HealthMark] {
        &self.marks
    }

    pub fn testing_task(&self) -> bool {
        self.testing_task
    }

    pub fn health(&self, agent_id: &AgentId, disabled: Option<String>) -> AgentHealth {
        AgentHealth {
            agent_id: agent_id.clone(),
            state: self.state,
            since: self.since,
            reason: self.reason.clone(),
            next_test_at: self.next_test_at,
            disabled,
        }
    }

    /// Writes the breaker into its `agents.json` entry, keeping the entry's disable.
    pub fn save(&self, agent_id: &AgentId, entry: &mut AgentRecord) {
        let disabled = entry.health.as_ref().and_then(|h| h.disabled.clone());
        entry.health = Some(self.health(agent_id, disabled));
        entry.left_down_at = self.left_down_at;
        entry.marks = self.marks.clone();
        entry.direct_success_at = self.direct_success_at;
    }

    /// The reset the agent reported, such as when its plan usage comes back. While it's down, the
    /// test waits for this instead of the cool-down.
    /// Counts network errors only as far as the network check knows the internet is there.
    pub fn knowing(&mut self, internet: Internet) {
        self.internet = internet;
    }

    pub fn set_reported_reset(&mut self, reset: Option<OffsetDateTime>) {
        self.reported_reset = reset;
        if self.state == HealthState::Down {
            self.next_test_at = self.test_time(self.since);
        }
    }

    /// The new state, if the signal changed it.
    pub fn observe(&mut self, signal: &HealthSignal, now: OffsetDateTime) -> Option<HealthState> {
        self.observe_from(signal, "", false, now)
    }

    /// Like [`Breaker::observe`], with sessions counted as `<scope>/<session>`, so sessions
    /// of different processes stay apart, and `proxied` when the session went out through the
    /// sandbox's proxy. Its network errors don't count toward Down while sessions that go
    /// straight out still finish their work. While the internet is down, network errors and
    /// errors nobody understood aren't marked at all, and a network error counts only once
    /// the internet was found up after it.
    pub fn observe_from(
        &mut self,
        signal: &HealthSignal,
        scope: &str,
        proxied: bool,
        now: OffsetDateTime,
    ) -> Option<HealthState> {
        if matches!(signal, HealthSignal::Success { .. }) && !proxied {
            self.direct_success_at = Some(now);
        }
        let (session, outage, network) = match signal {
            // Sign-in retries count as sign-in, not as trouble.
            HealthSignal::Retry { reason, .. } if reason.is_sign_in() => return None,
            HealthSignal::Retry {
                session, reason, ..
            } => (
                session,
                matches!(reason, RetryReason::Overloaded | RetryReason::ServerError),
                *reason == RetryReason::Network,
            ),
            HealthSignal::Error {
                session, category, ..
            } if is_agent_trouble(*category) => (
                session,
                matches!(category, ErrorCategory::Overloaded | ErrorCategory::Server),
                matches!(category, ErrorCategory::Network | ErrorCategory::Timeout),
            ),
            HealthSignal::Success { .. } => {
                return match self.state {
                    HealthState::Degraded => self.recover_if_quiet(now),
                    // A session that finished its work is as good a test as the test task.
                    HealthState::Down | HealthState::Testing => {
                        self.marks.clear();
                        self.set(
                            HealthState::Healthy,
                            now,
                            Some(TEST_TASK_SUCCEEDED.to_owned()),
                        )
                    }
                    HealthState::Healthy => None,
                };
            }
            HealthSignal::Error { .. } | HealthSignal::AdapterBroken { .. } => return None,
        };
        // Down and Testing wait for the test task, so what their paused tasks say is not counted.
        if matches!(self.state, HealthState::Down | HealthState::Testing) {
            return None;
        }
        if self.internet.is_down() && may_be_the_network(signal) {
            return None;
        }
        self.marks.push(HealthMark {
            at: now,
            session: if scope.is_empty() {
                session.to_string()
            } else {
                format!("{scope}/{session}")
            },
            outage,
            network,
            proxied,
        });
        self.prune(now);
        self.judge(now)
    }

    /// Takes the agent down, or calls it degraded, by the marks that count. A network error
    /// counts only once the internet was found up after it, which may be a while after it came.
    fn judge(&mut self, now: OffsetDateTime) -> Option<HealthState> {
        let direct_works = self.direct_success_at.is_some_and(|at| {
            span(&self.settings.window, "health.window").is_none_or(|window| at > now - window)
        });
        let internet = self.internet;
        let blamed = |mark: &&HealthMark| !mark.network || internet.blames(mark.at);
        let unreachable: Vec<&HealthMark> = self
            .marks
            .iter()
            .filter(blamed)
            .filter(|mark| mark.network && !(mark.proxied && direct_works))
            .collect();
        let sessions: BTreeSet<&str> = unreachable.iter().map(|m| m.session.as_str()).collect();
        if unreachable.len() >= at_least_one(self.settings.down_after)
            && sessions.len() >= at_least_one(self.settings.down_sessions)
        {
            let on = match sessions.len() {
                1 => "1 session".to_owned(),
                many => format!("{many} sessions"),
            };
            let reason = format!("network errors on {on} in {}", self.settings.window.0);
            return self.go_down(now, reason);
        }
        let counted = self.marks.iter().filter(blamed).count();
        if self.state == HealthState::Healthy
            && counted >= at_least_one(self.settings.degraded_after)
        {
            let reason = format!("{counted} errors or retries in {}", self.settings.window.0);
            return self.set(HealthState::Degraded, now, Some(reason));
        }
        None
    }

    /// Moves Down to Testing once the cool-down or the reported reset has passed, and Testing
    /// back to Down when no test task finished within [`TEST_TASK_LIMIT`].
    pub fn tick(&mut self, now: OffsetDateTime) -> Option<HealthState> {
        match self.state {
            HealthState::Down => {
                let due = self.next_test_at.is_none_or(|at| at <= now);
                if !due {
                    return None;
                }
                let reason = match self.reported_reset {
                    Some(reset) if reset > self.since && reset <= now => {
                        "the reported reset has passed"
                    }
                    _ => "the cool-down is over",
                };
                self.left_down_at = Some(now);
                self.next_test_at = None;
                self.set(HealthState::Testing, now, Some(reason.to_owned()))
            }
            HealthState::Testing if self.since + TEST_TASK_LIMIT <= now => {
                let minutes = TEST_TASK_LIMIT.as_secs() / 60;
                self.go_down(
                    now,
                    format!("no test task finished within {minutes} minutes"),
                )
            }
            HealthState::Degraded => self.recount(now).or_else(|| self.recover_if_quiet(now)),
            HealthState::Healthy => self.recount(now),
            HealthState::Testing => None,
        }
    }

    /// Network errors the check has since found the internet up after count now. Without a
    /// check, each one counted when it came.
    fn recount(&mut self, now: OffsetDateTime) -> Option<HealthState> {
        let internet = self.internet;
        let confirmed = internet != Internet::Unchecked
            && self
                .marks
                .iter()
                .any(|mark| mark.network && internet.blames(mark.at));
        if !confirmed {
            return None;
        }
        self.prune(now);
        self.judge(now)
    }

    /// Starts the one test task an agent in Testing takes, and its time limit with it. `false`
    /// when it isn't testing or a test task already runs.
    pub fn start_test(&mut self, now: OffsetDateTime) -> bool {
        if self.state != HealthState::Testing || self.testing_task {
            return false;
        }
        self.testing_task = true;
        self.since = now;
        self.reason = Some("a test task is running".to_owned());
        true
    }

    /// The test task's result: Healthy, or Down again.
    pub fn test_result(&mut self, ok: bool, now: OffsetDateTime) -> HealthState {
        if self.state != HealthState::Testing {
            return self.state;
        }
        if ok {
            self.marks.clear();
            self.set(
                HealthState::Healthy,
                now,
                Some(TEST_TASK_SUCCEEDED.to_owned()),
            );
        } else {
            self.go_down(now, "the test task failed".to_owned());
        }
        self.state
    }

    /// Two of its models are out at once, so the agent is down as a whole.
    pub fn models_out(&mut self, now: OffsetDateTime) -> Option<HealthState> {
        if matches!(self.state, HealthState::Down | HealthState::Testing) {
            return None;
        }
        self.go_down(now, MODELS_OUT.to_owned())
    }

    fn go_down(&mut self, now: OffsetDateTime, reason: String) -> Option<HealthState> {
        self.marks.clear();
        self.next_test_at = self.test_time(now);
        self.set(HealthState::Down, now, Some(reason))
    }

    /// When an agent that went down at `down_at` gets its test task: the reported reset if it
    /// came after, else the cool-down. A cool-down of `none` tests at once.
    fn test_time(&self, down_at: OffsetDateTime) -> Option<OffsetDateTime> {
        match self.reported_reset {
            Some(reset) if reset > down_at => Some(reset),
            _ => Some(down_at + cooldown(&self.settings)),
        }
    }

    fn recover_if_quiet(&mut self, now: OffsetDateTime) -> Option<HealthState> {
        self.prune(now);
        if self.marks.len() < at_least_one(self.settings.degraded_after) {
            self.set(
                HealthState::Healthy,
                now,
                Some("the errors stopped".to_owned()),
            )
        } else {
            None
        }
    }

    /// Forgets marks older than the window. A window of `none` keeps the latest few.
    fn prune(&mut self, now: OffsetDateTime) {
        if let Some(window) = span(&self.settings.window, "health.window") {
            self.marks.retain(|mark| mark.at > now - window);
        }
        let extra = self.marks.len().saturating_sub(MAX_MARKS);
        self.marks.drain(..extra);
    }

    fn set(
        &mut self,
        state: HealthState,
        now: OffsetDateTime,
        reason: Option<String>,
    ) -> Option<HealthState> {
        let changed = self.state != state;
        self.state = state;
        self.since = now;
        self.reason = reason;
        if state != HealthState::Down {
            self.next_test_at = None;
        }
        if state != HealthState::Testing {
            self.testing_task = false;
        }
        changed.then_some(state)
    }
}

/// Errors that say something about the agent's service. Account problems (login, usage limit,
/// billing) go to the fallback rules, and request or local problems say nothing about health.
pub(crate) fn is_agent_trouble(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::Overloaded
            | ErrorCategory::Server
            | ErrorCategory::Network
            | ErrorCategory::Timeout
            | ErrorCategory::RateLimit
            | ErrorCategory::ModelUnavailable
            | ErrorCategory::Unknown
    )
}

/// What a signal says about the agent under test: a success passes it, and an error of the
/// kinds that take an agent or its models out fails it. `None` when it says neither, as a
/// network error says nothing while the internet is down.
fn test_verdict(signal: &HealthSignal, internet: Internet) -> Option<bool> {
    if internet.is_down() && may_be_the_network(signal) {
        return None;
    }
    match signal {
        HealthSignal::Success { .. } => Some(true),
        HealthSignal::Error {
            category:
                ErrorCategory::Overloaded
                | ErrorCategory::Server
                | ErrorCategory::Network
                | ErrorCategory::Timeout,
            ..
        } => Some(false),
        HealthSignal::Retry { .. }
        | HealthSignal::Error { .. }
        | HealthSignal::AdapterBroken { .. } => None,
    }
}

/// The session a signal about the agent's service came from. Account and request errors come
/// from sessions that won't finish, so they don't take the test task.
fn reporting_session(signal: &HealthSignal) -> Option<&SessionKey> {
    match signal {
        HealthSignal::Retry { session, .. } | HealthSignal::Success { session, .. } => {
            Some(session)
        }
        HealthSignal::Error {
            session, category, ..
        } if is_agent_trouble(*category) => Some(session),
        HealthSignal::Error { .. } | HealthSignal::AdapterBroken { .. } => None,
    }
}

/// How long a down agent rests before its test task. `none` rests not at all.
pub(crate) fn cooldown(settings: &HealthSettings) -> Duration {
    span(&settings.cooldown, "health.cooldown").unwrap_or_default()
}

/// The span the counts look back over. `None` for `none`, which forgets nothing.
pub(crate) fn window(settings: &HealthSettings) -> Option<Duration> {
    span(&settings.window, "health.window")
}

/// A threshold of 0 would trip on nothing at all, so it reads as 1.
fn at_least_one(n: u32) -> usize {
    n.max(1) as usize
}

/// `None` for `none`. A span settings validation missed falls back to its default.
fn span(value: &Span, key: &str) -> Option<Duration> {
    match value.parse() {
        Ok(duration) => duration,
        Err(err) => {
            let defaults = HealthSettings::default();
            let fallback = if key.ends_with("window") {
                defaults.window
            } else {
                defaults.cooldown
            };
            tracing::warn!("{key}: {err}; using {}", fallback.0);
            fallback.parse().ok().flatten()
        }
    }
}

pub struct FileHealthBoard {
    store: AgentsStore,
    settings: HealthSettings,
    events: broadcast::Sender<AgentStatusEvent>,
    clock: Arc<dyn Clock>,
    /// What this board last told subscribers, so changes made by other processes are
    /// broadcast too once it reads them.
    seen: Mutex<BTreeMap<String, AgentHealth>>,
    /// The same for each model.
    seen_models: Mutex<BTreeMap<ModelRef, ModelHealth>>,
    /// Prefixes session keys with this process, like `<pid>.<start time>`.
    scope: String,
    /// Says which model a session runs, so a model named by an alias, by its full id, or not
    /// at all is kept once.
    models: Option<Arc<dyn ModelRegistry>>,
    /// The spelling found for each name asked about so far, since every read asks again.
    spelled: Mutex<BTreeMap<(String, Option<String>), String>>,
    /// Whether the internet is there at all, which a network error makes it check.
    network: Arc<Network>,
}

impl FileHealthBoard {
    pub fn new(paths: &Paths, settings: &HealthSettings) -> Self {
        let (events, _) = broadcast::channel(64);
        let store = AgentsStore::new(paths);
        let file = store.read().unwrap_or_default();
        let seen = file
            .agent_ids
            .iter()
            .filter_map(|(id, entry)| Some((id.clone(), entry.health.clone()?)))
            .collect();
        let seen_models = model_healths(&file)
            .map(|health| (health.model.clone(), health))
            .collect();
        let scope = crate::process::liveness::this_process()
            .map(|(pid, start)| format!("{pid}.{start}"))
            .unwrap_or_else(|| std::process::id().to_string());
        Self {
            store,
            settings: settings.clone(),
            events,
            clock: Arc::new(SystemClock),
            seen: Mutex::new(seen),
            seen_models: Mutex::new(seen_models),
            scope,
            models: None,
            spelled: Mutex::default(),
            network: Arc::new(default_network()),
        }
    }

    /// Checks the internet with `network`, as tests do with a probe of their own.
    pub fn with_network(mut self, network: Network) -> Self {
        self.network = Arc::new(network.with_clock(self.clock.clone()));
        self
    }

    /// Keeps each model under the one name `registry` gives it, and counts what a session
    /// that names no model says against the model its CLI runs by default.
    pub fn naming_models_with(mut self, registry: Arc<dyn ModelRegistry>) -> Self {
        self.models = Some(registry);
        self
    }

    /// The name `model` of `agent` is kept under: empty for a default nothing can name.
    fn kept_as(&self, agent: &str, model: Option<&str>) -> String {
        let asked = (agent.to_owned(), model.map(str::to_owned));
        if let Ok(spelled) = self.spelled.lock()
            && let Some(known) = spelled.get(&asked)
        {
            return known.clone();
        }
        let worker = ModelRef::new(agent, asked.1.clone());
        let name = match &self.models {
            Some(registry) => model_run(registry.as_ref(), &worker),
            None => asked.1.clone(),
        }
        .unwrap_or_default();
        // A default nothing names yet gets its name once its CLI's list is read.
        if !name.is_empty()
            && let Ok(mut spelled) = self.spelled.lock()
        {
            spelled.insert(asked, name.clone());
        }
        name
    }

    /// Uses `clock` instead of the system time, as tests do.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        if let Some(network) = Arc::get_mut(&mut self.network) {
            network.set_clock(clock.clone());
        }
        self.clock = clock;
        self
    }

    pub fn store(&self) -> &AgentsStore {
        &self.store
    }

    /// Moves every agent whose time has come (a finished cool-down, errors that stopped) and
    /// broadcasts what changed, here or in another process. The app calls this on a timer.
    pub fn refresh(&self) {
        let file = match self.store.read() {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("the health board could not read agents.json: {err}");
                return;
            }
        };
        let now = self.clock.now();
        let due: Vec<AgentId> = file
            .agent_ids
            .iter()
            .filter(|(_, entry)| self.is_due(entry, now))
            .map(|(id, _)| AgentId(id.clone()))
            .collect();
        if due.is_empty() {
            self.publish(&file);
        }
        for agent_id in due {
            self.change(&agent_id, |_, _, _| ());
        }
    }

    /// Claims the one test task an agent in Testing takes. `false` when it isn't testing or
    /// another live session has the task.
    pub fn claim_test_task(&self, agent_id: &AgentId, session: &str) -> bool {
        let holder = match Holder::this_process(Some(session.to_owned())) {
            Ok(holder) => holder,
            Err(err) => {
                tracing::warn!("could not claim {agent_id}'s test task: {err}");
                return false;
            }
        };
        self.change(agent_id, |breaker, entry, now| {
            let claimed = breaker.start_test(now);
            if claimed {
                entry.test_task = Some(holder);
            }
            claimed
        })
        .unwrap_or(false)
    }

    /// Lifts a disable, such as after the CLI was fixed.
    pub fn enable(&self, agent_id: &AgentId) {
        self.change(agent_id, |_, entry, _| {
            entry.disabled_by = None;
            if let Some(health) = &mut entry.health {
                health.disabled = None;
            }
        });
    }

    fn disable_adapter(&self, agent_id: &AgentId, detail: &str) {
        let reason = format!(
            "{}. {}",
            detail.trim_end_matches('.'),
            pin_suggestion(agent_id)
        );
        self.disable(agent_id, reason);
    }

    /// Records a disable owned by this process, so it lapses with it. A billing cap never
    /// narrows a disable that covers every model.
    fn disable(&self, agent_id: &AgentId, reason: String) {
        let holder = Holder::this_process(None);
        self.change(agent_id, |breaker, entry, _| {
            let health = entry
                .health
                .get_or_insert_with(|| breaker.health(agent_id, None));
            let narrower = disable_scope(&reason) == DisableScope::ApiKey
                && health
                    .disabled
                    .as_deref()
                    .is_some_and(|current| disable_scope(current) == DisableScope::Every);
            if narrower {
                return;
            }
            health.disabled = Some(reason);
            entry.disabled_by = holder.ok();
        });
    }

    /// While Testing, the first session to report takes the test task, and only what that
    /// session says decides the test.
    fn observe_test(
        breaker: &mut Breaker,
        entry: &mut AgentRecord,
        signal: &HealthSignal,
        now: OffsetDateTime,
    ) {
        let Some(session) = reporting_session(signal) else {
            return;
        };
        let holder = Holder::this_process(Some(session.to_string()))
            .map_err(|err| tracing::warn!("could not claim a test task: {err}"))
            .ok();
        let holds = match (&entry.test_task, &holder) {
            (Some(task), Some(holder)) => task == holder,
            (Some(_), None) => false,
            (None, _) => {
                breaker.start_test(now);
                entry.test_task = holder;
                true
            }
        };
        if holds && let Some(ok) = test_verdict(signal, breaker.internet) {
            breaker.test_result(ok, now);
        }
    }

    fn is_due(&self, entry: &AgentRecord, now: OffsetDateTime) -> bool {
        let mut breaker = self.breaker(entry);
        breaker.tick(now).is_some()
            || entry
                .models
                .values()
                .any(|model| model.clone().tick(&self.settings, now))
    }

    fn breaker(&self, entry: &AgentRecord) -> Breaker {
        let mut breaker = Breaker::restore(&self.settings, entry);
        let reset = entry
            .plan_usage
            .as_ref()
            .filter(|plan_usage| plan_usage.status == PlanUsageStatus::Rejected)
            .and_then(|plan_usage| plan_usage.resets_at);
        breaker.set_reported_reset(reset);
        breaker.knowing(self.network.internet());
        breaker
    }

    /// Changes one agent's entry under the file lock, after moving it along if its time came,
    /// then broadcasts what changed.
    fn change<T>(
        &self,
        agent_id: &AgentId,
        apply: impl FnOnce(&mut Breaker, &mut AgentRecord, OffsetDateTime) -> T,
    ) -> Option<T> {
        let now = self.clock.now();
        let updated = self.store.update(|file| {
            let entry = file.agent_ids.entry(agent_id.0.clone()).or_default();
            let mut breaker = self.breaker(entry);
            breaker.tick(now);
            for model in entry.models.values_mut() {
                model.tick(&self.settings, now);
            }
            let result = apply(&mut breaker, entry, now);
            breaker.save(agent_id, entry);
            if !breaker.testing_task() {
                entry.test_task = None;
            }
            (result, file.clone())
        });
        match updated {
            Ok((result, file)) => {
                self.publish(&file);
                Some(result)
            }
            Err(err) => {
                tracing::warn!("the health board could not write agents.json: {err}");
                None
            }
        }
    }

    /// Says what changed. A model's change goes out before its agent's, so whoever hears that
    /// an agent went down already knows which of its models are out, and why.
    fn publish(&self, file: &AgentsFile) {
        self.publish_models(file);
        self.publish_agents(file);
    }

    fn publish_models(&self, file: &AgentsFile) {
        let Ok(mut seen) = self.seen_models.lock() else {
            tracing::warn!("the health board's memory is poisoned");
            return;
        };
        for current in model_healths(file) {
            let before = seen.get(&current.model);
            let moved = before.map_or(current.state != ModelState::Healthy, |before| {
                (before.state, before.since, &before.reason)
                    != (current.state, current.since, &current.reason)
            });
            if moved {
                let _ = self.events.send(AgentStatusEvent::Model(current.clone()));
            }
            seen.insert(current.model.clone(), current);
        }
    }

    fn publish_agents(&self, file: &AgentsFile) {
        let Ok(mut seen) = self.seen.lock() else {
            tracing::warn!("the health board's memory is poisoned");
            return;
        };
        for (id, entry) in &file.agent_ids {
            let agent_id = AgentId(id.clone());
            let current = health_of(&agent_id, Some(entry));
            let before = seen
                .get(id)
                .cloned()
                .unwrap_or_else(|| health_of(&agent_id, None));
            if current == before {
                continue;
            }
            if let Some(reason) = &current.disabled
                && before.disabled.as_ref() != Some(reason)
            {
                let _ = self.events.send(AgentStatusEvent::Disabled {
                    agent_id: agent_id.clone(),
                    reason: reason.clone(),
                });
            }
            let moved = (
                current.state,
                current.since,
                &current.reason,
                current.next_test_at,
            ) != (
                before.state,
                before.since,
                &before.reason,
                before.next_test_at,
            );
            if moved || (current.disabled.is_none() && before.disabled.is_some()) {
                let _ = self.events.send(AgentStatusEvent::Health(current.clone()));
            }
            seen.insert(id.clone(), current);
        }
    }

    /// Counts what a session said against its own model, and takes the agent down once two of
    /// its models are out at once.
    fn observe_model(
        &self,
        breaker: &mut Breaker,
        entry: &mut AgentRecord,
        signal: &HealthSignal,
        name: Option<&str>,
        now: OffsetDateTime,
    ) {
        let (Some(said), Some(name)) = (model_health::seen(signal), name) else {
            return;
        };
        // Nothing is kept for a model nobody ever said a thing against.
        if said.is_ok() && !entry.models.contains_key(name) {
            return;
        }
        let model = entry.models.entry(name.to_owned()).or_default();
        let went_out = model.observe(said, &self.settings, now) && model.is_out();
        let out = entry.models.values().filter(|model| model.is_out()).count();
        if went_out && out >= MODELS_OUT_TO_DOWN {
            breaker.models_out(now);
        }
    }
}

/// The model a signal's session named, when the signal is about a model at all: `Some(None)`
/// for a session that named none.
fn model_of(signal: &HealthSignal) -> Option<Option<&str>> {
    match signal {
        HealthSignal::Retry { model, .. }
        | HealthSignal::Error { model, .. }
        | HealthSignal::Success { model, .. } => Some(model.as_deref()),
        HealthSignal::AdapterBroken { .. } => None,
    }
}

/// Every model the file holds anything against.
fn model_healths(file: &AgentsFile) -> impl Iterator<Item = ModelHealth> + '_ {
    file.agent_ids.iter().flat_map(|(id, entry)| {
        let agent_id = AgentId(id.clone());
        entry
            .models
            .iter()
            .map(move |(name, model)| model.health(&agent_id, name))
    })
}

/// A session on the agent worked, so its billing is fine again.
fn lift_billing_disable(entry: &mut AgentRecord) {
    if let Some(health) = &mut entry.health
        && health
            .disabled
            .as_deref()
            .is_some_and(|reason| disable_scope(reason) == DisableScope::ApiKey)
    {
        health.disabled = None;
        entry.disabled_by = None;
    }
}

fn health_of(agent_id: &AgentId, entry: Option<&AgentRecord>) -> AgentHealth {
    entry
        .and_then(|entry| entry.health.clone())
        .unwrap_or_else(|| Breaker::new(&HealthSettings::default()).health(agent_id, None))
}

/// How to get back to a CLI version the adapter was tested with.
fn pin_suggestion(agent_id: &AgentId) -> String {
    let tested = registered_prober(agent_id)
        .and_then(|prober| prober.adapter.tested_versions().last().copied());
    match tested {
        Some(version) => format!(
            "New tasks go to other agents. To pin a tested version, install {version} and set \
             agents.{agent_id}.version_pin = \"{version}\""
        ),
        None => format!(
            "New tasks go to other agents. To pin a tested version, install one and set \
             agents.{agent_id}.version_pin"
        ),
    }
}

/// The check a board starts with: a TCP connect to the providers' hosts. This crate's own tests
/// never reach past the machine, so theirs never checks unless a test gives it a probe, and
/// `ARGONAUTS_NETWORK_CHECK=off` turns it off for anything else.
fn default_network() -> Network {
    if cfg!(test) || crate::boards::network::check_is_off() {
        Network::unchecked()
    } else {
        Network::probing(Arc::new(crate::boards::network::TcpProbe::of_providers()))
    }
}

/// A network retry or error, or a timeout: what makes the board check the internet.
fn says_the_network(signal: &HealthSignal) -> bool {
    match signal {
        HealthSignal::Retry { reason, .. } => *reason == RetryReason::Network,
        HealthSignal::Error { category, .. } => {
            matches!(category, ErrorCategory::Network | ErrorCategory::Timeout)
        }
        HealthSignal::Success { .. } | HealthSignal::AdapterBroken { .. } => false,
    }
}

/// [`says_the_network`], or an error nobody understood, as an offline CLI's often is: while the
/// internet is down, none of these is held against anyone.
fn may_be_the_network(signal: &HealthSignal) -> bool {
    match signal {
        HealthSignal::Retry { reason, .. } => *reason == RetryReason::Network,
        HealthSignal::Error { category, .. } => matches!(
            category,
            ErrorCategory::Network | ErrorCategory::Timeout | ErrorCategory::Unknown
        ),
        HealthSignal::Success { .. } | HealthSignal::AdapterBroken { .. } => false,
    }
}

impl HealthBoard for FileHealthBoard {
    fn observe(&self, signal: HealthSignal) {
        let (HealthSignal::Retry { agent_id, .. }
        | HealthSignal::Error { agent_id, .. }
        | HealthSignal::Success { agent_id, .. }
        | HealthSignal::AdapterBroken { agent_id, .. }) = &signal;
        match &signal {
            HealthSignal::Success { model, .. }
                if !served_on_this_machine(&agent_id.0, model.as_deref()) =>
            {
                self.network.answered();
            }
            _ if says_the_network(&signal) => self.network.suspect(),
            _ => {}
        }
        match &signal {
            HealthSignal::AdapterBroken { detail, .. } => {
                self.disable_adapter(agent_id, detail);
                return;
            }
            HealthSignal::Error {
                category: ErrorCategory::Billing,
                ..
            } => {
                self.disable(agent_id, billing_disable());
                return;
            }
            HealthSignal::Retry { .. }
            | HealthSignal::Error { .. }
            | HealthSignal::Success { .. } => {}
        }
        let name = model_of(&signal).map(|model| self.kept_as(&agent_id.0, model));
        let name = name.as_deref();
        self.change(agent_id, |breaker, entry, now| {
            if matches!(signal, HealthSignal::Success { .. }) {
                lift_billing_disable(entry);
            }
            match breaker.state() {
                HealthState::Testing => {
                    Self::observe_test(breaker, entry, &signal, now);
                    // The session that passed the agent's test passed its own model's too.
                    let passed = breaker.state() == HealthState::Healthy;
                    if let (true, Some(name)) = (passed, name)
                        && let Some(model) = entry.models.get_mut(name)
                    {
                        model.pass(now);
                    }
                }
                // A session that finished its work shows the CLI answers again, as its test
                // task would. Anything else its paused tasks say waits for that test.
                HealthState::Down => {
                    if matches!(signal, HealthSignal::Success { .. }) {
                        breaker.observe_from(&signal, &self.scope, through_proxy(&signal), now);
                        self.observe_model(breaker, entry, &signal, name, now);
                    }
                }
                HealthState::Healthy | HealthState::Degraded => {
                    breaker.observe_from(&signal, &self.scope, through_proxy(&signal), now);
                    // An outage of the internet is no model's doing either.
                    if !(breaker.internet.is_down() && may_be_the_network(&signal)) {
                        self.observe_model(breaker, entry, &signal, name, now);
                    }
                }
            }
        });
    }

    /// Fails closed: when `agents.json` can't be read, the agent reads as disabled, since
    /// whether you or its adapter disabled it is unknown.
    fn state(&self, agent_id: &AgentId) -> AgentHealth {
        let file = match self.store.read() {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("the health board could not read agents.json: {err}");
                return AgentHealth {
                    disabled: Some(format!(
                        "could not read {}, so whether {agent_id} is disabled is unknown: {err}. \
                         Fix or remove the file to start new work on it",
                        self.store.path().display()
                    )),
                    ..health_of(agent_id, None)
                };
            }
        };
        match file.entry(agent_id) {
            Some(entry) if self.is_due(entry, self.clock.now()) => self
                .change(agent_id, |breaker, entry, _| {
                    let disabled = entry.health.as_ref().and_then(|h| h.disabled.clone());
                    breaker.health(agent_id, disabled)
                })
                .unwrap_or_else(|| health_of(agent_id, Some(entry))),
            entry => {
                self.publish(&file);
                health_of(agent_id, entry)
            }
        }
    }

    fn all(&self) -> Vec<AgentHealth> {
        self.refresh();
        match self.store.read() {
            Ok(file) => file
                .agent_ids
                .iter()
                .map(|(id, entry)| health_of(&AgentId(id.clone()), Some(entry)))
                .collect(),
            Err(err) => {
                tracing::warn!("the health board could not read agents.json: {err}");
                Vec::new()
            }
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<AgentStatusEvent> {
        self.events.subscribe()
    }

    fn test_result(&self, agent_id: &AgentId, ok: bool) {
        self.change(agent_id, |breaker, _, now| {
            if breaker.state() == HealthState::Testing {
                breaker.test_result(ok, now);
            } else {
                tracing::debug!("a test result for {agent_id} came while it wasn't testing");
            }
        });
    }

    fn model(&self, model: &ModelRef) -> ModelHealth {
        let name = self.kept_as(&model.agent, model.model.as_deref());
        self.models(&model.agent_id())
            .into_iter()
            .find(|health| health.model.model.as_deref().unwrap_or_default() == name)
            .unwrap_or_else(|| ModelHealth::healthy(model))
    }

    fn network(&self) -> NetState {
        self.network.state()
    }

    fn network_changes(&self) -> watch::Receiver<NetState> {
        self.network.changes()
    }

    fn check_network(&self) -> BoxFuture<'_, NetState> {
        Box::pin(self.network.settled())
    }

    fn models(&self, agent_id: &AgentId) -> Vec<ModelHealth> {
        // Reading the agent first moves along whatever is due, its models with it.
        let _ = self.state(agent_id);
        match self.store.read() {
            Ok(file) => file
                .entry(agent_id)
                .map(|entry| {
                    entry
                        .models
                        .iter()
                        .map(|(name, model)| model.health(agent_id, name))
                        .collect()
                })
                .unwrap_or_default(),
            Err(err) => {
                tracing::warn!("the health board could not read agents.json: {err}");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::base::names::SessionKey;
    use argonauts_core::models::limits::{
        PlanUsageReading, PlanUsageSource, Trouble, takes_fallbacks,
    };
    use time::Duration as Delta;

    fn settings() -> HealthSettings {
        HealthSettings {
            degraded_after: 2,
            down_after: 3,
            down_sessions: 2,
            window: Span::new("2m"),
            cooldown: Span::new("5m"),
        }
    }

    fn codex() -> AgentId {
        AgentId("codex".into())
    }

    fn retry(session: u32, reason: RetryReason) -> HealthSignal {
        HealthSignal::Retry {
            agent_id: codex(),
            model: None,
            session: SessionKey::numbered(session),
            reason,
        }
    }

    fn error(session: u32, category: ErrorCategory) -> HealthSignal {
        HealthSignal::Error {
            agent_id: codex(),
            model: None,
            session: SessionKey::numbered(session),
            category,
        }
    }

    fn success(session: u32) -> HealthSignal {
        HealthSignal::Success {
            agent_id: codex(),
            model: None,
            session: SessionKey::numbered(session),
        }
    }

    fn t0() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    /// A session that finished its work while its CLI was down is as good as the test task:
    /// the CLI is back at once, rather than after the cool-down.
    #[test]
    fn a_success_on_a_cli_marked_down_ends_its_down_state() {
        let mut breaker = Breaker::new(&settings());
        for session in 1..=3 {
            breaker.observe(&error(session, ErrorCategory::Network), t0());
        }
        assert_eq!(breaker.state(), HealthState::Down);
        assert_eq!(
            breaker.observe(&success(4), t0()),
            Some(HealthState::Healthy)
        );
        assert_eq!(breaker.reason(), Some(TEST_TASK_SUCCEEDED));
        assert!(breaker.marks().is_empty());
        assert_eq!(breaker.next_test_at(), None);
    }

    /// Sessions that go out through the sandbox's proxy fail on the network while the CLI's
    /// direct sessions finish their work: the proxy's trouble doesn't take the CLI down. With
    /// no direct success in the window, the same errors do.
    #[test]
    fn proxied_network_errors_dont_take_a_cli_down_while_direct_sessions_work() {
        let mut breaker = Breaker::new(&settings());
        breaker.observe_from(&success(1), "", false, t0());
        for session in 2..=4 {
            breaker.observe_from(&error(session, ErrorCategory::Network), "", true, t0());
        }
        assert_ne!(breaker.state(), HealthState::Down);
        assert!(breaker.marks().iter().all(|mark| mark.proxied));

        let mut later = Breaker::new(&settings());
        later.observe_from(&success(1), "", false, t0());
        let past_the_window = t0() + Delta::minutes(3);
        for session in 2..=4 {
            later.observe_from(
                &error(session, ErrorCategory::Network),
                "",
                true,
                past_the_window,
            );
        }
        assert_eq!(later.state(), HealthState::Down);

        // A proxied session's success says nothing of the agent's own network.
        let mut proxied_only = Breaker::new(&settings());
        proxied_only.observe_from(&success(1), "", true, t0());
        for session in 2..=4 {
            proxied_only.observe_from(&error(session, ErrorCategory::Network), "", true, t0());
        }
        assert_eq!(proxied_only.state(), HealthState::Down);
    }

    #[derive(Default)]
    struct ManualClock(Mutex<Option<OffsetDateTime>>);

    impl ManualClock {
        fn at(time: OffsetDateTime) -> Arc<Self> {
            Arc::new(Self(Mutex::new(Some(time))))
        }
        fn advance(&self, by: Delta) {
            let mut now = self.0.lock().unwrap();
            *now = Some(now.unwrap() + by);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> OffsetDateTime {
            self.0.lock().unwrap().unwrap()
        }
    }

    #[test]
    fn circuit_breaker_states() {
        let mut breaker = Breaker::new(&settings());
        let now = t0();
        assert_eq!(breaker.state(), HealthState::Healthy);

        // Account problems say nothing about the service.
        assert_eq!(breaker.observe(&error(1, ErrorCategory::Auth), now), None);
        assert_eq!(
            breaker.observe(&error(1, ErrorCategory::UsageLimit), now),
            None
        );
        assert_eq!(breaker.observe(&retry(1, RetryReason::Network), now), None);
        assert_eq!(
            breaker.observe(&retry(1, RetryReason::RateLimit), now),
            Some(HealthState::Degraded)
        );
        assert_eq!(breaker.reason(), Some("2 errors or retries in 2m"));

        // The errors stop: the window passes and a success brings it back.
        let later = now + Delta::minutes(3);
        assert_eq!(
            breaker.observe(&success(1), later),
            Some(HealthState::Healthy)
        );

        // Overloads are their model's own business: on however many sessions, the agent is
        // only degraded.
        for session in 1..=4 {
            breaker.observe(&error(session, ErrorCategory::Overloaded), later);
        }
        assert_eq!(breaker.state(), HealthState::Degraded);
        // Network errors are no model's doing. On one session they don't take it down, however
        // many.
        for _ in 0..4 {
            breaker.observe(&error(1, ErrorCategory::Network), later);
        }
        assert_eq!(breaker.state(), HealthState::Degraded);
        // A second session makes it an outage.
        assert_eq!(
            breaker.observe(&retry(2, RetryReason::Network), later),
            Some(HealthState::Down)
        );
        assert_eq!(breaker.reason(), Some("network errors on 2 sessions in 2m"));
        let test_at = later + Delta::minutes(5);
        assert_eq!(breaker.next_test_at(), Some(test_at));
        // Paused tasks' errors don't count while it's down.
        assert_eq!(
            breaker.observe(&error(3, ErrorCategory::Server), later),
            None
        );

        assert_eq!(breaker.tick(test_at - Delta::seconds(1)), None);
        assert_eq!(breaker.tick(test_at), Some(HealthState::Testing));
        assert_eq!(breaker.left_down_at(), Some(test_at));
        assert_eq!(breaker.next_test_at(), None);
        assert_eq!(breaker.test_result(true, test_at), HealthState::Healthy);
        assert!(breaker.marks().is_empty());

        // Down again, and this time the test task fails.
        for session in 1..=3 {
            breaker.observe(&error(session, ErrorCategory::Timeout), test_at);
        }
        assert_eq!(breaker.state(), HealthState::Down);
        let test_at = test_at + Delta::minutes(5);
        assert_eq!(breaker.tick(test_at), Some(HealthState::Testing));
        assert_eq!(breaker.test_result(false, test_at), HealthState::Down);
        assert_eq!(breaker.reason(), Some("the test task failed"));
        assert_eq!(breaker.next_test_at(), Some(test_at + Delta::minutes(5)));

        // A reported reset replaces the cool-down.
        let reset = test_at + Delta::minutes(30);
        breaker.set_reported_reset(Some(reset));
        assert_eq!(breaker.next_test_at(), Some(reset));
        assert_eq!(breaker.tick(test_at + Delta::minutes(10)), None);
        assert_eq!(breaker.tick(reset), Some(HealthState::Testing));
        assert_eq!(breaker.reason(), Some("the reported reset has passed"));
        // A result outside Testing changes nothing.
        assert_eq!(breaker.test_result(true, reset), HealthState::Healthy);
        assert_eq!(breaker.test_result(false, reset), HealthState::Healthy);
    }

    #[test]
    fn odd_settings_still_work() {
        let mut breaker = Breaker::new(&HealthSettings {
            degraded_after: 0,
            down_after: 0,
            down_sessions: 0,
            window: Span::new("none"),
            cooldown: Span::new("soon"),
        });
        let now = t0();
        assert_eq!(
            breaker.observe(&error(1, ErrorCategory::Network), now),
            Some(HealthState::Down)
        );
        // One session is counted as one.
        assert_eq!(
            breaker.reason(),
            Some("network errors on 1 session in none")
        );
        // An unreadable cool-down uses the default one.
        assert_eq!(breaker.next_test_at(), Some(now + Delta::minutes(5)));

        let mut breaker = Breaker::new(&HealthSettings {
            cooldown: Span::new("none"),
            window: Span::new("none"),
            ..settings()
        });
        for session in 1..=3 {
            breaker.observe(&error(session, ErrorCategory::Network), now);
        }
        assert_eq!(breaker.tick(now), Some(HealthState::Testing));
    }

    fn board(home: &std::path::Path, clock: &Arc<ManualClock>) -> FileHealthBoard {
        FileHealthBoard::new(&Paths::new(home), &settings()).with_clock(clock.clone())
    }

    fn drain(events: &mut broadcast::Receiver<AgentStatusEvent>) -> Vec<AgentStatusEvent> {
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    fn states(events: &[AgentStatusEvent]) -> Vec<HealthState> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentStatusEvent::Health(health) => Some(health.state),
                _ => None,
            })
            .collect()
    }

    /// The board, unlike a bare breaker, used to drop every signal while the CLI was down, so
    /// a CLI marked down stayed down while its sessions answered.
    #[test]
    fn a_success_ends_a_down_state_for_every_process() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let app = board(home.path(), &clock);
        let run_host = board(home.path(), &clock);
        let mut events = app.subscribe();

        for session in 1..=3 {
            run_host.observe(error(session, ErrorCategory::Network));
        }
        assert_eq!(app.state(&codex()).state, HealthState::Down);
        drain(&mut events);

        run_host.observe(success(9));
        let health = app.state(&codex());
        assert_eq!(health.state, HealthState::Healthy);
        assert_eq!(health.reason.as_deref(), Some(TEST_TASK_SUCCEEDED));
        assert_eq!(health.next_test_at, None);
        assert_eq!(states(&drain(&mut events)), vec![HealthState::Healthy]);
        let file = app.store().read().unwrap();
        assert_eq!(file.agent_ids["codex"].test_task, None);
    }

    #[test]
    fn health_changes_are_broadcast() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let app = board(home.path(), &clock);
        let run_host = board(home.path(), &clock);
        let mut events = app.subscribe();

        assert_eq!(app.state(&codex()).state, HealthState::Healthy);
        assert!(app.all().is_empty());
        app.observe(retry(1, RetryReason::Network));
        assert!(drain(&mut events).is_empty());
        app.observe(retry(1, RetryReason::Network));
        assert_eq!(states(&drain(&mut events)), vec![HealthState::Degraded]);

        // Another process's signals count with ours, and its change reaches our subscribers.
        run_host.observe(error(2, ErrorCategory::Network));
        assert_eq!(run_host.state(&codex()).state, HealthState::Down);
        assert!(drain(&mut events).is_empty());
        app.refresh();
        assert_eq!(states(&drain(&mut events)), vec![HealthState::Down]);

        clock.advance(Delta::minutes(5));
        assert_eq!(app.state(&codex()).state, HealthState::Testing);
        assert_eq!(states(&drain(&mut events)), vec![HealthState::Testing]);
        assert!(app.claim_test_task(&codex(), "s-004"));
        assert!(!run_host.claim_test_task(&codex(), "s-005"));
        assert_eq!(
            app.state(&codex()).reason.as_deref(),
            Some("a test task is running")
        );
        app.test_result(&codex(), true);
        assert_eq!(
            states(&drain(&mut events)),
            vec![HealthState::Testing, HealthState::Healthy]
        );
        assert_eq!(
            app.store().read().unwrap().agent_ids["codex"].test_task,
            None
        );

        // A broken adapter disables the agent for new tasks, with a pin suggestion.
        run_host.observe(HealthSignal::AdapterBroken {
            agent_id: codex(),
            detail: "unknown events piled up".into(),
        });
        app.refresh();
        let events = drain(&mut events);
        let Some(AgentStatusEvent::Disabled { agent_id, reason }) = events.first() else {
            panic!("expected a disable, got {events:?}");
        };
        assert_eq!(agent_id, &codex());
        assert!(reason.starts_with("unknown events piled up. New tasks go to other agents."));
        assert!(reason.contains("agents.codex.version_pin"));
        let health = app.state(&codex());
        assert_eq!(health.state, HealthState::Healthy);
        assert!(health.disabled.is_some());

        app.enable(&codex());
        assert_eq!(app.state(&codex()).disabled, None);
        assert_eq!(app.all().len(), 1);
    }

    #[test]
    fn wake_fields_recorded() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        let waiting_at = t0();
        for session in 1..=3 {
            board.observe(error(session, ErrorCategory::Network));
        }
        let store = board.store();
        let file = store.read().unwrap();
        let entry = &file.agent_ids["codex"];
        let health = entry.health.as_ref().unwrap();
        assert_eq!(health.state, HealthState::Down);
        let test_at = t0() + Delta::minutes(5);
        assert_eq!(health.next_test_at, Some(test_at));
        assert_eq!(entry.left_down_at, None);
        assert_eq!(file.earliest_reset(), Some(test_at));
        // With no run host running, the app still sees the agent is due back.
        assert!(!file.agent_back(&codex(), waiting_at, t0()));
        assert!(file.agent_back(&codex(), waiting_at, test_at));

        // A reported plan usage reset later than the cool-down moves the test.
        let reset = t0() + Delta::hours(1);
        store
            .update(|file| {
                file.agent_ids.get_mut("codex").unwrap().plan_usage = Some(PlanUsageReading {
                    agent_id: codex(),
                    status: PlanUsageStatus::Rejected,
                    used: Some(1.0),
                    resets_at: Some(reset),
                    window: Some("five_hour".into()),
                    source: PlanUsageSource::AppServer,
                    observed_at: t0(),
                    detail: None,
                    wrap_up: false,
                })
            })
            .unwrap();
        clock.advance(Delta::minutes(5));
        assert_eq!(board.state(&codex()).state, HealthState::Down);
        clock.advance(Delta::minutes(55));
        assert_eq!(board.state(&codex()).state, HealthState::Testing);

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        let codex_raw = &raw["agent_ids"]["codex"];
        assert_eq!(codex_raw["health"]["state"], "testing");
        assert_eq!(codex_raw["left_down_at"], "2027-01-15T09:00:00Z");
        assert_eq!(codex_raw["plan_usage"]["resets_at"], "2027-01-15T09:00:00Z");
        let file = store.read().unwrap();
        assert!(file.agent_back(&codex(), waiting_at, waiting_at));
        assert_eq!(file.earliest_reset(), Some(reset));
    }

    #[test]
    fn an_unreadable_board_disables_new_work() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        assert!(board.state(&codex()).disabled.is_none());
        std::fs::write(board.store().path(), "{ not json").unwrap();
        let health = board.state(&codex());
        assert!(health.disabled.unwrap().contains("agents.json"));
    }

    fn take_down(board: &FileHealthBoard) {
        for session in 1..=3 {
            board.observe(error(session, ErrorCategory::Network));
        }
        assert_eq!(board.state(&codex()).state, HealthState::Down);
    }

    fn test_task(board: &FileHealthBoard) -> Option<String> {
        let file = board.store().read().unwrap();
        file.agent_ids["codex"].test_task.clone()?.session
    }

    #[test]
    fn one_test_task_decides() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        take_down(&board);
        clock.advance(Delta::minutes(5));
        assert_eq!(board.state(&codex()).state, HealthState::Testing);

        // The first session to report takes the test task, and no one else can.
        board.observe(retry(4, RetryReason::Overloaded));
        assert_eq!(test_task(&board).as_deref(), Some("s-004"));
        assert_eq!(
            board.state(&codex()).reason.as_deref(),
            Some("a test task is running")
        );
        assert!(!board.claim_test_task(&codex(), "s-005"));
        board.observe(error(5, ErrorCategory::Server));
        board.observe(success(5));
        assert_eq!(board.state(&codex()).state, HealthState::Testing);
        board.observe(success(4));
        assert_eq!(board.state(&codex()).state, HealthState::Healthy);
        assert_eq!(test_task(&board), None);

        // Under test again, a new outage takes the task and fails it.
        take_down(&board);
        clock.advance(Delta::minutes(5));
        board.observe(error(6, ErrorCategory::Overloaded));
        let health = board.state(&codex());
        assert_eq!(health.state, HealthState::Down);
        assert_eq!(health.reason.as_deref(), Some("the test task failed"));
        assert_eq!(test_task(&board), None);
    }

    fn of(model: &str, session: u32, category: ErrorCategory) -> HealthSignal {
        HealthSignal::Error {
            agent_id: codex(),
            model: Some(model.to_owned()),
            session: SessionKey::numbered(session),
            category,
        }
    }

    fn worked(model: &str, session: u32) -> HealthSignal {
        HealthSignal::Success {
            agent_id: codex(),
            model: Some(model.to_owned()),
            session: SessionKey::numbered(session),
        }
    }

    fn model(name: &str) -> ModelRef {
        ModelRef::new("codex", Some(name.to_owned()))
    }

    fn model_events(events: &[AgentStatusEvent]) -> Vec<(String, ModelState)> {
        let models = events.iter().filter_map(|event| match event {
            AgentStatusEvent::Model(health) => Some((health.model.to_string(), health.state)),
            _ => None,
        });
        models.collect()
    }

    #[test]
    fn an_overload_takes_its_model_out_and_leaves_the_others_usable() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        let mut events = board.subscribe();

        // The failure and its retry, on one session.
        board.observe(of("big", 1, ErrorCategory::Overloaded));
        assert_eq!(board.model(&model("big")).state, ModelState::Healthy);
        board.observe(of("big", 1, ErrorCategory::Overloaded));
        let big = board.model(&model("big"));
        assert_eq!(big.state, ModelState::Out);
        assert_eq!(big.trouble, Some(Trouble::Overload));
        assert_eq!(big.reason.as_deref(), Some("2 overload errors in a row"));
        assert_eq!(big.next_test_at, Some(t0() + Delta::minutes(5)));
        assert_eq!(
            model_events(&drain(&mut events)),
            [("codex/big".to_owned(), ModelState::Out)]
        );

        // Only that model is out: the agent takes work, and so do its other models.
        assert_ne!(board.state(&codex()).state, HealthState::Down);
        assert_eq!(board.model(&model("small")).state, ModelState::Healthy);
        assert!(takes_fallbacks(&board, &model("small")));
        assert!(!takes_fallbacks(&board, &model("big")));
        assert_eq!(board.models(&codex()).len(), 1);

        // After its rest the model is under test, and the next session on it decides.
        clock.advance(Delta::minutes(5));
        assert_eq!(board.model(&model("big")).state, ModelState::Testing);
        assert!(
            !takes_fallbacks(&board, &model("big")),
            "only work asked of it"
        );
        board.observe(worked("big", 2));
        assert_eq!(board.model(&model("big")).state, ModelState::Healthy);
        let seen = model_events(&drain(&mut events));
        assert_eq!(
            seen.last(),
            Some(&("codex/big".to_owned(), ModelState::Healthy))
        );
    }

    /// A registry that knows Claude Code as its catalog ships it: named alone it runs Opus,
    /// which `opus` names too, beside Sonnet.
    struct ClaudeModels(argonauts_testkit::engine::services::StaticRegistry);

    impl ModelRegistry for ClaudeModels {
        fn get(&self, worker: &ModelRef) -> Option<argonauts_core::models::registry::ModelEntry> {
            self.0.get(worker)
        }
        fn provider(&self, worker: &ModelRef) -> String {
            self.0.provider(worker)
        }
        fn list(&self) -> Vec<argonauts_core::models::registry::ModelEntry> {
            self.0.list()
        }
        fn estimate(
            &self,
            worker: &ModelRef,
            input: u64,
            output: u64,
        ) -> argonauts_core::models::limits::CostEstimate {
            self.0.estimate(worker, input, output)
        }
        fn discover(
            &self,
        ) -> argonauts_core::base::future::BoxFuture<
            '_,
            Vec<argonauts_core::models::registry::ModelEntry>,
        > {
            self.0.discover()
        }
        fn catalog(&self) -> argonauts_core::models::model_catalog::ModelCatalog {
            use argonauts_core::models::model_catalog::{
                CATALOG_VERSION, CatalogEntry, ModelCatalog,
            };
            let mut default = CatalogEntry::new("claude-code".parse().unwrap());
            default.runs = Some("claude-opus-5-5".to_owned());
            let model = |id: &str, alias: &str| {
                let mut entry = CatalogEntry::new(id.parse().unwrap());
                entry.aliases = Some(vec![alias.to_owned()]);
                entry
            };
            ModelCatalog {
                version: CATALOG_VERSION,
                models: vec![
                    default,
                    model("claude-code/claude-opus-5-5", "opus"),
                    model("claude-code/claude-sonnet-5-5", "sonnet"),
                ],
                ..ModelCatalog::default()
            }
        }
    }

    /// One model overloaded is one model out, however its sessions named it: none at all, as
    /// the main session does, by its alias, by its full id, or with a variant in brackets.
    /// Its CLI stays up, and its other models still take what the overloaded one can't.
    #[test]
    fn one_model_named_three_ways_is_one_model_out() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock)
            .naming_models_with(Arc::new(ClaudeModels(Default::default())));
        let claude = AgentId("claude-code".into());
        let overloaded = |model: Option<&str>, session: u32| HealthSignal::Error {
            agent_id: claude.clone(),
            model: model.map(str::to_owned),
            session: SessionKey::numbered(session),
            category: ErrorCategory::Overloaded,
        };
        let spellings = [
            None,
            Some("opus"),
            Some("claude-opus-5-5"),
            Some("claude-opus-5-5[1m]"),
        ];
        for (session, model) in (1..).zip(spellings) {
            for _ in 0..2 {
                board.observe(overloaded(model, session));
            }
        }

        let kept = board.models(&claude);
        assert_eq!(kept.len(), 1, "{kept:#?}");
        assert_eq!(
            kept[0].model,
            "claude-code/claude-opus-5-5".parse().unwrap()
        );
        assert_eq!(kept[0].state, ModelState::Out);
        assert_ne!(board.state(&claude).state, HealthState::Down);
        for spelled in [
            "claude-code",
            "claude-code/opus",
            "claude-code/claude-opus-5-5",
        ] {
            let asked: ModelRef = spelled.parse().unwrap();
            assert_eq!(board.model(&asked).state, ModelState::Out, "{spelled}");
            assert!(!takes_fallbacks(&board, &asked), "{spelled}");
        }
        for spelled in ["claude-code/sonnet", "claude-code/claude-sonnet-5-5"] {
            assert!(
                takes_fallbacks(&board, &spelled.parse().unwrap()),
                "{spelled}"
            );
        }
    }

    #[test]
    fn two_models_out_take_the_agent_down_and_each_comes_back_on_its_own() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        for _ in 0..2 {
            board.observe(of("big", 1, ErrorCategory::Overloaded));
        }
        clock.advance(Delta::seconds(90));
        board.observe(of("small", 2, ErrorCategory::Server));
        assert_ne!(board.state(&codex()).state, HealthState::Down);
        let mut events = board.subscribe();
        board.observe(of("small", 2, ErrorCategory::Server));

        // The second model's own trouble is said before its agent's, which it explains.
        let said = drain(&mut events);
        let small_out = said.iter().position(|event| {
            matches!(event, AgentStatusEvent::Model(health) if health.state == ModelState::Out)
        });
        let agent_down = said.iter().position(|event| {
            matches!(event, AgentStatusEvent::Health(health) if health.state == HealthState::Down)
        });
        assert!(small_out.is_some() && small_out < agent_down, "{said:?}");

        let down = board.state(&codex());
        assert_eq!(down.state, HealthState::Down);
        assert_eq!(down.reason.as_deref(), Some(MODELS_OUT));
        let small = board.model(&model("small"));
        assert_eq!(small.trouble, Some(Trouble::ServerErrors));
        assert_eq!(small.reason.as_deref(), Some("2 server errors in a row"));
        // A model nothing was said against is no fallback either, its agent being down.
        assert!(!takes_fallbacks(&board, &model("tiny")));

        // The agent's test passes on one model: that model is back with it, the other waits
        // for a test of its own.
        clock.advance(Delta::minutes(5));
        assert_eq!(board.state(&codex()).state, HealthState::Testing);
        board.observe(worked("small", 3));
        assert_eq!(board.state(&codex()).state, HealthState::Healthy);
        assert_eq!(board.model(&model("small")).state, ModelState::Healthy);
        assert_eq!(board.model(&model("big")).state, ModelState::Testing);
        board.observe(of("big", 4, ErrorCategory::Overloaded));
        let big = board.model(&model("big"));
        assert_eq!(big.state, ModelState::Out);
        assert_eq!(big.reason.as_deref(), Some("the test task failed"));
        // One model out again is no outage of the agent.
        assert_eq!(board.state(&codex()).state, HealthState::Healthy);
    }

    #[test]
    fn network_errors_count_against_the_agent_and_take_no_model_out() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        board.observe(of("big", 1, ErrorCategory::Network));
        board.observe(of("big", 1, ErrorCategory::Timeout));
        assert_eq!(board.state(&codex()).state, HealthState::Degraded);
        let big = board.model(&model("big"));
        assert_eq!(big.state, ModelState::Degraded);
        assert_eq!(
            big.reason.as_deref(),
            Some("reconnected twice in 2 minutes")
        );
        assert_eq!(big.next_test_at, Some(t0() + Delta::minutes(2)));
        // Repeated on a second session, they take the whole agent down.
        board.observe(of("small", 2, ErrorCategory::Network));
        assert_eq!(board.state(&codex()).state, HealthState::Down);
        assert_eq!(board.model(&model("big")).state, ModelState::Degraded);
        assert_eq!(board.model(&model("small")).state, ModelState::Healthy);
    }

    #[test]
    fn a_sign_in_or_billing_error_is_held_against_no_model() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        for category in [
            ErrorCategory::Auth,
            ErrorCategory::UsageLimit,
            ErrorCategory::Billing,
        ] {
            board.observe(of("big", 1, category));
            board.observe(of("big", 1, category));
        }
        assert!(board.models(&codex()).is_empty());
        assert_eq!(board.model(&model("big")).state, ModelState::Healthy);
    }

    #[test]
    fn testing_without_a_result_falls_back_to_down() {
        let mut breaker = Breaker::new(&settings());
        for session in 1..=3 {
            breaker.observe(&error(session, ErrorCategory::Network), t0());
        }
        let test_at = t0() + Delta::minutes(5);
        assert_eq!(breaker.tick(test_at), Some(HealthState::Testing));
        let claimed_at = test_at + Delta::minutes(20);
        assert!(breaker.start_test(claimed_at));
        assert!(!breaker.start_test(claimed_at), "one test task at a time");
        // The limit runs from the claim.
        let limit = claimed_at + TEST_TASK_LIMIT;
        assert_eq!(breaker.tick(limit - Delta::seconds(1)), None);
        assert_eq!(breaker.tick(limit), Some(HealthState::Down));
        assert_eq!(
            breaker.reason(),
            Some("no test task finished within 60 minutes")
        );
        assert_eq!(breaker.next_test_at(), Some(limit + Delta::minutes(5)));
        assert!(!breaker.testing_task());
        assert!(!breaker.start_test(limit), "only Testing takes a test task");

        // On the board, a test task that never reports lets go of the agent too.
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        take_down(&board);
        clock.advance(Delta::minutes(5));
        assert!(board.claim_test_task(&codex(), "s-004"));
        clock.advance(Delta::try_from(TEST_TASK_LIMIT).unwrap());
        board.refresh();
        assert_eq!(board.state(&codex()).state, HealthState::Down);
        assert_eq!(test_task(&board), None);
        clock.advance(Delta::minutes(5));
        board.observe(error(7, ErrorCategory::Server));
        assert_eq!(board.state(&codex()).state, HealthState::Down);
    }

    #[test]
    fn billing_cap_disables_api_key_models() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = board(home.path(), &clock);
        let mut events = board.subscribe();
        let billing = || error(1, ErrorCategory::Billing);

        board.observe(billing());
        let health = board.state(&codex());
        assert_eq!(health.state, HealthState::Healthy);
        let reason = health.disabled.expect("a billing disable");
        assert_eq!(disable_scope(&reason), DisableScope::ApiKey);
        assert!(matches!(
            drain(&mut events).first(),
            Some(AgentStatusEvent::Disabled { .. })
        ));
        let file = board.store().read().unwrap();
        assert!(
            file.agent_ids["codex"].disabled_by.is_some(),
            "owned by this process"
        );

        // A session that works lifts it, such as the retry after you top up.
        board.observe(success(1));
        assert_eq!(board.state(&codex()).disabled, None);
        assert_eq!(
            board.store().read().unwrap().agent_ids["codex"].disabled_by,
            None
        );

        // So does `enable`.
        board.observe(billing());
        board.enable(&codex());
        assert_eq!(board.state(&codex()).disabled, None);

        // A billing cap never narrows a broken adapter's disable, and a success keeps it.
        board.observe(HealthSignal::AdapterBroken {
            agent_id: codex(),
            detail: "the startup check failed".into(),
        });
        board.observe(billing());
        board.observe(success(2));
        let reason = board.state(&codex()).disabled.expect("still disabled");
        assert_eq!(disable_scope(&reason), DisableScope::Every);
    }

    /// Always answers the same.
    struct Answers(bool);

    impl crate::boards::network::Probe for Answers {
        fn answers(&self) -> BoxFuture<'_, bool> {
            let answer = self.0;
            Box::pin(async move { answer })
        }
    }

    fn offline_board(home: &std::path::Path, clock: &Arc<ManualClock>) -> FileHealthBoard {
        board(home, clock).with_network(Network::probing(Arc::new(Answers(false))))
    }

    async fn network_down(board: &FileHealthBoard) {
        let mut changes = board.network_changes();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            changes.wait_for(|state| state.is_down()),
        )
        .await
        .expect("the network went down")
        .unwrap();
    }

    #[tokio::test]
    async fn a_network_error_checks_the_internet_and_a_providers_answer_brings_it_back() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = offline_board(home.path(), &clock);
        assert_eq!(board.network(), NetState::Up);
        board.observe(retry(1, RetryReason::Network));
        network_down(&board).await;
        assert_eq!(board.network(), NetState::Down { since: t0() });
        // A model on this machine works offline, so its answer says nothing of the internet.
        board.observe(HealthSignal::Success {
            agent_id: AgentId("opencode".to_owned()),
            model: Some("ollama/qwen3-coder:30b".to_owned()),
            session: SessionKey::numbered(2),
        });
        assert!(board.network().is_down());
        board.observe(success(3));
        assert_eq!(board.network(), NetState::Up);
    }

    #[tokio::test]
    async fn network_errors_while_offline_never_take_an_agent_down() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board = offline_board(home.path(), &clock);
        board.observe(retry(1, RetryReason::Network));
        network_down(&board).await;
        for session in 1..=4 {
            board.observe(retry(session, RetryReason::Network));
            board.observe(error(session, ErrorCategory::Network));
            board.observe(error(session, ErrorCategory::Timeout));
            // An offline CLI's failure often reads as nothing anyone understood.
            board.observe(error(session, ErrorCategory::Unknown));
        }
        let health = board.state(&codex());
        assert_eq!(health.state, HealthState::Healthy, "{:?}", health.reason);
        let models = board.models(&codex());
        assert!(
            models
                .iter()
                .all(|model| model.state == ModelState::Healthy),
            "{models:?}"
        );
    }

    #[tokio::test]
    async fn network_errors_count_once_the_internet_is_confirmed_up() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let board =
            board(home.path(), &clock).with_network(Network::probing(Arc::new(Answers(true))));
        // Before the check has said anything, the error blames nobody.
        board.observe(error(1, ErrorCategory::Network));
        assert_eq!(board.state(&codex()).state, HealthState::Healthy);
        board.check_network().await;
        // The internet answered after these, so they are the provider's own outage.
        board.observe(error(1, ErrorCategory::Network));
        board.observe(error(2, ErrorCategory::Network));
        let health = board.state(&codex());
        assert_eq!(health.state, HealthState::Down);
        assert_eq!(
            health.reason.as_deref(),
            Some("network errors on 2 sessions in 2m")
        );
    }

    #[tokio::test]
    async fn a_burst_of_network_errors_counts_once_a_check_after_its_last_finds_the_internet_up() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        let pace = crate::boards::network::Pace {
            at_most_every: std::time::Duration::from_millis(300),
            ..crate::boards::network::Pace::default()
        };
        let network = Network::probing(Arc::new(Answers(true))).with_pace(pace);
        let board = board(home.path(), &clock).with_network(network);
        board.observe(error(1, ErrorCategory::Network));
        board.check_network().await;
        // Two more, after the check that just found the internet up.
        clock.advance(Delta::seconds(1));
        board.observe(error(1, ErrorCategory::Network));
        clock.advance(Delta::seconds(1));
        board.observe(error(2, ErrorCategory::Network));
        assert_eq!(board.state(&codex()).state, HealthState::Healthy);
        // One more check comes once the pace allows, after them all.
        board.check_network().await;
        let health = board.state(&codex());
        assert_eq!(health.state, HealthState::Down);
        assert_eq!(
            health.reason.as_deref(),
            Some("network errors on 2 sessions in 2m")
        );
    }

    #[tokio::test]
    async fn a_test_tasks_network_error_while_offline_is_no_verdict() {
        let home = tempfile::tempdir().unwrap();
        let clock = ManualClock::at(t0());
        take_down(&board(home.path(), &clock));
        clock.advance(Delta::minutes(5));
        let board = offline_board(home.path(), &clock);
        assert_eq!(board.state(&codex()).state, HealthState::Testing);
        board.observe(retry(4, RetryReason::Network));
        network_down(&board).await;
        assert_eq!(test_task(&board).as_deref(), Some("s-004"));
        board.observe(error(4, ErrorCategory::Network));
        assert_eq!(board.state(&codex()).state, HealthState::Testing);
        board.observe(success(4));
        assert_eq!(board.state(&codex()).state, HealthState::Healthy);
    }
}
