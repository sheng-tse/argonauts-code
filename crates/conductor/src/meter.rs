//! Each session's meter, around core's `BudgetMeter`: spend in dollars, tokens, quota, or
//! minutes, tool calls with subagents' included where visible, and how full the context is. It
//! says when to send the wrap-up message, when to stop, and when to rotate.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::limits::{CostEstimate, QuotaReading};
use argonauts_core::names::WorkerRef;
use argonauts_core::registry::{CostUnit, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::endpoints::{
    endpoint_settings, host_and_port, local_endpoint_addresses,
};
use argonauts_core::worker::{SessionLimits, SessionSpec};
use argonauts_core::{
    AgentEvent, BudgetAction, BudgetDimension, BudgetLimit, BudgetMeter, BudgetStatus,
    Capabilities, Spend, TokenPrice, WorkLimits, builtin_price,
};
use time::OffsetDateTime;

/// How far above the meter's dollar limit the vendor's own cap sits, so the meter always acts
/// first.
pub const NATIVE_CAP_MARGIN: f64 = 1.1;
/// The smallest native cap passed, so the flag stays valid when nothing is left.
const MIN_NATIVE_CAP_USD: f64 = 0.01;

/// A quota reading at most this, and at least this much below the last one, means the window
/// reset.
const QUOTA_NEAR_ZERO: f64 = 0.05;
/// `resets_at` moving by less than this is jitter from vendors that report seconds until the
/// reset.
const RESET_JITTER: time::Duration = time::Duration::minutes(1);

/// Where the meter's dollar prices come from.
#[derive(Clone)]
pub enum Pricing {
    /// `ModelRegistry::estimate` for this worker.
    Registry {
        registry: Arc<dyn ModelRegistry>,
        worker: WorkerRef,
    },
    /// `agents.<id>.price`, else 0.0.1's `builtin_price`.
    Price(TokenPrice),
    /// Only what the vendor reports as cost.
    ReportedOnly,
}

/// The registry's price when there is one, then `agents.<id>.price`, then `builtin_price`, then
/// free for a model on one of your local servers. `served_from` is where the CLI's own config
/// sends its model calls, for one that takes its provider only from there.
pub fn pricing(
    worker: &WorkerRef,
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
    let agent = settings.agents.get(&worker.worker);
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
    worker: &WorkerRef,
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
/// model paid by quota, or one it doesn't know.
fn registry_price(registry: &dyn ModelRegistry, worker: &WorkerRef) -> Option<TokenPrice> {
    let cost = registry.get(worker)?.cost;
    match cost.unit {
        CostUnit::Usd => Some(TokenPrice {
            input_per_mtok: cost.input_per_mtok?,
            output_per_mtok: cost.output_per_mtok?,
            cached_input_per_mtok: cost.cached_input_per_mtok,
        }),
        CostUnit::None => Some(FREE),
        CostUnit::Quota => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeterVerdict {
    Continue,
    /// A limit reached `soft_stop`: send the fixed wrap-up message, once.
    WrapUp(BudgetDimension),
    /// A limit reached 100%: interrupt, then stop. The end reason is `BudgetHit`.
    Stop(BudgetDimension),
    /// The context reached `sessions.rotate_at`: end the session so the engine starts a fresh
    /// one with a handoff note.
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
    /// `budget.enabled`. Off counts and reports but never stops for budget.
    pub budgets_enabled: bool,
    /// `sessions.rotate_at` as tokens, already resolved against the window.
    pub rotate_at: Option<u64>,
    /// The registry's context window, used when the stream reports none.
    pub context_window: Option<u64>,
}

/// Tokens in the context and the window's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextSize {
    pub used: u64,
    pub size: Option<u64>,
}

/// With budgets off, spend never stops a session, but the wall-clock and tool-call limits still
/// do, since they are watchdogs.
pub struct SessionMeter {
    inner: BudgetMeter,
    /// A quota budget, as a share of the subscription window.
    quota_limit: Option<f64>,
    quota: QuotaTally,
    /// Prices usage the vendor sent without cost, when the registry has no pinned price.
    estimator: Option<(Arc<dyn ModelRegistry>, WorkerRef)>,
    /// Set when no price is known yet, so the model the stream reports can supply one.
    price_from_model: bool,
    capabilities: Capabilities,
    rotate_at: Option<u64>,
    context_window: Option<u64>,
    context: Option<ContextSize>,
    /// What earlier sessions spent on the vendor session this one resumes.
    earlier: Spend,
    /// The whole run time at the latest tick.
    elapsed: Duration,
    /// The run time when the vendor session last resumed in a new process. Core's meter counts
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
            budgets_enabled,
            rotate_at,
            context_window,
        } = setup;
        let budget = limits
            .budget
            .filter(|budget| budgets_enabled && budget.amount.is_finite());
        let (limit, quota_limit, budget_minutes) = match budget {
            Some(BudgetAmount { unit, amount }) => match unit {
                BudgetUnit::Usd => (BudgetLimit::Usd(amount), None, None),
                BudgetUnit::Tokens => (
                    BudgetLimit::Tokens(amount.max(0.0).round() as u64),
                    None,
                    None,
                ),
                BudgetUnit::Quota => (BudgetLimit::None, Some(amount), None),
                BudgetUnit::Minutes => (BudgetLimit::None, None, Some(amount)),
            },
            None => (BudgetLimit::None, None, None),
        };
        let max_minutes = match (limits.max_minutes, budget_minutes) {
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
        let inner = BudgetMeter::new(limit, limits.soft_stop)
            .with_price(price)
            .with_work_limits(WorkLimits {
                max_minutes,
                max_tool_calls: limits.max_tool_calls,
            });
        Self {
            inner,
            quota_limit,
            quota: QuotaTally::default(),
            price_from_model: price.is_none() && estimator.is_none(),
            estimator,
            capabilities,
            rotate_at,
            context_window,
            context: None,
            earlier: Spend::default(),
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
            AgentEvent::ContextFill { used, size } => {
                self.context = Some(ContextSize {
                    used: *used,
                    size: size.or(self.context_window),
                });
            }
            AgentEvent::Quota { reading } => self.quota.observe(reading),
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

    /// The vendor session was resumed in a new process: from here on only the difference in its
    /// reported totals counts, since they include the earlier turns. The wall clock runs on.
    pub fn resumed(&mut self) {
        if self.capabilities.resume_totals_include_earlier {
            self.inner.continue_conversation();
        } else {
            self.inner.start_new_session();
        }
        self.resumed_at = self.elapsed;
    }

    /// This meter's session resumes a vendor session on which `earlier` was already spent, such
    /// as after more budget. It counts toward the limits, which cover the whole card, but not in
    /// [`spend`](Self::spend). Where the vendor's totals include earlier turns, it's taken off
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
    }

    pub fn status(&self) -> BudgetStatus {
        let mut status = self.inner.status();
        if let Some(share) = self.quota_share()
            && status.fraction_used.is_none_or(|used| share > used)
        {
            status.fraction_used = Some(share);
            status.binding = Some(BudgetDimension::Spend);
            status.level = if share >= 1.0 {
                BudgetAction::Stop
            } else if share >= self.inner.soft_ratio() {
                BudgetAction::WrapUp
            } else {
                BudgetAction::Continue
            };
        }
        status
    }

    /// What this session spent, without what earlier sessions spent on the vendor session.
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

    /// What the vendor session has spent in all, earlier sessions on it included. The limits
    /// count this.
    pub fn total_spend(&self) -> Spend {
        self.inner.spend()
    }

    pub fn tool_calls(&self) -> u64 {
        self.inner.work().tool_calls
    }

    /// The share of the subscription window used since the session's first quota reading.
    pub fn quota_used(&self) -> Option<f64> {
        self.quota.used()
    }

    /// The latest `ContextFill`, with the size filled in from the registry when the stream
    /// gave none.
    pub fn context(&self) -> Option<ContextSize> {
        self.context
    }

    fn verdict(&mut self) -> MeterVerdict {
        let status = self.status();
        let worst = status.binding.zip(status.fraction_used);
        if let Some((dimension, share)) = worst
            && share >= 1.0
        {
            return MeterVerdict::Stop(dimension);
        }
        if let (Some(cap), Some(context)) = (self.rotate_at, self.context)
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

    fn quota_share(&self) -> Option<f64> {
        let limit = self.quota_limit?;
        let used = self.quota.used().unwrap_or(0.0);
        Some(if limit > 0.0 { used / limit } else { 1.0 })
    }

    /// A usage report as this session's own: where the vendor's totals include what earlier
    /// sessions spent on the vendor session it resumed, that's taken off.
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
                    CostEstimate::Quota(_) | CostEstimate::Unknown => None,
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

/// The share of each quota window used since the session's first reading of it. The largest
/// counts, since the windows fill at different speeds.
#[derive(Debug, Default)]
struct QuotaTally {
    windows: HashMap<Option<String>, QuotaWindow>,
}

#[derive(Debug)]
struct QuotaWindow {
    first: f64,
    /// The highest reading since the window last reset.
    last: f64,
    /// Use counted before the window reset.
    carried: f64,
    resets_at: Option<OffsetDateTime>,
}

impl QuotaTally {
    fn observe(&mut self, reading: &QuotaReading) {
        let Some(used) = reading.used.filter(|used| used.is_finite()) else {
            return;
        };
        let used = used.max(0.0);
        let window = self
            .windows
            .entry(reading.window.clone())
            .or_insert(QuotaWindow {
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
            || (used <= QUOTA_NEAR_ZERO && window.last - used >= QUOTA_NEAR_ZERO);
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

/// What each vendor session has spent so far, by working directory, vendor, and session id, so
/// a session that resumes one knows what came before. Kept for this process only: after a
/// restart a resumed session counts the vendor's whole totals, which errs toward stopping early.
static VENDOR_SESSION_SPEND: Mutex<BTreeMap<(PathBuf, String, String), Spend>> =
    Mutex::new(BTreeMap::new());

/// What earlier sessions spent on the vendor session `spec` resumes. Zero for a new one.
pub fn spent_before(spec: &SessionSpec, vendor: &str) -> Spend {
    let Some(id) = spec.resume.as_deref() else {
        return Spend::default();
    };
    let key = (spec.workdir.clone(), vendor.to_owned(), id.to_owned());
    VENDOR_SESSION_SPEND
        .lock()
        .ok()
        .and_then(|spent| spent.get(&key).copied())
        .unwrap_or_default()
}

/// Records `total`, everything the vendor session has spent so far, for a later resume.
pub fn record_vendor_session_spend(workdir: &Path, vendor: &str, id: &str, total: Spend) {
    if let Ok(mut spent) = VENDOR_SESSION_SPEND.lock() {
        spent.insert(
            (workdir.to_path_buf(), vendor.to_owned(), id.to_owned()),
            total,
        );
    }
}

/// Claude Code's `--max-budget-usd` backstop: a little above the meter's dollar limit, and on a
/// resume only what's left of it, since earlier totals don't count there.
pub fn native_cap_usd(limits: &SessionLimits, spent_before_usd: f64) -> Option<f64> {
    let cap = limits.native_cap_usd.or_else(|| match limits.budget {
        Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount,
        }) if amount.is_finite() && amount > 0.0 => Some(amount * NATIVE_CAP_MARGIN),
        _ => None,
    })?;
    let spent = if spent_before_usd.is_finite() {
        spent_before_usd.max(0.0)
    } else {
        0.0
    };
    let left = (cap - spent).max(MIN_NATIVE_CAP_USD);
    // Whole cents, rounded up, so the backstop never lands below the limit. The small nudge
    // keeps 0.55 from becoming 0.56 through float error.
    Some(((left * 100.0) - 1e-6).ceil() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::{QuotaSource, QuotaStatus};
    use argonauts_core::names::VendorId;
    use argonauts_core::settings::agents::AgentSettings;
    use argonauts_core::settings::sessions::RotateAt;
    use argonauts_testkit::services::StaticRegistry;
    use serde_json::json;

    const MINUTE: Duration = Duration::from_secs(60);

    fn limits(budget: Option<(BudgetUnit, f64)>) -> SessionLimits {
        SessionLimits {
            budget: budget.map(|(unit, amount)| BudgetAmount { unit, amount }),
            ..SessionLimits::default()
        }
    }

    fn setup(limits: SessionLimits) -> MeterSetup {
        MeterSetup {
            limits,
            pricing: Pricing::ReportedOnly,
            capabilities: Capabilities::default(),
            budgets_enabled: true,
            rotate_at: None,
            context_window: None,
        }
    }

    fn meter(budget: Option<(BudgetUnit, f64)>) -> SessionMeter {
        SessionMeter::new(setup(limits(budget)))
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

    fn quota(used: f64, window: &str) -> AgentEvent {
        quota_until(used, window, None)
    }

    fn quota_until(used: f64, window: &str, resets_at: Option<OffsetDateTime>) -> AgentEvent {
        AgentEvent::Quota {
            reading: QuotaReading {
                vendor: VendorId("claude-code".into()),
                status: QuotaStatus::Allowed,
                used: Some(used),
                resets_at,
                window: Some(window.into()),
                source: QuotaSource::RateLimitEvent,
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
                "family": "claude",
                "kind": "harness",
                "access": "api-key",
                "cost": {"unit": unit, "input_per_mtok": 2.0, "output_per_mtok": 10.0},
            }))
            .unwrap(),
        );
        Arc::new(registry)
    }

    #[test]
    fn meter_units_usd_tokens_quota_minutes() {
        use MeterVerdict::{Continue, Stop, WrapUp};

        let mut usd = meter(Some((BudgetUnit::Usd, 1.0)));
        assert_eq!(usd.observe(&cost(0.5)), Continue);
        assert!((usd.spend().cost_usd - 0.5).abs() < 1e-9);

        // Tokens times the price, when the vendor reports no cost.
        let price = TokenPrice {
            input_per_mtok: 2.0,
            output_per_mtok: 10.0,
            cached_input_per_mtok: None,
        };
        let mut priced = SessionMeter::new(MeterSetup {
            pricing: Pricing::Price(price),
            ..setup(limits(Some((BudgetUnit::Usd, 1.0))))
        });
        priced.observe(&usage(100_000, 20_000, None, false));
        assert!((priced.spend().cost_usd - 0.4).abs() < 1e-9);
        assert!(priced.spend().cost_estimated);

        let mut tokens = meter(Some((BudgetUnit::Tokens, 1_000.0)));
        assert_eq!(tokens.observe(&usage(400, 100, None, false)), Continue);
        assert_eq!(
            tokens.observe(&usage(300, 100, None, false)),
            WrapUp(BudgetDimension::Spend)
        );
        assert_eq!(
            tokens.observe(&usage(100, 0, None, false)),
            Stop(BudgetDimension::Spend)
        );

        // Quota counts the change since the first reading, in the window that moved most.
        let mut share = meter(Some((BudgetUnit::Quota, 0.10)));
        assert_eq!(share.observe(&quota(0.30, "five_hour")), Continue);
        assert_eq!(share.observe(&quota(0.52, "weekly")), Continue);
        assert_eq!(share.observe(&quota(0.35, "five_hour")), Continue);
        assert!((share.quota_used().unwrap() - 0.05).abs() < 1e-9);
        assert_eq!(
            share.observe(&quota(0.39, "five_hour")),
            WrapUp(BudgetDimension::Spend)
        );
        assert_eq!(share.status().binding, Some(BudgetDimension::Spend));
        // A reset keeps what was used before it.
        assert_eq!(
            share.observe(&quota(0.02, "five_hour")),
            Stop(BudgetDimension::Spend)
        );
        assert!((share.quota_used().unwrap() - 0.11).abs() < 1e-9);

        let mut minutes = meter(Some((BudgetUnit::Minutes, 10.0)));
        assert_eq!(minutes.tick(MINUTE * 8), Continue);
        assert_eq!(minutes.tick(MINUTE * 9), WrapUp(BudgetDimension::Minutes));
        assert_eq!(minutes.tick(MINUTE * 10), Stop(BudgetDimension::Minutes));

        // Subagents' tool calls count too.
        let mut calls = SessionMeter::new(setup(SessionLimits {
            max_tool_calls: Some(3),
            ..SessionLimits::default()
        }));
        assert_eq!(calls.observe(&tool("1", None)), Continue);
        assert_eq!(calls.observe(&tool("2", Some("task-1"))), Continue);
        assert_eq!(
            calls.observe(&tool("3", Some("task-1"))),
            Stop(BudgetDimension::ToolCalls)
        );
        assert_eq!(calls.tool_calls(), 3);
    }

    #[test]
    fn pricing_prefers_registry_then_settings_then_builtin() {
        let worker = WorkerRef::new("claude-code", Some("opus".into()));
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
        let quota_only = priced_registry("opus", "quota");
        assert!(matches!(
            pricing(&worker, &settings, Some(&quota_only), None),
            Pricing::Price(price) if price == agent_price
        ));

        let haiku = WorkerRef::new("claude-code", Some("haiku".into()));
        assert!(matches!(
            pricing(&haiku, &Settings::default(), None, None),
            Pricing::Price(price) if Some(price) == builtin_price("haiku")
        ));
        let unknown = WorkerRef::new("claude-code", Some("unheard-of".into()));
        assert!(matches!(
            pricing(&unknown, &Settings::default(), None, None),
            Pricing::ReportedOnly
        ));

        // A model on a server of yours is free, unless you set a price.
        let free = |price: &Pricing| {
            matches!(price, Pricing::Price(price) if price.input_per_mtok == 0.0
                && price.output_per_mtok == 0.0)
        };
        let routed = WorkerRef::new("opencode", Some("ollama/llama3.2".into()));
        assert!(free(&pricing(&routed, &Settings::default(), None, None)));
        let hosted = WorkerRef::new("opencode", Some("openrouter/z-ai/glm-5".into()));
        assert!(matches!(
            pricing(&hosted, &Settings::default(), None, None),
            Pricing::ReportedOnly
        ));
        let hermes = WorkerRef::new("hermes", None);
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
            ..setup(limits(Some((BudgetUnit::Usd, 1.0))))
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
            ..setup(limits(Some((BudgetUnit::Usd, 5.0))))
        })
    }

    #[test]
    fn meter_wrap_up_at_85_percent() {
        use MeterVerdict::{Continue, Stop, WrapUp};
        let mut meter = SessionMeter::new(setup(SessionLimits {
            max_tool_calls: Some(10),
            ..limits(Some((BudgetUnit::Usd, 1.0)))
        }));
        assert_eq!(meter.observe(&cost(0.80)), Continue);
        assert_eq!(meter.observe(&cost(0.06)), WrapUp(BudgetDimension::Spend));
        assert_eq!(meter.observe(&cost(0.05)), Continue);
        // Another limit reaching 85% doesn't send the message again.
        for id in 0..9 {
            assert_eq!(meter.observe(&tool(&id.to_string(), None)), Continue);
        }
        assert_eq!(meter.status().level, BudgetAction::WrapUp);
        assert_eq!(meter.observe(&cost(0.10)), Stop(BudgetDimension::Spend));
        assert_eq!(meter.tick(MINUTE), Stop(BudgetDimension::Spend));
    }

    #[test]
    fn native_cap_backstop_args() {
        let usd = limits(Some((BudgetUnit::Usd, 2.0)));
        assert_eq!(native_cap_usd(&usd, 0.0), Some(2.2));
        assert_eq!(native_cap_usd(&usd, 0.5), Some(1.7));
        assert_eq!(native_cap_usd(&usd, 5.0), Some(MIN_NATIVE_CAP_USD));
        assert_eq!(
            native_cap_usd(&limits(Some((BudgetUnit::Usd, 0.5))), 0.0),
            Some(0.55)
        );
        let pinned = SessionLimits {
            native_cap_usd: Some(3.0),
            ..usd.clone()
        };
        assert_eq!(native_cap_usd(&pinned, 1.0), Some(2.0));
        assert_eq!(
            native_cap_usd(&limits(Some((BudgetUnit::Tokens, 1e6))), 0.0),
            None
        );
        assert_eq!(native_cap_usd(&limits(None), 0.0), None);
    }

    #[test]
    fn budget_off_counts_only() {
        let mut meter = SessionMeter::new(MeterSetup {
            budgets_enabled: false,
            ..setup(SessionLimits {
                max_tool_calls: Some(2),
                ..limits(Some((BudgetUnit::Usd, 1.0)))
            })
        });
        assert_eq!(meter.observe(&cost(0.9)), MeterVerdict::Continue);
        assert_eq!(meter.observe(&cost(4.1)), MeterVerdict::Continue);
        assert!((meter.spend().cost_usd - 5.0).abs() < 1e-9);
        assert_eq!(meter.status().limit, BudgetLimit::None);
        // The tool-call watchdog still applies.
        assert_eq!(meter.observe(&tool("1", None)), MeterVerdict::Continue);
        assert_eq!(
            meter.observe(&tool("2", None)),
            MeterVerdict::Stop(BudgetDimension::ToolCalls)
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
            let resolved = form.tokens(window).unwrap();
            assert_eq!(resolved, Some(cap), "{form:?}");
            let mut meter = SessionMeter::new(MeterSetup {
                rotate_at: resolved,
                context_window: window,
                ..setup(limits(None))
            });
            let fill = |used| AgentEvent::ContextFill { used, size: None };
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
        // The default is an absolute cap, whatever the window.
        let default = argonauts_core::settings::sessions::SessionSettings::default().rotate_at;
        assert_eq!(default.tokens(None).unwrap(), Some(200_000));
        assert_eq!(RotateAt::Share(0.9).tokens(None).unwrap(), None);
    }

    #[test]
    fn meter_resume_counts_only_the_difference() {
        // The resumed process reports totals that include the earlier turns.
        let mut same = meter(Some((BudgetUnit::Usd, 10.0)));
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
            ..setup(limits(Some((BudgetUnit::Usd, 10.0))))
        });
        fresh_totals.observe(&usage(1_000, 100, Some(1.0), true));
        fresh_totals.resumed();
        fresh_totals.observe(&usage(500, 50, Some(0.5), true));
        assert!((fresh_totals.spend().cost_usd - 1.5).abs() < 1e-9);

        // A new session on a resumed vendor session, after more budget, spends only what it
        // adds, while the limit still sees what came before.
        let earlier = Spend {
            input_tokens: 1_000,
            output_tokens: 100,
            cost_usd: 1.0,
            ..Spend::default()
        };
        let mut later = meter(Some((BudgetUnit::Usd, 2.0)));
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
            MeterVerdict::WrapUp(BudgetDimension::Spend)
        );
        assert_eq!(
            later.observe(&usage(2_000, 200, Some(2.0), true)),
            MeterVerdict::Stop(BudgetDimension::Spend)
        );

        // Totals that start over add to what came before.
        let mut restarted = SessionMeter::new(MeterSetup {
            capabilities: Capabilities {
                resume_totals_include_earlier: false,
                ..Capabilities::default()
            },
            ..setup(limits(Some((BudgetUnit::Usd, 2.0))))
        });
        restarted.resumed_from(earlier);
        restarted.observe(&usage(500, 50, Some(0.5), true));
        assert!((restarted.spend().cost_usd - 0.5).abs() < 1e-9);
        assert!((restarted.total_spend().cost_usd - 1.5).abs() < 1e-9);
    }

    #[test]
    fn minutes_run_on_across_a_resume() {
        let mut meter = SessionMeter::new(setup(SessionLimits {
            max_minutes: Some(60.0),
            ..SessionLimits::default()
        }));
        assert_eq!(
            meter.tick(MINUTE * 51),
            MeterVerdict::WrapUp(BudgetDimension::Minutes)
        );
        meter.resumed();
        let later = MINUTE * 51 + Duration::from_secs(6);
        assert_eq!(meter.tick(later), MeterVerdict::Continue);
        assert!((meter.status().work.minutes - 51.1).abs() < 1e-6);
        meter.resumed();
        assert_eq!(meter.tick(MINUTE * 59), MeterVerdict::Continue);
        assert_eq!(
            meter.tick(MINUTE * 60),
            MeterVerdict::Stop(BudgetDimension::Minutes)
        );
    }

    #[test]
    fn quota_small_drops_are_not_resets() {
        let mut share = meter(Some((BudgetUnit::Quota, 0.2)));
        for used in [0.40, 0.50, 0.49] {
            assert_eq!(
                share.observe(&quota(used, "five_hour")),
                MeterVerdict::Continue
            );
        }
        assert!((share.quota_used().unwrap() - 0.10).abs() < 1e-9);
        share.observe(&quota(0.52, "five_hour"));
        assert!((share.quota_used().unwrap() - 0.12).abs() < 1e-9);

        // A later reset time is a new window, even with more used than before.
        let hour = time::Duration::hours(1);
        let resets = OffsetDateTime::UNIX_EPOCH + hour;
        let mut windows = meter(Some((BudgetUnit::Quota, 1.0)));
        windows.observe(&quota_until(0.40, "five_hour", Some(resets)));
        windows.observe(&quota_until(0.50, "five_hour", Some(resets)));
        windows.observe(&quota_until(0.60, "five_hour", Some(resets + hour * 5)));
        assert!((windows.quota_used().unwrap() - 0.70).abs() < 1e-9);
        // A reading from before that reset changes nothing.
        windows.observe(&quota_until(0.90, "five_hour", Some(resets)));
        assert!((windows.quota_used().unwrap() - 0.70).abs() < 1e-9);
        // Seconds of jitter in the reset time are the same window.
        let jitter = time::Duration::seconds(2);
        windows.observe(&quota_until(
            0.58,
            "five_hour",
            Some(resets + hour * 5 + jitter),
        ));
        windows.observe(&quota_until(
            0.65,
            "five_hour",
            Some(resets + hour * 5 - jitter),
        ));
        assert!((windows.quota_used().unwrap() - 0.75).abs() < 1e-9);
    }
}
