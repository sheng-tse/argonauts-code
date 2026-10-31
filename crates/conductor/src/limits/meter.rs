//! Each session's meter, around core's `SpendMeter`: spend in dollars, tokens, plan usage, or
//! minutes, tool calls with subagents' included where visible, and how full the context is. It
//! says when to send the wrap-up message, when to stop, and when to rotate.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::agents::session_runner::{SessionLimits, SessionSpec};
use argonauts_core::base::names::ModelRef;
use argonauts_core::models::limits::{CostEstimate, PlanUsageReading};
use argonauts_core::models::registry::{CostUnit, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::settings::endpoints::{
    endpoint_settings, host_and_port, local_endpoint_addresses,
};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::{
    AgentEvent, Capabilities, LimitDimension, MeterAction, MeterLimit, Spend, SpendMeter,
    SpendStatus, TokenPrice, WorkLimits, builtin_price,
};
use time::OffsetDateTime;

/// How far above the meter's dollar limit the CLI's own cap sits, so the meter always acts
/// first.
pub const NATIVE_CAP_MARGIN: f64 = 1.1;
/// The smallest native cap passed, so the flag stays valid when nothing is left.
const MIN_NATIVE_CAP_USD: f64 = 0.01;

/// A plan usage reading at most this, and at least this much below the last one, means the window
/// reset.
const PLAN_USAGE_NEAR_ZERO: f64 = 0.05;
/// `resets_at` moving by less than this is jitter from agents that report seconds until the
/// reset.
const RESET_JITTER: time::Duration = time::Duration::minutes(1);

/// Where the meter's dollar prices come from.
#[derive(Clone)]
pub enum Pricing {
    /// `ModelRegistry::estimate` for this model.
    Registry {
        registry: Arc<dyn ModelRegistry>,
        worker: ModelRef,
    },
    /// `agents.<id>.price`, else 0.0.1's `builtin_price`.
    Price(TokenPrice),
    /// Only what the agent reports as cost.
    ReportedOnly,
}

/// The registry's price when there is one, then `agents.<id>.price`, then `builtin_price`, then
/// free for a model on one of your local servers. `served_from` is where the CLI's own config
/// sends its model calls, for one that takes its provider only from there.
pub fn pricing(
    worker: &ModelRef,
    settings: &Settings,
    registry: Option<&Arc<dyn ModelRegistry>>,
    served_from: Option<&str>,
) -> Pricing {
    if let Some(registry) = registry
        && registry_price(registry.as_ref(), worker).is_some()
    {
        return Pricing::Registry {
            registry: Arc::clone(registry),
            worker: worker.clone(),
        };
    }
    let agent = settings.agents.get(&worker.agent);
    if let Some(price) = agent.and_then(|agent| agent.price) {
        return Pricing::Price(price);
    }
    let model = worker
        .model
        .as_deref()
        .or_else(|| agent.and_then(|agent| agent.model.as_deref()));
    if let Some(price) = model.and_then(builtin_price) {
        return Pricing::Price(price);
    }
    if runs_locally(worker, model, settings, registry, served_from) {
        return Pricing::Price(FREE);
    }
    Pricing::ReportedOnly
}

const FREE: TokenPrice = TokenPrice {
    input_per_mtok: 0.0,
    output_per_mtok: 0.0,
    cached_input_per_mtok: None,
};

/// Whether the model runs on a server of yours: its route names a local endpoint, as
/// `ollama/llama3.2` does for OpenCode, its registry entry runs Codex on a local provider, or the
/// CLI's own config points at a local endpoint or this machine.
fn runs_locally(
    worker: &ModelRef,
    model: Option<&str>,
    settings: &Settings,
    registry: Option<&Arc<dyn ModelRegistry>>,
    served_from: Option<&str>,
) -> bool {
    let routed = model
        .and_then(|model| model.split_once('/'))
        .and_then(|(provider, _)| endpoint_settings(settings, provider))
        .is_some_and(|endpoint| endpoint.local);
    let launched = registry
        .and_then(|registry| registry.get(worker))
        .and_then(|entry| entry.launch)
        .is_some_and(|launch| launch.oss || launch.local_provider.is_some());
    let configured = served_from
        .and_then(host_and_port)
        .is_some_and(|(host, port)| {
            local_endpoint_addresses(settings).contains(&(host.clone(), port))
                || host == "localhost"
                || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        });
    routed || launched || configured
}

/// The registry's pinned dollar prices for `worker`, or zero for a free model. `None` for a
/// model paid by plan usage, or one it doesn't know.
fn registry_price(registry: &dyn ModelRegistry, worker: &ModelRef) -> Option<TokenPrice> {
    let cost = registry.get(worker)?.cost;
    match cost.unit {
        CostUnit::Usd => Some(TokenPrice {
            input_per_mtok: cost.input_per_mtok?,
            output_per_mtok: cost.output_per_mtok?,
            cached_input_per_mtok: cost.cached_input_per_mtok,
        }),
        CostUnit::None => Some(FREE),
        CostUnit::PlanUsage => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeterVerdict {
    Continue,
    /// A limit reached `soft_stop`: send the fixed wrap-up message, once.
    WrapUp(LimitDimension),
    /// A limit reached 100%: interrupt, then stop. The end reason is `LimitHit`.
    Stop(LimitDimension),
    /// The context reached `sessions.rotate_at`: end the session so the engine starts a fresh
    /// one with a fallback note.
    Rotate {
        used: u64,
        cap: u64,
    },
}

/// Everything a meter needs when its session starts.
#[derive(Clone)]
pub struct MeterSetup {
    pub limits: SessionLimits,
    pub pricing: Pricing,
    /// Whether usage is cumulative and whether a resume's totals include earlier turns.
    pub capabilities: Capabilities,
    /// `sessions.rotate_at`, which the meter reads against the window as the stream last
    /// reported it. `None` never rotates.
    pub rotate_at: Option<RotateAt>,
    /// The registry's context window, used when the stream reports none.
    pub context_window: Option<u64>,
    /// `sessions.context` in tokens when it lowers the window: the session is held to it
    /// whatever window its CLI reports.
    pub context_cap: Option<u64>,
}

/// Tokens in the context and the window's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextSize {
    pub used: u64,
    pub size: Option<u64>,
}

/// With limits off, spend never stops a session, but the wall-clock and tool-call limits still
/// do, since they are watchdogs.
pub struct SessionMeter {
    inner: SpendMeter,
    /// A plan usage limit, as a share of the subscription window.
    plan_usage_limit: Option<f64>,
    plan_usage: PlanUsageTally,
    /// Prices usage the agent sent without cost, when the registry has no pinned price.
    estimator: Option<(Arc<dyn ModelRegistry>, ModelRef)>,
    /// Set when no price is known yet, so the model the stream reports can supply one.
    price_from_model: bool,
    /// The model runs on a server of yours, or the registry says it costs nothing, so no
    /// figure the parser prices at a provider's rates counts.
    free: bool,
    capabilities: Capabilities,
    rotate_at: Option<RotateAt>,
    context_window: Option<u64>,
    context_cap: Option<u64>,
    context: Option<ContextSize>,
    /// What earlier sessions spent on the CLI session this one resumes.
    earlier: Spend,
    /// What the CLI session had spent in all when its current process started, to which what
    /// a process says of its own spend adds.
    before_process: f64,
    /// What was left of the run's limit as the session started, against which only its own
    /// spend counts.
    run_left: Option<LimitAmount>,
    /// The whole run time at the latest tick.
    elapsed: Duration,
    /// The run time when the CLI session last resumed in a new process. Core's meter counts
    /// each process's time from zero.
    resumed_at: Duration,
    wrapped: bool,
    rotated: bool,
}

impl SessionMeter {
    pub fn new(setup: MeterSetup) -> Self {
        let MeterSetup {
            limits,
            pricing,
            capabilities,
            rotate_at,
            context_window,
            context_cap,
        } = setup;
        // With no limit the meter still counts and reports, and never stops the session.
        let cap = limits.limit.filter(|limit| limit.amount.is_finite());
        let (limit, plan_usage_limit, limit_minutes) = match cap {
            Some(LimitAmount { unit, amount }) => match unit {
                LimitUnit::Usd => (MeterLimit::Usd(amount), None, None),
                LimitUnit::Tokens => (
                    MeterLimit::Tokens(amount.max(0.0).round() as u64),
                    None,
                    None,
                ),
                LimitUnit::PlanUsage => (MeterLimit::None, Some(amount), None),
                LimitUnit::Minutes => (MeterLimit::None, None, Some(amount)),
            },
            None => (MeterLimit::None, None, None),
        };
        let max_minutes = match (limits.max_minutes, limit_minutes) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let (price, estimator) = match pricing {
            Pricing::Registry { registry, worker } => {
                match registry_price(registry.as_ref(), &worker) {
                    Some(price) => (Some(price), None),
                    None => (None, Some((registry, worker))),
                }
            }
            Pricing::Price(price) => (Some(price), None),
            Pricing::ReportedOnly => (None, None),
        };
        let inner = SpendMeter::new(limit, limits.soft_stop)
            .with_price(price)
            .with_work_limits(WorkLimits {
                max_minutes,
                max_tool_calls: limits.max_tool_calls,
            });
        Self {
            inner,
            plan_usage_limit,
            plan_usage: PlanUsageTally::default(),
            price_from_model: price.is_none() && estimator.is_none(),
            free: price == Some(FREE),
            estimator,
            capabilities,
            rotate_at,
            context_window,
            context_cap,
            context: None,
            earlier: Spend::default(),
            before_process: 0.0,
            run_left: limits.run_left.filter(|left| {
                left.amount.is_finite() && matches!(left.unit, LimitUnit::Usd | LimitUnit::Tokens)
            }),
            elapsed: Duration::ZERO,
            resumed_at: Duration::ZERO,
            wrapped: false,
            rotated: false,
        }
    }

    pub fn observe(&mut self, event: &AgentEvent) -> MeterVerdict {
        match event {
            AgentEvent::Usage { .. } => {
                let usage = self.priced(self.without_earlier_turns(event));
                self.inner.observe(&usage);
            }
            AgentEvent::ContextFill { used, size, .. } => {
                self.context = Some(ContextSize {
                    used: *used,
                    size: self.held(size.or(self.context_window)),
                });
            }
            AgentEvent::PlanUsage { reading } => self.plan_usage.observe(reading),
            AgentEvent::Init {
                model: Some(model), ..
            } => self.learn_price(model),
            other => {
                self.inner.observe(other);
            }
        }
        self.verdict()
    }

    /// Checks the wall clock. `elapsed` is the session's whole run time so far, across resumes.
    pub fn tick(&mut self, elapsed: Duration) -> MeterVerdict {
        self.elapsed = elapsed;
        self.inner
            .observe_time(elapsed.saturating_sub(self.resumed_at));
        self.verdict()
    }

    /// The CLI session was resumed in a new process: from here on only the difference in its
    /// reported totals counts, since they include the earlier turns. The wall clock runs on.
    pub fn resumed(&mut self) {
        if self.capabilities.resume_totals_include_earlier {
            self.inner.continue_conversation();
        } else {
            self.inner.start_new_session();
        }
        self.resumed_at = self.elapsed;
        self.before_process = self.inner.spend().cost_usd;
    }

    /// This meter's session resumes a CLI session on which `earlier` was already spent, such
    /// as after a raised limit. It counts toward the limits, which cover the whole task, but not in
    /// [`spend`](Self::spend). Where the agent's totals include earlier turns, it's taken off
    /// them. Call it before the first event.
    pub fn resumed_from(&mut self, earlier: Spend) {
        if earlier == Spend::default() {
            return;
        }
        self.inner.observe(&AgentEvent::Usage {
            input_tokens: earlier.input_tokens,
            output_tokens: earlier.output_tokens,
            cached_tokens: earlier.cached_tokens,
            cost_usd: Some(earlier.cost_usd),
            cumulative: false,
        });
        self.inner.start_new_session();
        self.earlier = earlier;
        self.before_process = self.inner.spend().cost_usd;
    }

    /// The parser's figure for the requests since the agent's last totals, priced the way they
    /// are billed, in place of the meter's own estimate of them.
    pub fn bill_since_totals(&mut self, usd: Option<f64>) -> MeterVerdict {
        if let Some(usd) = usd.filter(|_| !self.free) {
            self.inner.bill_since_totals(usd);
        }
        self.verdict()
    }

    /// The process says it has spent at least `usd` of its own, where its totals may never
    /// follow, as Claude Code does when its spend cap stops the session's background agents.
    /// What the CLI session spent before the process adds to it.
    pub fn spent_at_least(&mut self, usd: Option<f64>) -> MeterVerdict {
        if let Some(usd) = usd.filter(|_| !self.free) {
            self.inner.spent_at_least(self.before_process + usd);
        }
        self.verdict()
    }

    /// Where the session stands against its limits. The spend in it is the session's own: the
    /// CLI session's earlier spend counts toward the task's limit, but the run holds it already.
    pub fn status(&self) -> SpendStatus {
        let mut status = self.inner.status();
        status.spend = self.spend();
        if let Some((share, limit)) = self.run_share()
            && status.fraction_used.is_none_or(|used| share > used)
        {
            status.fraction_used = Some(share);
            status.binding = Some(LimitDimension::Spend);
            status.limit = limit;
            status.level = self.level_at(share);
        }
        if let Some(share) = self.plan_usage_share()
            && status.fraction_used.is_none_or(|used| share > used)
        {
            status.fraction_used = Some(share);
            status.binding = Some(LimitDimension::Spend);
            status.level = self.level_at(share);
        }
        status
    }

    fn level_at(&self, share: f64) -> MeterAction {
        if share >= 1.0 {
            MeterAction::Stop
        } else if share >= self.inner.soft_ratio() {
            MeterAction::WrapUp
        } else {
            MeterAction::Continue
        }
    }

    /// What this session spent, without what earlier sessions spent on the CLI session.
    pub fn spend(&self) -> Spend {
        let total = self.inner.spend();
        let earlier = &self.earlier;
        Spend {
            input_tokens: total.input_tokens.saturating_sub(earlier.input_tokens),
            output_tokens: total.output_tokens.saturating_sub(earlier.output_tokens),
            cached_tokens: total.cached_tokens.saturating_sub(earlier.cached_tokens),
            cost_usd: (total.cost_usd - earlier.cost_usd).max(0.0),
            ..total
        }
    }

    /// What the CLI session has spent in all, earlier sessions on it included. The limits
    /// count this.
    pub fn total_spend(&self) -> Spend {
        self.inner.spend()
    }

    pub fn tool_calls(&self) -> u64 {
        self.inner.work().tool_calls
    }

    /// The share of the subscription window used since the session's first plan usage reading.
    pub fn plan_usage_used(&self) -> Option<f64> {
        self.plan_usage.used()
    }

    /// The latest `ContextFill`, with the size filled in from the registry when the stream
    /// gave none.
    pub fn context(&self) -> Option<ContextSize> {
        self.context
    }

    /// `window` held to `sessions.context`, when that lowers it.
    fn held(&self, window: Option<u64>) -> Option<u64> {
        match (window, self.context_cap) {
            (Some(window), Some(cap)) => Some(window.min(cap)),
            (window, cap) => window.or(cap),
        }
    }

    /// Where the session starts fresh, in tokens. A share is of the window as the stream
    /// last reported it, which may be larger than the registry knew.
    fn rotate_cap(&self) -> Option<u64> {
        let window = self
            .context
            .and_then(|context| context.size)
            .or(self.held(self.context_window));
        self.rotate_at.as_ref()?.tokens(window).ok().flatten()
    }

    fn verdict(&mut self) -> MeterVerdict {
        let status = self.status();
        let worst = status.binding.zip(status.fraction_used);
        if let Some((dimension, share)) = worst
            && share >= 1.0
        {
            return MeterVerdict::Stop(dimension);
        }
        if let (Some(cap), Some(context)) = (self.rotate_cap(), self.context)
            && !self.rotated
            && context.used >= cap
        {
            self.rotated = true;
            return MeterVerdict::Rotate {
                used: context.used,
                cap,
            };
        }
        if let Some((dimension, share)) = worst
            && share >= self.inner.soft_ratio()
            && !self.wrapped
        {
            self.wrapped = true;
            return MeterVerdict::WrapUp(dimension);
        }
        MeterVerdict::Continue
    }

    fn plan_usage_share(&self) -> Option<f64> {
        let limit = self.plan_usage_limit?;
        let used = self.plan_usage.used().unwrap_or(0.0);
        Some(if limit > 0.0 { used / limit } else { 1.0 })
    }

    /// The share of what was left of the run that the session has spent itself, and that
    /// remainder as a limit.
    fn run_share(&self) -> Option<(f64, MeterLimit)> {
        let left = self.run_left?;
        let limit = match left.unit {
            LimitUnit::Usd => MeterLimit::Usd(left.amount),
            LimitUnit::Tokens => MeterLimit::Tokens(left.amount.max(0.0).round() as u64),
            LimitUnit::PlanUsage | LimitUnit::Minutes => return None,
        };
        let spent = spent_in(&self.spend(), left.unit);
        let share = if left.amount > 0.0 {
            spent / left.amount
        } else {
            1.0
        };
        Some((share, limit))
    }

    /// A usage report as this session's own: where the agent's totals include what earlier
    /// sessions spent on the CLI session it resumed, that's taken off.
    pub fn without_earlier_turns(&self, event: &AgentEvent) -> AgentEvent {
        if !self.capabilities.resume_totals_include_earlier {
            return event.clone();
        }
        match *event {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cached_tokens,
                cost_usd,
                cumulative: true,
            } => {
                let earlier = &self.earlier;
                AgentEvent::Usage {
                    input_tokens: input_tokens.saturating_sub(earlier.input_tokens),
                    output_tokens: output_tokens.saturating_sub(earlier.output_tokens),
                    cached_tokens: cached_tokens.saturating_sub(earlier.cached_tokens),
                    cost_usd: cost_usd.map(|cost| (cost - earlier.cost_usd).max(0.0)),
                    cumulative: true,
                }
            }
            _ => event.clone(),
        }
    }

    fn priced(&self, usage: AgentEvent) -> AgentEvent {
        match (usage, &self.estimator) {
            (
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cached_tokens,
                    cost_usd: None,
                    cumulative,
                },
                Some((registry, worker)),
            ) => {
                let cost_usd = match registry.estimate(worker, input_tokens, output_tokens) {
                    CostEstimate::Usd(usd) => Some(usd),
                    CostEstimate::Free => Some(0.0),
                    CostEstimate::PlanUsage(_) | CostEstimate::Unknown => None,
                };
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cached_tokens,
                    cost_usd,
                    cumulative,
                }
            }
            (usage, _) => usage,
        }
    }

    /// The stream named the model, where the launch left it to the CLI's default.
    fn learn_price(&mut self, model: &str) {
        if !self.price_from_model {
            return;
        }
        if let Some(price) = builtin_price(model) {
            self.inner.set_price(Some(price));
            self.price_from_model = false;
        }
    }
}

/// The share of each plan usage window used since the session's first reading of it. The largest
/// counts, since the windows fill at different speeds.
#[derive(Debug, Default)]
struct PlanUsageTally {
    windows: HashMap<Option<String>, PlanWindow>,
}

#[derive(Debug)]
struct PlanWindow {
    first: f64,
    /// The highest reading since the window last reset.
    last: f64,
    /// Use counted before the window reset.
    carried: f64,
    resets_at: Option<OffsetDateTime>,
}

impl PlanUsageTally {
    fn observe(&mut self, reading: &PlanUsageReading) {
        let Some(used) = reading.used.filter(|used| used.is_finite()) else {
            return;
        };
        let used = used.max(0.0);
        let window = self
            .windows
            .entry(reading.window.clone())
            .or_insert(PlanWindow {
                first: used,
                last: used,
                carried: 0.0,
                resets_at: reading.resets_at,
            });
        let moved = window
            .resets_at
            .zip(reading.resets_at)
            .map(|(known, now)| now - known);
        if moved.is_some_and(|moved| moved < -RESET_JITTER) {
            // Read before the window last reset, such as by a slow probe.
            return;
        }
        let reset = moved.is_some_and(|moved| moved > RESET_JITTER)
            || (used <= PLAN_USAGE_NEAR_ZERO && window.last - used >= PLAN_USAGE_NEAR_ZERO);
        if reset {
            // Everything used in the new window counts.
            window.carried += window.last - window.first;
            window.first = 0.0;
            window.last = used;
        } else {
            // Small drops come from rounding, other readers, or a rolling window. Use already
            // counted stays counted.
            window.last = window.last.max(used);
        }
        window.resets_at = reading.resets_at.or(window.resets_at);
    }

    fn used(&self) -> Option<f64> {
        self.windows
            .values()
            .map(|window| window.carried + (window.last - window.first).max(0.0))
            .reduce(f64::max)
    }
}

/// What each CLI session has spent so far, by working directory, agent, and session id, so
/// a session that resumes one knows what came before. Kept for this process only: after a
/// restart a resumed session counts the agent's whole totals, which errs toward stopping early.
static CLI_SESSION_SPEND: Mutex<BTreeMap<(PathBuf, String, String), Spend>> =
    Mutex::new(BTreeMap::new());

/// What earlier sessions spent on the CLI session `spec` resumes. Zero for a new one.
pub fn spent_before(spec: &SessionSpec, agent_id: &str) -> Spend {
    let Some(id) = spec.resume.as_deref() else {
        return Spend::default();
    };
    let key = (spec.workdir.clone(), agent_id.to_owned(), id.to_owned());
    CLI_SESSION_SPEND
        .lock()
        .ok()
        .and_then(|spent| spent.get(&key).copied())
        .unwrap_or_default()
}

/// Records `total`, everything the CLI session has spent so far, for a later resume.
pub fn record_cli_session_spend(workdir: &Path, agent_id: &str, id: &str, total: Spend) {
    if let Ok(mut spent) = CLI_SESSION_SPEND.lock() {
        spent.insert(
            (workdir.to_path_buf(), agent_id.to_owned(), id.to_owned()),
            total,
        );
    }
}

/// Claude Code's `--max-budget-usd` backstop: a little above the meter's dollar limit, and on a
/// resume only what's left of it, since earlier totals don't count there. A session whose spend
/// goes on unseen in the background has no other stop there, so its cap is the limit itself,
/// rounded down to the cent.
pub fn native_cap_usd(limits: &SessionLimits, spent_before_usd: f64) -> Option<f64> {
    let margin = if limits.spends_unseen {
        1.0
    } else {
        NATIVE_CAP_MARGIN
    };
    let task_limit = match limits.limit {
        Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount,
        }) if amount.is_finite() && amount > 0.0 => Some(amount),
        _ => None,
    };
    let task = if limits.spends_unseen {
        task_limit.or(limits.native_cap_usd)
    } else {
        limits
            .native_cap_usd
            .or(task_limit.map(|amount| amount * NATIVE_CAP_MARGIN))
    };
    let spent = if spent_before_usd.is_finite() {
        spent_before_usd.max(0.0)
    } else {
        0.0
    };
    // What's left of the run counts only this process's spend, as Claude Code's own count does.
    let run = match limits.run_left {
        Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount,
        }) if amount.is_finite() && amount >= 0.0 => Some(amount * margin),
        _ => None,
    };
    let left = match (task.map(|cap| cap - spent), run) {
        (Some(task), Some(run)) => task.min(run),
        (task, run) => task.or(run)?,
    };
    let left = left.max(MIN_NATIVE_CAP_USD);
    // Whole cents: rounded up, so a backstop never lands below the limit, or down, so a cap
    // that is the limit never lets the background past it. The small nudge keeps 0.55 from
    // becoming 0.56 or 0.54 through float error.
    let cents = if limits.spends_unseen {
        ((left * 100.0) + 1e-6).floor()
    } else {
        ((left * 100.0) - 1e-6).ceil()
    };
    Some((cents / 100.0).max(MIN_NATIVE_CAP_USD))
}

fn spent_in(spend: &Spend, unit: LimitUnit) -> f64 {
    match unit {
        LimitUnit::Usd => spend.cost_usd,
        LimitUnit::Tokens => spend.billable_tokens() as f64,
        LimitUnit::PlanUsage | LimitUnit::Minutes => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::base::names::AgentId;
    use argonauts_core::models::limits::{PlanUsageSource, PlanUsageStatus};
    use argonauts_core::settings::agents::AgentSettings;
    use argonauts_core::settings::sessions::RotateAt;
    use argonauts_testkit::engine::services::StaticRegistry;
    use serde_json::json;

    const MINUTE: Duration = Duration::from_secs(60);

    fn limits(limit: Option<(LimitUnit, f64)>) -> SessionLimits {
        SessionLimits {
            limit: limit.map(|(unit, amount)| LimitAmount { unit, amount }),
            ..SessionLimits::default()
        }
    }

    fn setup(limits: SessionLimits) -> MeterSetup {
        MeterSetup {
            limits,
            pricing: Pricing::ReportedOnly,
            capabilities: Capabilities::default(),
            rotate_at: None,
            context_window: None,
            context_cap: None,
        }
    }

    fn meter(limit: Option<(LimitUnit, f64)>) -> SessionMeter {
        SessionMeter::new(setup(limits(limit)))
    }

    fn usage(input: u64, output: u64, cost: Option<f64>, cumulative: bool) -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: 0,
            cost_usd: cost,
            cumulative,
        }
    }

    fn cost(usd: f64) -> AgentEvent {
        usage(10, 10, Some(usd), false)
    }

    fn tool(id: &str, parent: Option<&str>) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: "Bash".into(),
            input: json!({"command": "ls"}),
            parent: parent.map(str::to_owned),
        }
    }

    fn plan_usage(used: f64, window: &str) -> AgentEvent {
        plan_usage_until(used, window, None)
    }

    fn plan_usage_until(used: f64, window: &str, resets_at: Option<OffsetDateTime>) -> AgentEvent {
        AgentEvent::PlanUsage {
            reading: PlanUsageReading {
                agent_id: AgentId("claude-code".into()),
                status: PlanUsageStatus::Allowed,
                used: Some(used),
                resets_at,
                window: Some(window.into()),
                source: PlanUsageSource::RateLimitEvent,
                observed_at: OffsetDateTime::UNIX_EPOCH,
                detail: None,
                wrap_up: false,
            },
        }
    }

    fn priced_registry(model: &str, unit: &str) -> Arc<dyn ModelRegistry> {
        let registry = StaticRegistry::default();
        registry.add(
            serde_json::from_value(json!({
                "id": format!("claude-code/{model}"),
                "provider": "anthropic",
                "kind": "cli",
                "access": "api-key",
                "cost": {"unit": unit, "input_per_mtok": 2.0, "output_per_mtok": 10.0},
            }))
            .unwrap(),
        );
        Arc::new(registry)
    }

    #[test]
    fn meter_units_usd_tokens_plan_usage_minutes() {
        use MeterVerdict::{Continue, Stop, WrapUp};

        let mut usd = meter(Some((LimitUnit::Usd, 1.0)));
        assert_eq!(usd.observe(&cost(0.5)), Continue);
        assert!((usd.spend().cost_usd - 0.5).abs() < 1e-9);

        // Tokens times the price, when the agent reports no cost.
        let price = TokenPrice {
            input_per_mtok: 2.0,
            output_per_mtok: 10.0,
            cached_input_per_mtok: None,
        };
        let mut priced = SessionMeter::new(MeterSetup {
            pricing: Pricing::Price(price),
            ..setup(limits(Some((LimitUnit::Usd, 1.0))))
        });
        priced.observe(&usage(100_000, 20_000, None, false));
        assert!((priced.spend().cost_usd - 0.4).abs() < 1e-9);
        assert!(priced.spend().cost_estimated);

        let mut tokens = meter(Some((LimitUnit::Tokens, 1_000.0)));
        assert_eq!(tokens.observe(&usage(400, 100, None, false)), Continue);
        assert_eq!(
            tokens.observe(&usage(300, 100, None, false)),
            WrapUp(LimitDimension::Spend)
        );
        assert_eq!(
            tokens.observe(&usage(100, 0, None, false)),
            Stop(LimitDimension::Spend)
        );

        // Plan usage counts the change since the first reading, in the window that moved most.
        let mut share = meter(Some((LimitUnit::PlanUsage, 0.10)));
        assert_eq!(share.observe(&plan_usage(0.30, "five_hour")), Continue);
        assert_eq!(share.observe(&plan_usage(0.52, "weekly")), Continue);
        assert_eq!(share.observe(&plan_usage(0.35, "five_hour")), Continue);
        assert!((share.plan_usage_used().unwrap() - 0.05).abs() < 1e-9);
        assert_eq!(
            share.observe(&plan_usage(0.39, "five_hour")),
            WrapUp(LimitDimension::Spend)
        );
        assert_eq!(share.status().binding, Some(LimitDimension::Spend));
        // A reset keeps what was used before it.
        assert_eq!(
            share.observe(&plan_usage(0.02, "five_hour")),
            Stop(LimitDimension::Spend)
        );
        assert!((share.plan_usage_used().unwrap() - 0.11).abs() < 1e-9);

        let mut minutes = meter(Some((LimitUnit::Minutes, 10.0)));
        assert_eq!(minutes.tick(MINUTE * 8), Continue);
        assert_eq!(minutes.tick(MINUTE * 9), WrapUp(LimitDimension::Minutes));
        assert_eq!(minutes.tick(MINUTE * 10), Stop(LimitDimension::Minutes));

        // Subagents' tool calls count too.
        let mut calls = SessionMeter::new(setup(SessionLimits {
            max_tool_calls: Some(3),
            ..SessionLimits::default()
        }));
        assert_eq!(calls.observe(&tool("1", None)), Continue);
        assert_eq!(calls.observe(&tool("2", Some("task-1"))), Continue);
        assert_eq!(
            calls.observe(&tool("3", Some("task-1"))),
            Stop(LimitDimension::ToolCalls)
        );
        assert_eq!(calls.tool_calls(), 3);
    }

    #[test]
    fn pricing_prefers_registry_then_settings_then_builtin() {
        let worker = ModelRef::new("claude-code", Some("opus".into()));
        let mut settings = Settings::default();
        let registry = priced_registry("opus", "usd");
        assert!(matches!(
            pricing(&worker, &settings, Some(&registry), None),
            Pricing::Registry { .. }
        ));

        let agent_price = TokenPrice {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cached_input_per_mtok: None,
        };
        settings.agents.insert(
            "claude-code".into(),
            AgentSettings {
                price: Some(agent_price),
                ..AgentSettings::default()
            },
        );
        let plan_usage_only = priced_registry("opus", "plan_usage");
        assert!(matches!(
            pricing(&worker, &settings, Some(&plan_usage_only), None),
            Pricing::Price(price) if price == agent_price
        ));

        let haiku = ModelRef::new("claude-code", Some("haiku".into()));
        assert!(matches!(
            pricing(&haiku, &Settings::default(), None, None),
            Pricing::Price(price) if Some(price) == builtin_price("haiku")
        ));
        let unknown = ModelRef::new("claude-code", Some("unheard-of".into()));
        assert!(matches!(
            pricing(&unknown, &Settings::default(), None, None),
            Pricing::ReportedOnly
        ));

        // A model on a server of yours is free, unless you set a price.
        let free = |price: &Pricing| {
            matches!(price, Pricing::Price(price) if price.input_per_mtok == 0.0
                && price.output_per_mtok == 0.0)
        };
        let routed = ModelRef::new("opencode", Some("ollama/llama3.2".into()));
        assert!(free(&pricing(&routed, &Settings::default(), None, None)));
        let hosted = ModelRef::new("opencode", Some("openrouter/z-ai/glm-5".into()));
        assert!(matches!(
            pricing(&hosted, &Settings::default(), None, None),
            Pricing::ReportedOnly
        ));
        let hermes = ModelRef::new("hermes", None);
        for local in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080/v1",
            "http://[::1]:1234/v1",
        ] {
            assert!(
                free(&pricing(&hermes, &Settings::default(), None, Some(local))),
                "{local}"
            );
        }
        assert!(matches!(
            pricing(
                &hermes,
                &Settings::default(),
                None,
                Some("https://openrouter.ai/api/v1")
            ),
            Pricing::ReportedOnly
        ));
        let mut gpu = Settings::default();
        gpu.endpoints.insert(
            "gpu".into(),
            argonauts_core::settings::agents::EndpointSettings {
                base_url: Some("http://gpu-box:8000/v1".into()),
                local: true,
                ..Default::default()
            },
        );
        assert!(free(&pricing(
            &hermes,
            &gpu,
            None,
            Some("http://gpu-box:8000/v1")
        )));
        let mut priced = Settings::default();
        priced.agents.insert(
            "hermes".into(),
            AgentSettings {
                price: Some(agent_price),
                ..AgentSettings::default()
            },
        );
        assert!(matches!(
            pricing(&hermes, &priced, None, Some("http://localhost:11434/v1")),
            Pricing::Price(price) if price == agent_price
        ));
        let mut free_meter = meter_with(pricing(
            &hermes,
            &Settings::default(),
            None,
            Some("http://localhost:11434/v1"),
        ));
        free_meter.observe(&usage(100_000, 20_000, None, false));
        let spend = free_meter.spend();
        assert_eq!(spend.cost_usd, 0.0);
        assert!(!spend.cost_partial, "{spend:?}");

        // The registry's pinned price turns tokens into dollars.
        let mut meter = SessionMeter::new(MeterSetup {
            pricing: pricing(&worker, &Settings::default(), Some(&registry), None),
            ..setup(limits(Some((LimitUnit::Usd, 1.0))))
        });
        meter.observe(&usage(100_000, 20_000, None, false));
        assert!((meter.spend().cost_usd - 0.4).abs() < 1e-9);

        // A model named only by the stream still gets its built-in price.
        let mut default_model = meter_with(Pricing::ReportedOnly);
        default_model.observe(&AgentEvent::Init {
            session_id: None,
            model: Some("claude-haiku-4-5-20251001".into()),
            tools: Vec::new(),
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        });
        default_model.observe(&usage(1_000_000, 0, None, false));
        assert!((default_model.spend().cost_usd - 1.0).abs() < 1e-9);
    }

    fn meter_with(pricing: Pricing) -> SessionMeter {
        SessionMeter::new(MeterSetup {
            pricing,
            ..setup(limits(Some((LimitUnit::Usd, 5.0))))
        })
    }

    #[test]
    fn meter_wrap_up_at_85_percent() {
        use MeterVerdict::{Continue, Stop, WrapUp};
        let mut meter = SessionMeter::new(setup(SessionLimits {
            max_tool_calls: Some(10),
            ..limits(Some((LimitUnit::Usd, 1.0)))
        }));
        assert_eq!(meter.observe(&cost(0.80)), Continue);
        assert_eq!(meter.observe(&cost(0.06)), WrapUp(LimitDimension::Spend));
        assert_eq!(meter.observe(&cost(0.05)), Continue);
        // Another limit reaching 85% doesn't send the message again.
        for id in 0..9 {
            assert_eq!(meter.observe(&tool(&id.to_string(), None)), Continue);
        }
        assert_eq!(meter.status().level, MeterAction::WrapUp);
        assert_eq!(meter.observe(&cost(0.10)), Stop(LimitDimension::Spend));
        assert_eq!(meter.tick(MINUTE), Stop(LimitDimension::Spend));
    }

    #[test]
    fn native_cap_backstop_args() {
        let usd = limits(Some((LimitUnit::Usd, 2.0)));
        assert_eq!(native_cap_usd(&usd, 0.0), Some(2.2));
        assert_eq!(native_cap_usd(&usd, 0.5), Some(1.7));
        assert_eq!(native_cap_usd(&usd, 5.0), Some(MIN_NATIVE_CAP_USD));
        assert_eq!(
            native_cap_usd(&limits(Some((LimitUnit::Usd, 0.5))), 0.0),
            Some(0.55)
        );
        let pinned = SessionLimits {
            native_cap_usd: Some(3.0),
            ..usd.clone()
        };
        assert_eq!(native_cap_usd(&pinned, 1.0), Some(2.0));
        assert_eq!(
            native_cap_usd(&limits(Some((LimitUnit::Tokens, 1e6))), 0.0),
            None
        );
        assert_eq!(native_cap_usd(&limits(None), 0.0), None);
    }

    #[test]
    fn with_no_limit_the_meter_counts_only() {
        let mut meter = SessionMeter::new(setup(SessionLimits {
            max_tool_calls: Some(2),
            ..limits(None)
        }));
        assert_eq!(meter.observe(&cost(0.9)), MeterVerdict::Continue);
        assert_eq!(meter.observe(&cost(4.1)), MeterVerdict::Continue);
        assert!((meter.spend().cost_usd - 5.0).abs() < 1e-9);
        assert_eq!(meter.status().limit, MeterLimit::None);
        // A cap on tool calls that you set still applies.
        assert_eq!(meter.observe(&tool("1", None)), MeterVerdict::Continue);
        assert_eq!(
            meter.observe(&tool("2", None)),
            MeterVerdict::Stop(LimitDimension::ToolCalls)
        );
    }

    #[test]
    fn rotate_at_three_forms() {
        let window = Some(200_000);
        let forms = [
            (RotateAt::Tokens(150_000), 150_000),
            (RotateAt::Text("120k".into()), 120_000),
            (RotateAt::Share(0.9), 180_000),
        ];
        for (form, cap) in forms {
            assert_eq!(form.tokens(window).unwrap(), Some(cap), "{form:?}");
            let mut meter = SessionMeter::new(MeterSetup {
                rotate_at: Some(form.clone()),
                context_window: window,
                ..setup(limits(None))
            });
            let fill = |used| AgentEvent::ContextFill {
                used,
                size: None,
                sent: None,
            };
            assert_eq!(meter.observe(&fill(cap - 1)), MeterVerdict::Continue);
            assert_eq!(
                meter.context(),
                Some(ContextSize {
                    used: cap - 1,
                    size: window
                })
            );
            assert_eq!(
                meter.observe(&fill(cap)),
                MeterVerdict::Rotate { used: cap, cap }
            );
            assert_eq!(meter.observe(&fill(cap + 10)), MeterVerdict::Continue);
        }
        assert_eq!(RotateAt::Share(0.9).tokens(None).unwrap(), None);
    }

    /// By default a session starts fresh only near the end of its window, whatever its size,
    /// and never while the window is unknown.
    #[test]
    fn the_default_rotates_near_the_end_of_the_window_the_stream_reports() {
        let default = argonauts_core::settings::sessions::SessionSettings::default().rotate_at;
        let fill = |used, size| AgentEvent::ContextFill {
            used,
            size,
            sent: None,
        };
        let mut meter = SessionMeter::new(MeterSetup {
            rotate_at: Some(default.clone()),
            ..setup(limits(None))
        });
        assert_eq!(meter.observe(&fill(450_000, None)), MeterVerdict::Continue);
        // The CLI says how large its window is, and the share is of that.
        let window = Some(1_000_000);
        assert_eq!(
            meter.observe(&fill(899_999, window)),
            MeterVerdict::Continue
        );
        assert_eq!(
            meter.observe(&fill(900_000, window)),
            MeterVerdict::Rotate {
                used: 900_000,
                cap: 900_000
            }
        );

        // The registry's window is used until the stream reports one.
        let mut meter = SessionMeter::new(MeterSetup {
            rotate_at: Some(default),
            context_window: Some(872_000),
            ..setup(limits(None))
        });
        assert_eq!(meter.observe(&fill(784_799, None)), MeterVerdict::Continue);
        assert_eq!(
            meter.observe(&fill(784_800, None)),
            MeterVerdict::Rotate {
                used: 784_800,
                cap: 784_800
            }
        );
    }

    /// `sessions.context` holds a session below the window its CLI runs with.
    #[test]
    fn a_lowered_context_holds_the_session_whatever_window_the_cli_reports() {
        let mut meter = SessionMeter::new(MeterSetup {
            rotate_at: Some(RotateAt::Share(0.9)),
            context_window: Some(200_000),
            context_cap: Some(200_000),
            ..setup(limits(None))
        });
        let fill = |used| AgentEvent::ContextFill {
            used,
            size: Some(1_000_000),
            sent: None,
        };
        assert_eq!(meter.observe(&fill(179_999)), MeterVerdict::Continue);
        assert_eq!(
            meter.context(),
            Some(ContextSize {
                used: 179_999,
                size: Some(200_000)
            })
        );
        assert_eq!(
            meter.observe(&fill(180_000)),
            MeterVerdict::Rotate {
                used: 180_000,
                cap: 180_000
            }
        );
    }

    #[test]
    fn meter_resume_counts_only_the_difference() {
        // The resumed process reports totals that include the earlier turns.
        let mut same = meter(Some((LimitUnit::Usd, 10.0)));
        same.observe(&usage(1_000, 100, Some(1.0), true));
        same.resumed();
        same.observe(&usage(200, 20, None, false));
        assert_eq!(same.spend().input_tokens, 1_200);
        same.observe(&usage(1_500, 150, Some(1.5), true));
        assert!((same.spend().cost_usd - 1.5).abs() < 1e-9);

        // Totals that start over on resume add up instead.
        let mut fresh_totals = SessionMeter::new(MeterSetup {
            capabilities: Capabilities {
                resume_totals_include_earlier: false,
                ..Capabilities::default()
            },
            ..setup(limits(Some((LimitUnit::Usd, 10.0))))
        });
        fresh_totals.observe(&usage(1_000, 100, Some(1.0), true));
        fresh_totals.resumed();
        fresh_totals.observe(&usage(500, 50, Some(0.5), true));
        assert!((fresh_totals.spend().cost_usd - 1.5).abs() < 1e-9);

        // A new session on a resumed CLI session, after a raised limit, spends only what it
        // adds, while the limit still sees what came before.
        let earlier = Spend {
            input_tokens: 1_000,
            output_tokens: 100,
            cost_usd: 1.0,
            ..Spend::default()
        };
        let mut later = meter(Some((LimitUnit::Usd, 2.0)));
        later.resumed_from(earlier);
        assert_eq!(
            later.observe(&usage(1_400, 140, Some(1.4), true)),
            MeterVerdict::Continue
        );
        let spend = later.spend();
        assert!((spend.cost_usd - 0.4).abs() < 1e-9);
        assert_eq!((spend.input_tokens, spend.output_tokens), (400, 40));
        assert!((later.total_spend().cost_usd - 1.4).abs() < 1e-9);
        assert_eq!(
            later.observe(&usage(1_800, 180, Some(1.8), true)),
            MeterVerdict::WrapUp(LimitDimension::Spend)
        );
        assert_eq!(
            later.observe(&usage(2_000, 200, Some(2.0), true)),
            MeterVerdict::Stop(LimitDimension::Spend)
        );

        // Totals that start over add to what came before.
        let mut restarted = SessionMeter::new(MeterSetup {
            capabilities: Capabilities {
                resume_totals_include_earlier: false,
                ..Capabilities::default()
            },
            ..setup(limits(Some((LimitUnit::Usd, 2.0))))
        });
        restarted.resumed_from(earlier);
        restarted.observe(&usage(500, 50, Some(0.5), true));
        assert!((restarted.spend().cost_usd - 0.5).abs() < 1e-9);
        assert!((restarted.total_spend().cost_usd - 1.5).abs() < 1e-9);
    }

    /// A session resumed the CLI session of one that had spent $0.4355 of the run's $0.50, with
    /// $0.0626 of the run left. Its readings say what it spent itself, so the run counts the
    /// earlier spend once, and only its own spend counts against what was left of the run.
    #[test]
    fn a_resumed_session_counts_only_its_own_spend_against_the_run() {
        let usd = |amount| {
            Some(LimitAmount {
                unit: LimitUnit::Usd,
                amount,
            })
        };
        let earlier = Spend {
            input_tokens: 370_264,
            output_tokens: 3_000,
            cost_usd: 0.43548515,
            ..Spend::default()
        };
        let mut meter = SessionMeter::new(setup(SessionLimits {
            run_left: usd(0.0626),
            ..SessionLimits::default()
        }));
        meter.resumed_from(earlier);
        // Before Claude Code answers, the resumed CLI session's totals are the earlier ones.
        assert_eq!(
            meter.observe(&usage(370_264, 3_000, Some(0.43548515), true)),
            MeterVerdict::Continue
        );
        let status = meter.status();
        assert_eq!(status.spend.cost_usd, 0.0);
        assert_eq!(status.fraction_used, Some(0.0));
        assert_eq!(status.limit, MeterLimit::Usd(0.0626));
        // Its own $0.064 crosses it.
        assert_eq!(
            meter.observe(&usage(380_000, 3_500, Some(0.49948515), true)),
            MeterVerdict::Stop(LimitDimension::Spend)
        );

        // The task's limit still counts what the CLI session spent before.
        let mut task = SessionMeter::new(setup(SessionLimits {
            limit: usd(0.50),
            run_left: usd(0.30),
            ..SessionLimits::default()
        }));
        task.resumed_from(earlier);
        assert_eq!(
            task.observe(&usage(372_000, 3_100, Some(0.50), true)),
            MeterVerdict::Stop(LimitDimension::Spend)
        );
    }

    #[test]
    fn the_native_cap_holds_to_what_is_left_of_the_run() {
        let limits = |limit: Option<f64>, left: f64| SessionLimits {
            limit: limit.map(|amount| LimitAmount {
                unit: LimitUnit::Usd,
                amount,
            }),
            run_left: Some(LimitAmount {
                unit: LimitUnit::Usd,
                amount: left,
            }),
            ..SessionLimits::default()
        };
        assert_eq!(native_cap_usd(&limits(None, 0.2255), 0.0), Some(0.25));
        // Claude Code counts only the resumed process's spend, as what's left of the run does.
        assert_eq!(native_cap_usd(&limits(None, 0.0626), 0.4355), Some(0.07));
        // The tighter of the task's and the run's.
        assert_eq!(
            native_cap_usd(&limits(Some(0.50), 0.30), 0.4355),
            Some(0.12)
        );
        assert_eq!(native_cap_usd(&limits(Some(0.50), 0.0), 0.0), Some(0.01));
    }

    /// What a session's own workflow spends in the background reaches the meter only with a
    /// later result, so for such a session the cap is the only stop there and holds to the
    /// limit with no margin, rounded down to the cent.
    #[test]
    fn a_session_spending_unseen_is_capped_at_the_limit_itself() {
        let usd = |amount: f64| {
            Some(LimitAmount {
                unit: LimitUnit::Usd,
                amount,
            })
        };
        let plain = SessionLimits {
            run_left: usd(0.60),
            ..SessionLimits::default()
        };
        let unseen = SessionLimits {
            spends_unseen: true,
            ..plain.clone()
        };
        assert_eq!(native_cap_usd(&plain, 0.0), Some(0.66));
        assert_eq!(native_cap_usd(&unseen, 0.0), Some(0.60));
        // The task's cap likewise, and the tighter of the two still wins.
        let task = SessionLimits {
            limit: usd(0.50),
            native_cap_usd: Some(0.55),
            ..unseen.clone()
        };
        assert_eq!(native_cap_usd(&task, 0.0), Some(0.50));
        assert_eq!(native_cap_usd(&task, 0.2), Some(0.30));
        let left = SessionLimits {
            run_left: usd(0.239),
            ..unseen.clone()
        };
        assert_eq!(native_cap_usd(&left, 0.0), Some(0.23));
        let none_left = SessionLimits {
            run_left: usd(0.0),
            ..unseen
        };
        assert_eq!(native_cap_usd(&none_left, 0.0), Some(MIN_NATIVE_CAP_USD));
    }

    /// The parser's figure for a request, as it is billed, stops the session at the request
    /// that crosses its limit, where a flat price would have let it go on.
    #[test]
    fn a_billed_figure_stops_the_request_that_crosses_the_limit() {
        let mut meter = SessionMeter::new(MeterSetup {
            pricing: Pricing::Price(builtin_price("claude-haiku-5-5").unwrap()),
            ..setup(limits(Some((LimitUnit::Usd, 0.2255))))
        });
        assert_eq!(
            meter.observe(&usage(361_887, 2, None, false)),
            MeterVerdict::Continue
        );
        assert_eq!(meter.bill_since_totals(None), MeterVerdict::Continue);
        assert_eq!(
            meter.bill_since_totals(Some(0.361891)),
            MeterVerdict::Stop(LimitDimension::Spend)
        );
        assert!(meter.status().spend.cost_estimated);
    }

    /// Claude Code on a model a server of yours runs costs nothing, whatever the parser would
    /// price its requests at.
    #[test]
    fn a_session_on_a_free_model_takes_no_billed_figure() {
        let mut meter = SessionMeter::new(MeterSetup {
            pricing: Pricing::Price(FREE),
            ..setup(limits(Some((LimitUnit::Usd, 0.10))))
        });
        meter.observe(&usage(361_887, 2, None, false));
        assert_eq!(
            meter.bill_since_totals(Some(0.361891)),
            MeterVerdict::Continue
        );
        assert_eq!(meter.spent_at_least(Some(0.675)), MeterVerdict::Continue);
        assert_eq!(meter.status().spend.cost_usd, 0.0);
    }

    /// Claude Code says what its process spent when its cap stops the session's background
    /// agents, and no result may follow. That counts as the process's own spend, on top of what
    /// the CLI session spent before the process, and never lowers the tally.
    #[test]
    fn what_a_process_says_it_spent_counts_on_top_of_what_came_before_it() {
        let mut fresh = meter(Some((LimitUnit::Usd, 0.60)));
        fresh.observe(&usage(366_000, 3_000, Some(0.4274356), true));
        assert_eq!(
            fresh.spent_at_least(Some(0.675)),
            MeterVerdict::Stop(LimitDimension::Spend)
        );
        let spend = fresh.spend();
        assert!((spend.cost_usd - 0.675).abs() < 1e-9, "{spend:?}");
        assert!(spend.cost_estimated);

        // Resumed after a session that spent $0.4355 on the CLI session, whose figure the
        // process leaves out.
        let earlier = Spend {
            input_tokens: 370_264,
            output_tokens: 3_000,
            cost_usd: 0.43548515,
            ..Spend::default()
        };
        let mut later = meter(None);
        later.resumed_from(earlier);
        later.observe(&usage(371_000, 3_100, Some(0.45), true));
        later.spent_at_least(Some(0.10));
        assert!((later.spend().cost_usd - 0.10).abs() < 1e-9);
        assert!((later.total_spend().cost_usd - 0.53548515).abs() < 1e-9);

        // In a new process of the same session, what the first one spent comes first.
        let mut relaunched = meter(None);
        relaunched.observe(&usage(1_000, 100, Some(0.20), true));
        relaunched.resumed();
        relaunched.observe(&usage(1_500, 150, Some(0.25), true));
        relaunched.spent_at_least(Some(0.30));
        assert!((relaunched.spend().cost_usd - 0.50).abs() < 1e-9);
        relaunched.spent_at_least(Some(0.01));
        relaunched.spent_at_least(None);
        assert!((relaunched.spend().cost_usd - 0.50).abs() < 1e-9);
    }

    #[test]
    fn minutes_run_on_across_a_resume() {
        let mut meter = SessionMeter::new(setup(SessionLimits {
            max_minutes: Some(60.0),
            ..SessionLimits::default()
        }));
        assert_eq!(
            meter.tick(MINUTE * 51),
            MeterVerdict::WrapUp(LimitDimension::Minutes)
        );
        meter.resumed();
        let later = MINUTE * 51 + Duration::from_secs(6);
        assert_eq!(meter.tick(later), MeterVerdict::Continue);
        assert!((meter.status().work.minutes - 51.1).abs() < 1e-6);
        meter.resumed();
        assert_eq!(meter.tick(MINUTE * 59), MeterVerdict::Continue);
        assert_eq!(
            meter.tick(MINUTE * 60),
            MeterVerdict::Stop(LimitDimension::Minutes)
        );
    }

    #[test]
    fn plan_usage_small_drops_are_not_resets() {
        let mut share = meter(Some((LimitUnit::PlanUsage, 0.2)));
        for used in [0.40, 0.50, 0.49] {
            assert_eq!(
                share.observe(&plan_usage(used, "five_hour")),
                MeterVerdict::Continue
            );
        }
        assert!((share.plan_usage_used().unwrap() - 0.10).abs() < 1e-9);
        share.observe(&plan_usage(0.52, "five_hour"));
        assert!((share.plan_usage_used().unwrap() - 0.12).abs() < 1e-9);

        // A later reset time is a new window, even with more used than before.
        let hour = time::Duration::hours(1);
        let resets = OffsetDateTime::UNIX_EPOCH + hour;
        let mut windows = meter(Some((LimitUnit::PlanUsage, 1.0)));
        windows.observe(&plan_usage_until(0.40, "five_hour", Some(resets)));
        windows.observe(&plan_usage_until(0.50, "five_hour", Some(resets)));
        windows.observe(&plan_usage_until(
            0.60,
            "five_hour",
            Some(resets + hour * 5),
        ));
        assert!((windows.plan_usage_used().unwrap() - 0.70).abs() < 1e-9);
        // A reading from before that reset changes nothing.
        windows.observe(&plan_usage_until(0.90, "five_hour", Some(resets)));
        assert!((windows.plan_usage_used().unwrap() - 0.70).abs() < 1e-9);
        // Seconds of jitter in the reset time are the same window.
        let jitter = time::Duration::seconds(2);
        windows.observe(&plan_usage_until(
            0.58,
            "five_hour",
            Some(resets + hour * 5 + jitter),
        ));
        windows.observe(&plan_usage_until(
            0.65,
            "five_hour",
            Some(resets + hour * 5 - jitter),
        ));
        assert!((windows.plan_usage_used().unwrap() - 0.75).abs() < 1e-9);
    }
}
