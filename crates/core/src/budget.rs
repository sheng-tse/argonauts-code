//! The budget meter. It is plain arithmetic over usage events, so no agent can talk it into more.

use std::fmt;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::AgentEvent;

/// Share of the budget at which the meter asks the agent to wrap up.
pub const DEFAULT_SOFT_RATIO: f64 = 0.85;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BudgetUnit {
    #[default]
    Usd,
    /// Uncached input tokens plus output tokens.
    Tokens,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "unit", content = "amount", rename_all = "snake_case")]
pub enum BudgetLimit {
    #[default]
    None,
    Usd(f64),
    /// Billable tokens, see [`Spend::billable_tokens`].
    Tokens(u64),
}

impl BudgetLimit {
    /// Token amounts are rounded, and negative ones become zero.
    pub fn from_unit(unit: BudgetUnit, amount: f64) -> Self {
        match unit {
            BudgetUnit::Usd => Self::Usd(amount),
            BudgetUnit::Tokens => Self::Tokens(amount.max(0.0).round() as u64),
            BudgetUnit::None => Self::None,
        }
    }

    pub fn amount(&self) -> Option<f64> {
        match *self {
            Self::None => None,
            Self::Usd(usd) => Some(usd),
            Self::Tokens(tokens) => Some(tokens as f64),
        }
    }
}

impl fmt::Display for BudgetLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("no limit"),
            Self::Usd(usd) => write!(f, "${usd:.2}"),
            Self::Tokens(tokens) => write!(f, "{tokens} tokens"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAction {
    Continue,
    /// The meter returns this once.
    WrapUp,
    Stop,
}

/// Dollars per million tokens, used to estimate cost when a vendor reports only tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TokenPrice {
    /// Uncached input.
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    /// Defaults to the input price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_per_mtok: Option<f64>,
}

impl TokenPrice {
    pub fn cost(&self, input_tokens: u64, output_tokens: u64, cached_tokens: u64) -> f64 {
        let cached = cached_tokens.min(input_tokens);
        let fresh = input_tokens - cached;
        let cached_rate = self.cached_input_per_mtok.unwrap_or(self.input_per_mtok);
        (fresh as f64 * self.input_per_mtok
            + cached as f64 * cached_rate
            + output_tokens as f64 * self.output_per_mtok)
            / 1_000_000.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Spend {
    /// All input tokens, cached ones included.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The cached part of `input_tokens`.
    pub cached_tokens: u64,
    /// A lower bound when `cost_partial` is set.
    pub cost_usd: f64,
    /// Some usage came without a cost and no price was configured.
    pub cost_partial: bool,
    pub cost_estimated: bool,
}

impl Spend {
    /// Uncached input plus output tokens, the count token budgets use.
    pub fn billable_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_sub(self.cached_tokens)
            .saturating_add(self.output_tokens)
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    fn plus(&self, other: &Spend) -> Spend {
        Spend {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cached_tokens: self.cached_tokens.saturating_add(other.cached_tokens),
            cost_usd: self.cost_usd + other.cost_usd,
            cost_partial: self.cost_partial || other.cost_partial,
            cost_estimated: self.cost_estimated || other.cost_estimated,
        }
    }
}

/// Totals across all of a card's sessions.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkLimits {
    pub max_minutes: Option<f64>,
    pub max_tool_calls: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkUsed {
    pub minutes: f64,
    /// Subagents' calls included.
    pub tool_calls: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    /// Dollars or tokens.
    Spend,
    Minutes,
    ToolCalls,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub level: BudgetAction,
    pub limit: BudgetLimit,
    pub spend: Spend,
    pub fraction_used: Option<f64>,
    #[serde(default)]
    pub work: WorkUsed,
    #[serde(default)]
    pub work_limits: WorkLimits,
    /// The limit `fraction_used` refers to.
    #[serde(default)]
    pub binding: Option<BudgetDimension>,
}

impl BudgetStatus {
    /// The binding limit in words, such as `the budget of $3.00` or `the 60-minute limit`.
    pub fn limit_text(&self) -> String {
        match self.binding {
            Some(BudgetDimension::Minutes) => match self.work_limits.max_minutes {
                Some(minutes) => format!("the {}-minute limit", trim_number(minutes)),
                None => "the time limit".to_owned(),
            },
            Some(BudgetDimension::ToolCalls) => match self.work_limits.max_tool_calls {
                Some(calls) => format!("the limit of {calls} tool calls"),
                None => "the tool-call limit".to_owned(),
            },
            Some(BudgetDimension::Spend) | None => format!("the budget of {}", self.limit),
        }
    }
}

fn trim_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// Tracks spending for one card and says when to wrap up or stop.
#[derive(Debug, Clone)]
pub struct BudgetMeter {
    limit: BudgetLimit,
    soft_ratio: f64,
    price: Option<TokenPrice>,
    /// Spend from sessions that already ended.
    closed: Spend,
    /// The latest cumulative totals reported by the current session.
    baseline: Spend,
    /// Increments reported after `baseline`.
    since_baseline: Spend,
    work_limits: WorkLimits,
    tool_calls: u64,
    closed_time: Duration,
    session_time: Duration,
    wrap_up_sent: bool,
}

impl BudgetMeter {
    /// Asks for a wrap-up at `soft_ratio` of `limit`. The ratio is clamped to between 0 and 1,
    /// and a NaN falls back to [`DEFAULT_SOFT_RATIO`].
    pub fn new(limit: BudgetLimit, soft_ratio: f64) -> Self {
        let soft_ratio = if soft_ratio.is_nan() {
            DEFAULT_SOFT_RATIO
        } else {
            soft_ratio.clamp(0.0, 1.0)
        };
        Self {
            limit,
            soft_ratio,
            price: None,
            closed: Spend::default(),
            baseline: Spend::default(),
            since_baseline: Spend::default(),
            work_limits: WorkLimits::default(),
            tool_calls: 0,
            closed_time: Duration::ZERO,
            session_time: Duration::ZERO,
            wrap_up_sent: false,
        }
    }

    /// Estimates dollars from `price` when usage events carry no cost.
    pub fn with_price(mut self, price: Option<TokenPrice>) -> Self {
        self.price = price;
        self
    }

    /// Zero limits count as none.
    pub fn with_work_limits(mut self, limits: WorkLimits) -> Self {
        self.work_limits = WorkLimits {
            max_minutes: limits.max_minutes.filter(|m| m.is_finite() && *m > 0.0),
            max_tool_calls: limits.max_tool_calls.filter(|n| *n > 0),
        };
        self
    }

    pub fn set_price(&mut self, price: Option<TokenPrice>) {
        self.price = price;
    }

    pub fn has_price(&self) -> bool {
        self.price.is_some()
    }

    /// Non-usage events leave the tally alone but still return the current action, so `Stop`
    /// repeats until the session ends. `WrapUp` is returned only once.
    pub fn observe(&mut self, event: &AgentEvent) -> BudgetAction {
        match event {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cached_tokens,
                cost_usd,
                cumulative,
            } => {
                let part =
                    self.spend_from_usage(*input_tokens, *output_tokens, *cached_tokens, *cost_usd);
                if *cumulative {
                    self.baseline = part;
                    self.since_baseline = Spend::default();
                } else {
                    self.since_baseline = self.since_baseline.plus(&part);
                }
            }
            AgentEvent::ToolStarted { .. } => self.tool_calls = self.tool_calls.saturating_add(1),
            _ => {}
        }
        self.action()
    }

    /// `session_elapsed` is the current session's whole run time, not a delta.
    pub fn observe_time(&mut self, session_elapsed: Duration) -> BudgetAction {
        self.session_time = session_elapsed;
        self.action()
    }

    /// Closes the current session's tally, so the next session's cumulative totals add to it.
    pub fn start_new_session(&mut self) {
        self.closed = self.closed.plus(&self.baseline.plus(&self.since_baseline));
        self.baseline = Spend::default();
        self.since_baseline = Spend::default();
        self.close_session_time();
    }

    /// Starts a resumed session whose cumulative totals already include the earlier runs of the
    /// conversation, so they replace the current tally instead of adding to it.
    pub fn continue_conversation(&mut self) {
        self.close_session_time();
    }

    fn close_session_time(&mut self) {
        self.closed_time = self.closed_time.saturating_add(self.session_time);
        self.session_time = Duration::ZERO;
    }

    fn action(&mut self) -> BudgetAction {
        match self.level() {
            BudgetAction::WrapUp if self.wrap_up_sent => BudgetAction::Continue,
            BudgetAction::WrapUp => {
                self.wrap_up_sent = true;
                BudgetAction::WrapUp
            }
            other => other,
        }
    }

    pub fn spend(&self) -> Spend {
        self.closed.plus(&self.baseline).plus(&self.since_baseline)
    }

    /// Amount spent in the limit's unit, or `None` when unlimited.
    pub fn used(&self) -> Option<f64> {
        let spend = self.spend();
        match self.limit {
            BudgetLimit::None => None,
            BudgetLimit::Usd(_) => Some(spend.cost_usd),
            BudgetLimit::Tokens(_) => Some(spend.billable_tokens() as f64),
        }
    }

    pub fn work(&self) -> WorkUsed {
        WorkUsed {
            minutes: self
                .closed_time
                .saturating_add(self.session_time)
                .as_secs_f64()
                / 60.0,
            tool_calls: self.tool_calls,
        }
    }

    /// The largest share used of any limit, when there is one.
    pub fn fraction_used(&self) -> Option<f64> {
        self.binding().map(|(_, fraction)| fraction)
    }

    /// The limit closest to being used up, and the share of it used.
    fn binding(&self) -> Option<(BudgetDimension, f64)> {
        let work = self.work();
        let spend = self
            .used()
            .zip(self.limit.amount())
            .map(|(used, limit)| (BudgetDimension::Spend, share(used, limit)));
        let minutes = self
            .work_limits
            .max_minutes
            .map(|max| (BudgetDimension::Minutes, share(work.minutes, max)));
        let calls = self.work_limits.max_tool_calls.map(|max| {
            (
                BudgetDimension::ToolCalls,
                share(work.tool_calls as f64, max as f64),
            )
        });
        [spend, minutes, calls]
            .into_iter()
            .flatten()
            .reduce(|best, next| if best.1 >= next.1 { best } else { next })
    }

    /// Where spending stands, without the once-only rule `observe` applies to `WrapUp`.
    pub fn level(&self) -> BudgetAction {
        match self.fraction_used() {
            Some(fraction) if fraction >= 1.0 => BudgetAction::Stop,
            Some(fraction) if fraction >= self.soft_ratio => BudgetAction::WrapUp,
            _ => BudgetAction::Continue,
        }
    }

    pub fn status(&self) -> BudgetStatus {
        let binding = self.binding();
        BudgetStatus {
            level: self.level(),
            limit: self.limit,
            spend: self.spend(),
            fraction_used: binding.map(|(_, fraction)| fraction),
            work: self.work(),
            work_limits: self.work_limits,
            binding: binding.map(|(dimension, _)| dimension),
        }
    }

    pub fn limit(&self) -> BudgetLimit {
        self.limit
    }

    pub fn soft_ratio(&self) -> f64 {
        self.soft_ratio
    }

    /// Whether `observe` has already returned `WrapUp`.
    pub fn wrap_up_sent(&self) -> bool {
        self.wrap_up_sent
    }

    pub fn work_limits(&self) -> WorkLimits {
        self.work_limits
    }

    fn spend_from_usage(&self, input: u64, output: u64, cached: u64, cost: Option<f64>) -> Spend {
        let mut spend = Spend {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached,
            ..Spend::default()
        };
        match (cost.filter(|c| c.is_finite() && *c >= 0.0), self.price) {
            (Some(cost), _) => spend.cost_usd = cost,
            (None, Some(price)) => {
                spend.cost_usd = price.cost(input, output, cached);
                spend.cost_estimated = true;
            }
            (None, None) => spend.cost_partial = input > 0 || output > 0,
        }
        spend
    }
}

/// A zero limit counts as used up.
fn share(used: f64, limit: f64) -> f64 {
    if limit > 0.0 { used / limit } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, cost: Option<f64>, cumulative: bool) -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: 0,
            cost_usd: cost,
            cumulative,
        }
    }

    fn text() -> AgentEvent {
        AgentEvent::TextDelta {
            text: "x".into(),
            parent: None,
        }
    }

    #[test]
    fn incremental_usage_adds_up() {
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(1.0), 0.85);
        assert_eq!(
            meter.observe(&usage(10, 5, Some(0.2), false)),
            BudgetAction::Continue
        );
        assert_eq!(
            meter.observe(&usage(10, 5, Some(0.3), false)),
            BudgetAction::Continue
        );
        let spend = meter.spend();
        assert_eq!(spend.input_tokens, 20);
        assert_eq!(spend.output_tokens, 10);
        assert!((spend.cost_usd - 0.5).abs() < 1e-9);
        assert!(!spend.cost_partial);
    }

    #[test]
    fn cumulative_usage_replaces_the_total() {
        let mut meter = BudgetMeter::new(BudgetLimit::Tokens(1_000), 0.85);
        meter.observe(&usage(100, 10, None, true));
        meter.observe(&usage(300, 30, None, true));
        assert_eq!(meter.spend().billable_tokens(), 330);
    }

    #[test]
    fn increments_after_a_cumulative_total_add_to_it() {
        let mut meter = BudgetMeter::new(BudgetLimit::Tokens(10_000), 0.85);
        meter.observe(&usage(50, 5, None, false));
        meter.observe(&usage(100, 10, None, true));
        meter.observe(&usage(20, 2, None, false));
        assert_eq!(meter.spend().billable_tokens(), 132);
    }

    #[test]
    fn wrap_up_fires_once_then_stop_repeats() {
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(1.0), 0.85);
        assert_eq!(
            meter.observe(&usage(1, 1, Some(0.5), false)),
            BudgetAction::Continue
        );
        assert_eq!(
            meter.observe(&usage(1, 1, Some(0.4), false)),
            BudgetAction::WrapUp
        );
        assert!(meter.wrap_up_sent());
        assert_eq!(meter.observe(&text()), BudgetAction::Continue);
        assert_eq!(meter.level(), BudgetAction::WrapUp);
        assert_eq!(
            meter.observe(&usage(1, 1, Some(0.1), false)),
            BudgetAction::Stop
        );
        assert_eq!(meter.observe(&text()), BudgetAction::Stop);
    }

    #[test]
    fn a_jump_past_the_limit_stops_without_wrap_up() {
        let mut meter = BudgetMeter::new(BudgetLimit::Tokens(100), 0.85);
        assert_eq!(
            meter.observe(&usage(500, 0, None, true)),
            BudgetAction::Stop
        );
        assert!(!meter.wrap_up_sent());
    }

    #[test]
    fn no_limit_never_acts() {
        let mut meter = BudgetMeter::new(BudgetLimit::None, 0.85);
        assert_eq!(
            meter.observe(&usage(u64::MAX, u64::MAX, Some(1e9), false)),
            BudgetAction::Continue
        );
        assert_eq!(meter.used(), None);
        assert_eq!(meter.fraction_used(), None);
    }

    #[test]
    fn token_budget_ignores_cached_input() {
        let mut meter = BudgetMeter::new(BudgetLimit::Tokens(1_000), 0.85);
        meter.observe(&AgentEvent::Usage {
            input_tokens: 24_763,
            output_tokens: 122,
            cached_tokens: 24_448,
            cost_usd: None,
            cumulative: true,
        });
        assert_eq!(meter.used(), Some(437.0));
        assert_eq!(meter.level(), BudgetAction::Continue);
        assert_eq!(meter.spend().total_tokens(), 24_885);
    }

    #[test]
    fn usd_budget_uses_prices_when_cost_is_missing() {
        let price = TokenPrice {
            input_per_mtok: 1.0,
            output_per_mtok: 10.0,
            cached_input_per_mtok: Some(0.1),
        };
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(1.0), 0.85).with_price(Some(price));
        meter.observe(&AgentEvent::Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cached_tokens: 500_000,
            cost_usd: None,
            cumulative: false,
        });
        let spend = meter.spend();
        assert!((spend.cost_usd - 0.55).abs() < 1e-9);
        assert!(spend.cost_estimated);
        assert!(!spend.cost_partial);
    }

    #[test]
    fn missing_cost_without_prices_is_a_lower_bound() {
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(1.0), 0.85);
        meter.observe(&usage(100, 10, None, false));
        assert!(meter.spend().cost_partial);
        assert_eq!(
            meter.observe(&usage(0, 0, Some(1.5), false)),
            BudgetAction::Stop
        );
    }

    #[test]
    fn bad_costs_are_treated_as_missing() {
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(1.0), 0.85);
        meter.observe(&usage(1, 1, Some(f64::NAN), false));
        meter.observe(&usage(1, 1, Some(-3.0), false));
        assert_eq!(meter.spend().cost_usd, 0.0);
        assert!(meter.spend().cost_partial);
    }

    #[test]
    fn new_session_keeps_earlier_spend() {
        let mut meter = BudgetMeter::new(BudgetLimit::Tokens(1_000), 0.85);
        meter.observe(&usage(200, 0, None, true));
        meter.start_new_session();
        meter.observe(&usage(100, 0, None, true));
        assert_eq!(meter.used(), Some(300.0));
    }

    #[test]
    fn a_resumed_conversation_replaces_the_tally() {
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(1.0), 0.85);
        meter.observe(&usage(900, 0, Some(0.9), true));
        meter.continue_conversation();
        meter.observe(&usage(10, 0, Some(0.01), false));
        assert!((meter.spend().cost_usd - 0.91).abs() < 1e-9);
        meter.observe(&usage(950, 0, Some(0.95), true));
        assert!((meter.spend().cost_usd - 0.95).abs() < 1e-9);
        assert_eq!(meter.spend().input_tokens, 950);
        assert_eq!(meter.level(), BudgetAction::WrapUp);
    }

    #[test]
    fn tool_calls_count_toward_their_limit() {
        let mut meter = BudgetMeter::new(BudgetLimit::None, 0.8).with_work_limits(WorkLimits {
            max_minutes: None,
            max_tool_calls: Some(5),
        });
        let tool = AgentEvent::ToolStarted {
            id: "t".into(),
            name: "Bash".into(),
            input: serde_json::Value::Null,
            parent: None,
        };
        for _ in 0..3 {
            assert_eq!(meter.observe(&tool), BudgetAction::Continue);
        }
        assert_eq!(meter.observe(&tool), BudgetAction::WrapUp);
        assert_eq!(meter.observe(&tool), BudgetAction::Stop);
        let status = meter.status();
        assert_eq!(status.work.tool_calls, 5);
        assert_eq!(status.binding, Some(BudgetDimension::ToolCalls));
        assert_eq!(status.limit_text(), "the limit of 5 tool calls");
    }

    #[test]
    fn time_counts_across_sessions() {
        let mut meter =
            BudgetMeter::new(BudgetLimit::Usd(10.0), 0.85).with_work_limits(WorkLimits {
                max_minutes: Some(10.0),
                max_tool_calls: None,
            });
        assert_eq!(
            meter.observe_time(Duration::from_secs(6 * 60)),
            BudgetAction::Continue
        );
        meter.start_new_session();
        assert_eq!(
            meter.observe_time(Duration::from_secs(3 * 60)),
            BudgetAction::WrapUp
        );
        meter.continue_conversation();
        assert_eq!(
            meter.observe_time(Duration::from_secs(60)),
            BudgetAction::Stop
        );
        let status = meter.status();
        assert!((status.work.minutes - 10.0).abs() < 1e-9);
        assert_eq!(status.binding, Some(BudgetDimension::Minutes));
        assert_eq!(status.limit_text(), "the 10-minute limit");
    }

    #[test]
    fn the_largest_share_binds() {
        let mut meter =
            BudgetMeter::new(BudgetLimit::Usd(1.0), 0.85).with_work_limits(WorkLimits {
                max_minutes: Some(0.0),
                max_tool_calls: Some(0),
            });
        assert_eq!(meter.work_limits(), WorkLimits::default());
        meter.observe(&usage(1, 1, Some(0.5), false));
        let status = meter.status();
        assert_eq!(status.binding, Some(BudgetDimension::Spend));
        assert_eq!(status.fraction_used, Some(0.5));
        assert_eq!(status.limit_text(), "the budget of $1.00");
    }

    #[test]
    fn a_price_can_arrive_later() {
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(1.0), 0.85);
        assert!(!meter.has_price());
        meter.set_price(Some(TokenPrice {
            input_per_mtok: 1.0,
            output_per_mtok: 1.0,
            cached_input_per_mtok: None,
        }));
        assert!(meter.has_price());
        meter.observe(&usage(500_000, 0, None, false));
        assert!((meter.spend().cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn old_status_json_still_reads() {
        let json = r#"{"level":"continue","limit":{"unit":"none"},"spend":{"input_tokens":0,"output_tokens":0,"cached_tokens":0,"cost_usd":0.0,"cost_partial":false,"cost_estimated":false},"fraction_used":null}"#;
        let status: BudgetStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.work, WorkUsed::default());
        assert_eq!(status.binding, None);
    }

    #[test]
    fn soft_ratio_is_clamped() {
        assert_eq!(BudgetMeter::new(BudgetLimit::None, 7.0).soft_ratio(), 1.0);
        assert_eq!(BudgetMeter::new(BudgetLimit::None, -1.0).soft_ratio(), 0.0);
        assert_eq!(
            BudgetMeter::new(BudgetLimit::None, f64::NAN).soft_ratio(),
            DEFAULT_SOFT_RATIO
        );
    }

    #[test]
    fn status_reports_fraction() {
        let mut meter = BudgetMeter::new(BudgetLimit::Usd(2.0), 0.85);
        meter.observe(&usage(1, 1, Some(0.5), false));
        let status = meter.status();
        assert_eq!(status.level, BudgetAction::Continue);
        assert_eq!(status.fraction_used, Some(0.25));
        assert_eq!(status.limit, BudgetLimit::Usd(2.0));
    }

    #[test]
    fn limit_serde_and_display() {
        let cases = [
            (BudgetLimit::None, r#"{"unit":"none"}"#, "no limit"),
            (
                BudgetLimit::Usd(3.0),
                r#"{"unit":"usd","amount":3.0}"#,
                "$3.00",
            ),
            (
                BudgetLimit::Tokens(500),
                r#"{"unit":"tokens","amount":500}"#,
                "500 tokens",
            ),
        ];
        for (limit, json, shown) in cases {
            assert_eq!(serde_json::to_string(&limit).unwrap(), json);
            assert_eq!(serde_json::from_str::<BudgetLimit>(json).unwrap(), limit);
            assert_eq!(limit.to_string(), shown);
        }
    }

    #[test]
    fn limit_from_unit() {
        assert_eq!(
            BudgetLimit::from_unit(BudgetUnit::Usd, 2.5),
            BudgetLimit::Usd(2.5)
        );
        assert_eq!(
            BudgetLimit::from_unit(BudgetUnit::Tokens, 99.6),
            BudgetLimit::Tokens(100)
        );
        assert_eq!(
            BudgetLimit::from_unit(BudgetUnit::Tokens, -5.0),
            BudgetLimit::Tokens(0)
        );
        assert_eq!(
            BudgetLimit::from_unit(BudgetUnit::None, 9.0),
            BudgetLimit::None
        );
    }
}
