//! The run's spend against `budget.run_*`: each session's meter readings added up and split
//! into the budget lines. The host records it as `Budget(Run)`, which the scheduler reads to
//! defer cards and the screen reads for the spend against the limit. Once the run's budget is
//! spent, the host pauses the run, since only you raise a ceiling.

use std::collections::{BTreeMap, HashMap};

use argonauts_core::budget::Spend;
use argonauts_core::events::plan::BudgetPlan;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::{LimitEvent, SessionEvent};
use argonauts_core::events::vendor::{LineUse, RunBudget};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::settings::budget::{BudgetAmount, BudgetLineKind, BudgetSettings, BudgetUnit};
use time::{Duration, OffsetDateTime};

use crate::divide::budget_lines;

/// How often a session's readings may update the run's budget; an end always does.
const EVERY: Duration = Duration::SECOND;

#[derive(Debug, Default)]
struct SessionSpend {
    role: Option<RoleName>,
    /// What its earlier processes spent. A resumed session's meter starts again from zero.
    banked: Spend,
    current: Spend,
}

#[derive(Debug, Default)]
pub struct RunMeter {
    /// `None` when no run limit is set: the meter still counts.
    plan: Option<BudgetPlan>,
    sessions: BTreeMap<SessionKey, SessionSpend>,
    last_sent: Option<OffsetDateTime>,
    /// What went out last, so a reading that changes nothing isn't recorded again.
    last_budget: Option<RunBudget>,
    /// A pause for the spent budget is under way, so no second one starts.
    holding: bool,
}

impl RunMeter {
    pub fn new(settings: &BudgetSettings) -> Self {
        let plan = budget_lines::plan(settings).ok().flatten();
        Self {
            plan,
            ..Self::default()
        }
    }

    /// Folds one event. Returns the run's budget when it's due and differs from the last one
    /// returned: at a session's end, or at most once a second while sessions report spend.
    pub fn observe(&mut self, event: &RunEvent) -> Option<RunBudget> {
        let ended = match &event.kind {
            RunEventKind::Run(RunLifecycle::Resumed(_) | RunLifecycle::PauseCancelled) => {
                self.holding = false;
                return None;
            }
            RunEventKind::Session(session) => match session {
                SessionEvent::Started(started) => {
                    let entry = self.session(event)?;
                    entry.role = Some(started.role);
                    entry.current = Spend::default();
                    return None;
                }
                SessionEvent::Limit(LimitEvent::Budget(status)) => {
                    self.session(event)?.current = status.spend;
                    false
                }
                SessionEvent::Ended(end) => {
                    let entry = self.session(event)?;
                    entry.banked = add(&entry.banked, &end.spend);
                    entry.current = Spend::default();
                    true
                }
                _ => return None,
            },
            _ => return None,
        };
        let due = self.last_sent.is_none_or(|last| event.ts - last >= EVERY);
        if !(ended || due) {
            return None;
        }
        let budget = self.budget();
        if self.last_budget.as_ref() == Some(&budget) {
            return None;
        }
        self.last_sent = Some(event.ts);
        self.last_budget = Some(budget.clone());
        Some(budget)
    }

    fn session(&mut self, event: &RunEvent) -> Option<&mut SessionSpend> {
        let key = event.session.clone()?;
        let entry = self.sessions.entry(key).or_default();
        // The narrator has its own cap and never draws on the run's budget.
        (entry.role != Some(RoleName::Narrator)).then_some(entry)
    }

    pub fn budget(&self) -> RunBudget {
        let mut spent = Spend::default();
        let mut by_line: HashMap<BudgetLineKind, Spend> = HashMap::new();
        for session in self.sessions.values() {
            if session.role == Some(RoleName::Narrator) {
                continue;
            }
            let total = add(&session.banked, &session.current);
            spent = add(&spent, &total);
            let line = line_of(session.role);
            let sum = add(by_line.get(&line).unwrap_or(&Spend::default()), &total);
            by_line.insert(line, sum);
        }
        let Some(plan) = &self.plan else {
            return RunBudget {
                spent,
                limit: None,
                lines: Vec::new(),
                reserve_left: None,
            };
        };
        let unit = plan.total.unit;
        let mut lines: Vec<LineUse> = plan
            .lines
            .iter()
            .filter(|line| line.line != BudgetLineKind::Reserve)
            .map(|line| LineUse {
                line: line.line,
                allocated: line.amount,
                used: by_line
                    .get(&line.line)
                    .map_or(0.0, |spend| amount(spend, unit)),
            })
            .collect();
        // What the other lines spent past their share comes out of the reserve.
        let over: f64 = lines
            .iter()
            .map(|line| (line.used - line.allocated.amount).max(0.0))
            .sum();
        let reserve = plan
            .lines
            .iter()
            .find(|line| line.line == BudgetLineKind::Reserve)
            .map(|line| line.amount);
        let left_in_run = (plan.total.amount - amount(&spent, unit)).max(0.0);
        let reserve_left = reserve.map(|reserve| BudgetAmount {
            unit,
            amount: (reserve.amount - over).max(0.0).min(left_in_run),
        });
        if let Some(reserve) = reserve {
            lines.push(LineUse {
                line: BudgetLineKind::Reserve,
                allocated: reserve,
                used: over,
            });
        }
        RunBudget {
            spent,
            limit: Some(plan.total),
            lines,
            reserve_left,
        }
    }

    /// What's left of the run's limit, in its unit, when the meter can count that unit.
    pub fn left(&self) -> Option<BudgetAmount> {
        let total = self.plan.as_ref()?.total;
        metered(total.unit).then(|| BudgetAmount {
            unit: total.unit,
            amount: (total.amount - amount(&self.budget().spent, total.unit)).max(0.0),
        })
    }

    /// True once, when the run's limit is spent and no pause for it is under way yet.
    pub fn start_holding(&mut self) -> bool {
        if self.holding || self.left().is_none_or(|left| left.amount > 0.0) {
            return false;
        }
        self.holding = true;
        true
    }
}

/// Quota and minutes are read elsewhere: the meter counts dollars and tokens.
fn metered(unit: BudgetUnit) -> bool {
    matches!(unit, BudgetUnit::Usd | BudgetUnit::Tokens)
}

fn amount(spend: &Spend, unit: BudgetUnit) -> f64 {
    match unit {
        BudgetUnit::Usd => spend.cost_usd,
        BudgetUnit::Tokens => spend.billable_tokens() as f64,
        BudgetUnit::Quota | BudgetUnit::Minutes => 0.0,
    }
}

fn line_of(role: Option<RoleName>) -> BudgetLineKind {
    match role {
        Some(RoleName::Worker) => BudgetLineKind::Cards,
        Some(RoleName::Reviewer | RoleName::Adjudicator) => BudgetLineKind::Review,
        Some(RoleName::Spike) => BudgetLineKind::Spikes,
        Some(RoleName::Retro) => BudgetLineKind::Integration,
        _ => BudgetLineKind::GoalAndPlan,
    }
}

fn add(a: &Spend, b: &Spend) -> Spend {
    Spend {
        input_tokens: a.input_tokens + b.input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        cached_tokens: a.cached_tokens + b.cached_tokens,
        cost_usd: a.cost_usd + b.cost_usd,
        cost_partial: a.cost_partial || b.cost_partial,
        cost_estimated: a.cost_estimated || b.cost_estimated,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(seq: u64, secs: i64, session: &str, data: serde_json::Value) -> RunEvent {
        let ts = OffsetDateTime::UNIX_EPOCH + Duration::seconds(secs);
        let ts = ts
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        serde_json::from_value(json!({
            "seq": seq, "ts": ts, "run_id": "run-1", "card_id": null, "session": session,
            "source": "conductor", "kind": {"type": "session", "data": data},
        }))
        .unwrap()
    }

    fn started(seq: u64, session: &str, role: &str) -> RunEvent {
        event(
            seq,
            0,
            session,
            json!({
                "event": "started", "worker": "claude-code", "vendor": "claude-code",
                "family": "claude", "kind": "harness", "role": role, "step": "build",
                "access": "build", "lifetime": "task", "resumed_from": null, "follow_key": null,
                "workflow_step": null,
            }),
        )
    }

    fn spend(usd: f64) -> serde_json::Value {
        json!({"input_tokens": 1000, "output_tokens": 10, "cached_tokens": 0, "cost_usd": usd,
               "cost_partial": false, "cost_estimated": false})
    }

    fn reading(seq: u64, secs: i64, session: &str, usd: f64) -> RunEvent {
        event(
            seq,
            secs,
            session,
            json!({
                "event": "limit", "kind": "budget", "level": "continue",
                "limit": {"unit": "usd", "amount": 0.5}, "spend": spend(usd),
                "fraction_used": 0.1, "work": {"minutes": 0.1, "tool_calls": 0},
                "work_limits": {"max_minutes": 30.0, "max_tool_calls": 150}, "binding": "minutes",
            }),
        )
    }

    fn ended(seq: u64, secs: i64, session: &str, usd: f64) -> RunEvent {
        event(
            seq,
            secs,
            session,
            json!({
                "event": "ended", "vendor_session": null, "exit_code": 0, "signal": null,
                "duration_ms": 1000, "reason": {"why": "completed"}, "spend": spend(usd),
                "tool_calls": 1, "result_text": null, "structured": null,
            }),
        )
    }

    fn meter(spend_per_run: f64) -> RunMeter {
        RunMeter::new(&BudgetSettings {
            spend_per_run: Some(spend_per_run),
            ..BudgetSettings::default()
        })
    }

    #[test]
    fn readings_add_up_across_sessions_and_resumes_at_most_once_a_second() {
        let mut meter = meter(1.0);
        assert!(meter.observe(&started(1, "s-001", "worker")).is_none());
        let first = meter.observe(&reading(2, 1, "s-001", 0.10)).unwrap();
        assert!((first.spent.cost_usd - 0.10).abs() < 1e-9);
        // Within the second: counted, not sent.
        assert!(meter.observe(&reading(3, 1, "s-001", 0.20)).is_none());
        // An end always sends, and a resumed process starts its meter from zero again.
        let after_end = meter.observe(&ended(4, 1, "s-001", 0.25)).unwrap();
        assert!((after_end.spent.cost_usd - 0.25).abs() < 1e-9);
        meter.observe(&started(5, "s-001", "worker"));
        let resumed = meter.observe(&reading(6, 3, "s-001", 0.05)).unwrap();
        assert!((resumed.spent.cost_usd - 0.30).abs() < 1e-9);
        meter.observe(&started(7, "s-002", "reviewer"));
        let both = meter.observe(&ended(8, 4, "s-002", 0.10)).unwrap();
        assert!((both.spent.cost_usd - 0.40).abs() < 1e-9);
        let line = |kind| both.lines.iter().find(|l| l.line == kind).unwrap().used;
        assert!((line(BudgetLineKind::Cards) - 0.30).abs() < 1e-9);
        assert!((line(BudgetLineKind::Review) - 0.10).abs() < 1e-9);
        assert_eq!(both.limit.unwrap().amount, 1.0);
    }

    #[test]
    fn an_unchanged_budget_is_not_sent_again() {
        let mut meter = meter(1.0);
        meter.observe(&started(1, "s-001", "worker"));
        assert!(meter.observe(&reading(2, 1, "s-001", 0.10)).is_some());
        assert!(meter.observe(&reading(3, 5, "s-001", 0.10)).is_none());
        assert!(meter.observe(&ended(4, 6, "s-001", 0.10)).is_none());
        meter.observe(&started(5, "s-001", "worker"));
        let moved = meter.observe(&reading(6, 8, "s-001", 0.05)).unwrap();
        assert!((moved.spent.cost_usd - 0.15).abs() < 1e-9);
        assert!(meter.observe(&reading(7, 10, "s-001", 0.05)).is_none());
    }

    #[test]
    fn the_narrator_never_counts() {
        let mut meter = meter(1.0);
        meter.observe(&started(1, "s-001", "narrator"));
        assert!(meter.observe(&ended(2, 1, "s-001", 0.40)).is_none());
        assert_eq!(meter.budget().spent.cost_usd, 0.0);
    }

    #[test]
    fn a_spent_limit_holds_once_until_the_run_resumes() {
        let mut meter = meter(0.02);
        meter.observe(&started(1, "s-001", "worker"));
        let budget = meter.observe(&ended(2, 1, "s-001", 0.05)).unwrap();
        assert_eq!(budget.reserve_left.unwrap().amount, 0.0);
        assert_eq!(meter.left().unwrap().amount, 0.0);
        assert!(meter.start_holding());
        assert!(!meter.start_holding(), "one pause at a time");
        let resumed: RunEvent = serde_json::from_value(json!({
            "seq": 3, "ts": "1970-01-01T00:00:05Z", "run_id": "run-1", "card_id": null,
            "session": null, "source": "conductor",
            "kind": {"type": "run", "data": {"event": "pause_cancelled"}},
        }))
        .unwrap();
        meter.observe(&resumed);
        assert!(meter.start_holding(), "still spent after you carry on");
    }

    #[test]
    fn with_no_limit_the_meter_counts_but_never_holds() {
        let mut meter = RunMeter::new(&BudgetSettings::default());
        meter.observe(&started(1, "s-001", "worker"));
        let budget = meter.observe(&ended(2, 1, "s-001", 5.0)).unwrap();
        assert_eq!(budget.limit, None);
        assert!((budget.spent.cost_usd - 5.0).abs() < 1e-9);
        assert!(meter.left().is_none());
        assert!(!meter.start_holding());
    }
}
