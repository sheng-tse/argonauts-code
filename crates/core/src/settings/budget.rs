//! Budgets and watchdogs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BudgetAmount {
    pub unit: BudgetUnit,
    pub amount: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BudgetUnit {
    /// Tokens times a pinned price.
    Usd,
    /// Input plus output tokens.
    Tokens,
    /// Share of a subscription window, 0 to 1.
    Quota,
    Minutes,
}

/// Each limit's unit is in its key. The `run_*` keys are one choice, and so are the
/// `per_card_*` keys: a layer that sets one clears the ones it inherits, built-in defaults
/// included, so a later layer can switch units. Two in the same layer is an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BudgetSettings {
    /// Off means the meter still counts, but nothing stops for budget.
    pub enabled: bool,
    pub run_usd: Option<f64>,
    pub run_tokens: Option<u64>,
    pub run_quota: Option<f64>,
    pub run_minutes: Option<f64>,
    pub per_card_usd: Option<f64>,
    pub per_card_tokens: Option<u64>,
    pub per_card_quota: Option<f64>,
    pub per_card_minutes: Option<f64>,
    /// Share of any limit that sends the wrap-up message.
    pub soft_stop: f64,
    pub on_hit: OnHit,
    /// Use the median spend of past cards of the same task type, once there are
    /// `history_min` of them. Until then, `per_card_*`.
    pub from_history: bool,
    pub history_min: u32,
    /// How the run budget splits. Shares add up to 1.
    pub lines: BudgetLines,
}

impl Default for BudgetSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            run_usd: Some(20.0),
            run_tokens: None,
            run_quota: None,
            run_minutes: None,
            per_card_usd: Some(3.0),
            per_card_tokens: None,
            per_card_quota: None,
            per_card_minutes: None,
            soft_stop: 0.85,
            on_hit: OnHit::Ask,
            from_history: true,
            history_min: 5,
            lines: BudgetLines::default(),
        }
    }
}

/// What happens when a card hits its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnHit {
    /// Ask you: more budget, hand off, or escalate.
    Ask,
    /// More budget from your rules, within the reserve.
    Extend,
    Handoff,
    Escalate,
}

/// Shares of the run budget. The defaults are placeholders until real runs calibrate them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BudgetLines {
    pub goal_and_plan: f64,
    pub spikes: f64,
    /// Interface and acceptance-test cards.
    pub contracts: f64,
    pub cards: f64,
    pub review: f64,
    pub integration: f64,
    pub reserve: f64,
}

impl Default for BudgetLines {
    fn default() -> Self {
        Self {
            goal_and_plan: 0.15,
            spikes: 0.10,
            contracts: 0.10,
            cards: 0.40,
            review: 0.10,
            integration: 0.05,
            reserve: 0.10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BudgetLineKind {
    GoalAndPlan,
    Spikes,
    Contracts,
    Cards,
    Review,
    Integration,
    Reserve,
}

/// Watchdogs, per card unless the card overrides them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WatchdogSettings {
    /// Tool calls per card, subagents included.
    pub max_tool_calls: u64,
    pub wall_clock_min: f64,
    /// Minutes with no events, CPU, or output from the process tree. Above the harnesses'
    /// own 10-minute limits, so they time out first.
    pub inactivity_min: f64,
    /// The same call this many times in a row gets a nudge; again, a soft stop.
    pub repeat_limit: u32,
}

impl Default for WatchdogSettings {
    fn default() -> Self {
        Self {
            max_tool_calls: 150,
            wall_clock_min: 30.0,
            inactivity_min: 15.0,
            repeat_limit: 10,
        }
    }
}
