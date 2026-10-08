//! The run and task limits, and the watchdogs.

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

/// The run and task limits of `[limits]`, where they sit beside the hard limits. None is set
/// unless you set it, so nothing stops a run by default; spend and plan usage are counted and
/// shown either way.
///
/// Each limit's unit is in its key. The run's limit is one choice out of `spend_per_run`,
/// `run_tokens`, `run_plan_usage` and `run_minutes`, and so are the `per_task_*` keys: a layer
/// that sets one clears the ones it inherits, so a later layer can switch units. Two in the
/// same layer is an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BudgetSettings {
    /// Dollars one run may spend.
    pub spend_per_run: Option<f64>,
    pub run_tokens: Option<u64>,
    pub run_plan_usage: Option<f64>,
    pub run_minutes: Option<f64>,
    pub per_task_usd: Option<f64>,
    pub per_task_tokens: Option<u64>,
    pub per_task_plan_usage: Option<f64>,
    pub per_task_minutes: Option<f64>,
    /// How full a provider's plan window may get, 0 to 1, before its agents stop being used.
    /// Unset, a plan is used until its provider says it's out.
    pub plan_usage: Option<f64>,
    /// Share of any limit that sends the wrap-up message.
    pub soft_stop: f64,
    pub when_reached: OnHit,
    /// Use the median spend of past tasks of the same type, once there are
    /// `history_min` of them. Until then, `per_task_*`. Only for a run that has a limit.
    pub from_history: bool,
    pub history_min: u32,
    /// How the run budget splits. Shares add up to 1.
    pub lines: BudgetLines,
}

impl Default for BudgetSettings {
    fn default() -> Self {
        Self {
            spend_per_run: None,
            run_tokens: None,
            run_plan_usage: None,
            run_minutes: None,
            per_task_usd: None,
            per_task_tokens: None,
            per_task_plan_usage: None,
            per_task_minutes: None,
            plan_usage: None,
            soft_stop: 0.85,
            when_reached: OnHit::Fallback,
            from_history: true,
            history_min: 5,
            lines: BudgetLines::default(),
        }
    }
}

impl BudgetSettings {
    /// Its keys below `limits.`, which tell the run and task limits from the hard limits
    /// beside them: `/limits` shows these, and `/permissions` the others.
    pub const KEYS: &[&str] = &[
        "spend_per_run",
        "run_tokens",
        "run_plan_usage",
        "run_minutes",
        "per_task_usd",
        "per_task_tokens",
        "per_task_plan_usage",
        "per_task_minutes",
        "plan_usage",
        "soft_stop",
        "when_reached",
        "from_history",
        "history_min",
        "lines",
    ];

    /// The day's limit is kept with the hard limits, since approvals read it there, and is
    /// shown with these.
    const SPEND_PER_DAY: &str = "spend_per_day";

    /// Whether a dotted key, such as `limits.lines.review`, is one of them.
    pub fn owns(key: &str) -> bool {
        key.strip_prefix("limits.")
            .and_then(|below| below.split('.').next())
            .is_some_and(|name| name == Self::SPEND_PER_DAY || Self::KEYS.contains(&name))
    }

    /// The run's own limit, in the unit you set it in.
    pub fn run_limit(&self) -> Option<BudgetAmount> {
        one_of(
            self.spend_per_run,
            self.run_tokens,
            self.run_plan_usage,
            self.run_minutes,
        )
    }

    /// The limit of a task that has none of its own.
    pub fn task_limit(&self) -> Option<BudgetAmount> {
        one_of(
            self.per_task_usd,
            self.per_task_tokens,
            self.per_task_plan_usage,
            self.per_task_minutes,
        )
    }
}

/// The keys are one choice, so the first one set is the only one.
fn one_of(
    usd: Option<f64>,
    tokens: Option<u64>,
    plan_usage: Option<f64>,
    minutes: Option<f64>,
) -> Option<BudgetAmount> {
    let amount = |unit, amount| BudgetAmount { unit, amount };
    usd.map(|usd| amount(BudgetUnit::Usd, usd))
        .or_else(|| tokens.map(|tokens| amount(BudgetUnit::Tokens, tokens as f64)))
        .or_else(|| plan_usage.map(|share| amount(BudgetUnit::Quota, share)))
        .or_else(|| minutes.map(|minutes| amount(BudgetUnit::Minutes, minutes)))
}

/// What happens when a limit you set is reached. Neither waits for you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnHit {
    /// The fallbacks of `[fallback]` take over. With none that can, it stops. A run at its
    /// own limit pauses for you, except on autopilot, where it ends with its work kept.
    Fallback,
    /// The task stops with its work kept, and so does a run at its own limit.
    Stop,
}

/// Shares of the run budget. The defaults are placeholders until real runs calibrate them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BudgetLines {
    pub goal_and_plan: f64,
    pub experiments: f64,
    /// Interface and acceptance-test tasks.
    pub contracts: f64,
    pub tasks: f64,
    pub review: f64,
    pub integration: f64,
    pub reserve: f64,
}

impl Default for BudgetLines {
    fn default() -> Self {
        Self {
            goal_and_plan: 0.15,
            experiments: 0.10,
            contracts: 0.10,
            tasks: 0.40,
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

/// Watchdogs, per task unless the task overrides them. The two caps on a task's work are off
/// until you set them. The other two catch a session that hangs or loops, so they stay on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WatchdogSettings {
    /// Tool calls per task, subagents included.
    pub max_tool_calls: Option<u64>,
    /// Minutes a task may run.
    pub wall_clock_min: Option<f64>,
    /// Minutes with no events, CPU, or output from the process tree. Above the CLIs'
    /// own 10-minute limits, so they time out first.
    pub inactivity_min: f64,
    /// The same call this many times in a row gets a nudge; again, a soft stop.
    pub repeat_limit: u32,
}

impl Default for WatchdogSettings {
    fn default() -> Self {
        Self {
            max_tool_calls: None,
            wall_clock_min: None,
            inactivity_min: 15.0,
            repeat_limit: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_list_names_every_field() {
        let serde_json::Value::Object(fields) =
            serde_json::to_value(BudgetSettings::default()).unwrap()
        else {
            panic!("the limits are a table");
        };
        let mut fields: Vec<&str> = fields.keys().map(String::as_str).collect();
        let mut listed = BudgetSettings::KEYS.to_vec();
        fields.sort_unstable();
        listed.sort_unstable();
        assert_eq!(fields, listed);
    }

    #[test]
    fn a_key_is_owned_by_its_first_name_below_limits() {
        assert!(BudgetSettings::owns("limits.spend_per_run"));
        assert!(BudgetSettings::owns("limits.plan_usage"));
        assert!(BudgetSettings::owns("limits.lines.review"));
        assert!(BudgetSettings::owns("limits.spend_per_day"));
        assert!(!BudgetSettings::owns("limits.auto_merges_per_day"));
        assert!(!BudgetSettings::owns("limits"));
        assert!(!BudgetSettings::owns("budget.spend_per_run"));
    }
}
