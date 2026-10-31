//! The run's spend against `limit.run_*`: each session's meter readings added up and split
//! into the limit lines. The host records it as `Limit(Run)`, which the scheduler reads to
//! defer tasks and the screen reads for the spend against the limit. Once the run's limit is
//! spent, the host pauses the run, since only you raise a ceiling.
//!
//! A session spends most of what it costs to open in its first request, before any reading
//! can stop it, so the meter also learns what a session of each worker costs to open, and the
//! host starts none that what's left of the limit can't hold.

use std::collections::{BTreeMap, HashMap};

use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::models::meter::Spend;
use argonauts_core::runs::events::agent_status::{LineUse, RunSpend};
use argonauts_core::runs::events::plan::LimitPlan;
use argonauts_core::runs::events::run::RunLifecycle;
use argonauts_core::runs::events::session::{LimitEvent, SessionEvent};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::limit::{LimitAmount, LimitLineKind, LimitUnit, SpendLimits};
use argonauts_core::workflows::steps::StepName;
use time::{Duration, OffsetDateTime};

use crate::divide::limit_lines;

/// How often a session's readings may update the run's limit; an end always does.
const EVERY: Duration = Duration::SECOND;

#[derive(Debug, Default)]
struct SessionSpend {
    role: Option<RoleName>,
    step: Option<StepName>,
    /// How it opened: sessions alike cost about the same to open.
    opened_as: Option<Opener>,
    /// Started, and no reading of what it spent has come yet, so it holds what one like it
    /// costs to open.
    unread: bool,
    /// What its earlier processes spent. A resumed session's meter starts again from zero.
    banked: Spend,
    current: Spend,
}

/// The worker, its access, and whether it resumed a CLI session, whose first request mostly
/// reads what an earlier one wrote to the cache.
type Opener = (ModelRef, Access, bool);

/// A session the run's limit can't hold: what's left of it, and what one like it costs to open
/// plus what the sessions started and not yet heard from will take.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoRoom {
    pub left: LimitAmount,
    pub opening: f64,
    pub held: f64,
}

#[derive(Debug, Default)]
pub struct RunMeter {
    /// `None` when no run limit is set: the meter still counts.
    plan: Option<LimitPlan>,
    sessions: BTreeMap<SessionKey, SessionSpend>,
    last_sent: Option<OffsetDateTime>,
    /// What went out last, so a reading that changes nothing isn't recorded again.
    last_limit: Option<RunSpend>,
    /// A pause for the spent limit is under way, so no second one starts.
    holding: bool,
    /// The least a session that opened each way cost to open in this run, in the limit's unit:
    /// its first reading, or all it spent by its end.
    opening: HashMap<Opener, f64>,
}

impl RunMeter {
    pub fn new(settings: &SpendLimits) -> Self {
        let plan = limit_lines::plan(settings).ok().flatten();
        Self {
            plan,
            ..Self::default()
        }
    }

    /// Folds one event. Returns the run's limit when it's due and is different from the last one
    /// returned: at a session's end, or at most once a second while sessions report spend.
    pub fn observe(&mut self, event: &RunEvent) -> Option<RunSpend> {
        let ended = match &event.kind {
            RunEventKind::Run(RunLifecycle::Resumed(_) | RunLifecycle::PauseCancelled) => {
                self.holding = false;
                return None;
            }
            RunEventKind::Session(session) => match session {
                SessionEvent::Started(started) => {
                    let entry = self.session(event)?;
                    entry.role = Some(started.role);
                    entry.step = started.step;
                    entry.opened_as = Some((
                        started.worker.clone(),
                        started.access,
                        started.resumed_from.is_some(),
                    ));
                    entry.unread = true;
                    entry.current = Spend::default();
                    return None;
                }
                SessionEvent::Limit(LimitEvent::Spend(status)) => {
                    let first = self.session(event)?.unread;
                    let spent = self.spent_amount(&status.spend);
                    let entry = self.session(event)?;
                    entry.current = status.spend;
                    if first && spent > 0.0 {
                        entry.unread = false;
                        let opener = entry.opened_as.clone();
                        self.learn_opening(opener, spent);
                    }
                    false
                }
                SessionEvent::Ended(end) => {
                    let spent = self.spent_amount(&end.spend);
                    let entry = self.session(event)?;
                    entry.banked = add(&entry.banked, &end.spend);
                    entry.current = Spend::default();
                    entry.unread = false;
                    let opener = entry.opened_as.clone();
                    if spent > 0.0 {
                        self.learn_opening(opener, spent);
                    }
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
        let limit = self.limit();
        if self.last_limit.as_ref() == Some(&limit) {
            return None;
        }
        self.last_sent = Some(event.ts);
        self.last_limit = Some(limit.clone());
        Some(limit)
    }

    fn session(&mut self, event: &RunEvent) -> Option<&mut SessionSpend> {
        let key = event.session.clone()?;
        let entry = self.sessions.entry(key).or_default();
        // The narrator has its own cap and never draws on the run's limit.
        (entry.role != Some(RoleName::Narrator)).then_some(entry)
    }

    pub fn limit(&self) -> RunSpend {
        let mut spent = Spend::default();
        let mut by_line: HashMap<LimitLineKind, Spend> = HashMap::new();
        for session in self.sessions.values() {
            if session.role == Some(RoleName::Narrator) {
                continue;
            }
            let total = add(&session.banked, &session.current);
            spent = add(&spent, &total);
            let line = line_of(session.role, session.step);
            let sum = add(by_line.get(&line).unwrap_or(&Spend::default()), &total);
            by_line.insert(line, sum);
        }
        let Some(plan) = &self.plan else {
            return RunSpend {
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
            .filter(|line| line.line != LimitLineKind::Reserve)
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
            .find(|line| line.line == LimitLineKind::Reserve)
            .map(|line| line.amount);
        let left_in_run = (plan.total.amount - amount(&spent, unit)).max(0.0);
        let reserve_left = reserve.map(|reserve| LimitAmount {
            unit,
            amount: (reserve.amount - over).max(0.0).min(left_in_run),
        });
        if let Some(reserve) = reserve {
            lines.push(LineUse {
                line: LimitLineKind::Reserve,
                allocated: reserve,
                used: over,
            });
        }
        RunSpend {
            spent,
            limit: Some(plan.total),
            lines,
            reserve_left,
        }
    }

    /// What's left of the run's limit, in its unit, when the meter can count that unit.
    pub fn left(&self) -> Option<LimitAmount> {
        let total = self.plan.as_ref()?.total;
        metered(total.unit).then(|| LimitAmount {
            unit: total.unit,
            amount: (total.amount - amount(&self.limit().spent, total.unit)).max(0.0),
        })
    }

    /// True once, when the run's limit is spent and no pause for it is under way yet.
    pub fn start_holding(&mut self) -> bool {
        if self.left().is_none_or(|left| left.amount > 0.0) {
            return false;
        }
        self.hold()
    }

    /// True once while no pause for the run's limit is under way, so only one starts.
    pub fn hold(&mut self) -> bool {
        if self.holding {
            return false;
        }
        self.holding = true;
        true
    }

    /// Whether what's left of the run's limit can't hold a session of `worker` with `access`
    /// starting now, fresh or resuming a CLI session: less is left than one like it costs to
    /// open, with what the sessions started and not yet heard from will take. One like no
    /// session that opened in this run yet always starts.
    pub fn no_room_for(&self, worker: &ModelRef, access: Access, resumes: bool) -> Option<NoRoom> {
        let left = self.left()?;
        let opening = *self.opening.get(&(worker.clone(), access, resumes))?;
        let held = self
            .sessions
            .values()
            .filter(|session| session.unread && session.role != Some(RoleName::Narrator))
            .filter_map(|session| session.opened_as.as_ref())
            .filter_map(|opener| self.opening.get(opener))
            .sum();
        (left.amount < opening + held).then_some(NoRoom {
            left,
            opening,
            held,
        })
    }

    fn learn_opening(&mut self, opener: Option<Opener>, spent: f64) {
        let Some(opener) = opener else {
            return;
        };
        let least = self.opening.entry(opener).or_insert(spent);
        *least = least.min(spent);
    }

    /// `spend` in the run limit's unit, or nothing when the run has none the meter counts.
    fn spent_amount(&self, spend: &Spend) -> f64 {
        self.plan
            .as_ref()
            .map(|plan| plan.total.unit)
            .filter(|unit| metered(*unit))
            .map_or(0.0, |unit| amount(spend, unit))
    }
}

/// Plan usage and minutes are read elsewhere: the meter counts dollars and tokens.
fn metered(unit: LimitUnit) -> bool {
    matches!(unit, LimitUnit::Usd | LimitUnit::Tokens)
}

fn amount(spend: &Spend, unit: LimitUnit) -> f64 {
    match unit {
        LimitUnit::Usd => spend.cost_usd,
        LimitUnit::Tokens => spend.billable_tokens() as f64,
        LimitUnit::PlanUsage | LimitUnit::Minutes => 0.0,
    }
}

fn line_of(role: Option<RoleName>, step: Option<StepName>) -> LimitLineKind {
    match (role, step) {
        (Some(RoleName::Worker), _) => LimitLineKind::Tasks,
        (Some(RoleName::Reviewer), _) | (_, Some(StepName::Settle)) => LimitLineKind::Review,
        (Some(RoleName::Experiment), _) => LimitLineKind::Experiments,
        (Some(RoleName::RunReview), _) => LimitLineKind::Merge,
        _ => LimitLineKind::GoalAndPlan,
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
            "seq": seq, "ts": ts, "run_id": "run-1", "task_id": null, "session": session,
            "source": "argonauts-code", "kind": {"type": "session", "data": data},
        }))
        .unwrap()
    }

    fn started(seq: u64, session: &str, role: &str) -> RunEvent {
        event(
            seq,
            0,
            session,
            json!({
                "event": "started", "worker": "claude-code", "agent_id": "claude-code",
                "provider": "anthropic", "kind": "cli", "role": role, "step": "build",
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
                "event": "limit", "kind": "spend", "level": "continue",
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
                "event": "ended", "cli_session": null, "exit_code": 0, "signal": null,
                "duration_ms": 1000, "reason": {"why": "completed"}, "spend": spend(usd),
                "tool_calls": 1, "result_text": null, "structured": null,
            }),
        )
    }

    fn meter(spend_per_run: f64) -> RunMeter {
        RunMeter::new(&SpendLimits {
            spend_per_run: Some(spend_per_run),
            ..SpendLimits::default()
        })
    }

    /// A session of Haiku 5.5 on Claude Code, read-only, fresh or resuming a CLI session.
    fn opened(seq: u64, session: &str, resumes: bool) -> RunEvent {
        event(
            seq,
            0,
            session,
            json!({
                "event": "started", "worker": "claude-code/haiku", "agent_id": "claude-code",
                "provider": "anthropic", "kind": "cli", "role": "draft", "step": null,
                "access": "read_only", "lifetime": "task",
                "resumed_from": resumes.then_some("cc-1"), "follow_key": null,
                "workflow_step": null,
            }),
        )
    }

    fn haiku() -> ModelRef {
        "claude-code/haiku".parse().unwrap()
    }

    /// The council run under its $0.60 limit: the planner's first request was billed $0.3619
    /// and its session $0.3745, which leaves $0.2255, less than another one costs to open.
    #[test]
    fn a_session_that_would_cross_the_limit_has_no_room() {
        let mut meter = meter(0.60);
        meter.observe(&opened(1, "s-001", false));
        // A reading before any request says nothing of what a session costs to open.
        meter.observe(&reading(2, 1, "s-001", 0.0));
        assert_eq!(meter.no_room_for(&haiku(), Access::ReadOnly, false), None);
        meter.observe(&reading(3, 2, "s-001", 0.361891));
        meter.observe(&ended(4, 3, "s-001", 0.3745));
        let no_room = meter
            .no_room_for(&haiku(), Access::ReadOnly, false)
            .expect("no room");
        assert!((no_room.left.amount - 0.2255).abs() < 1e-9, "{no_room:?}");
        assert!((no_room.opening - 0.361891).abs() < 1e-9, "{no_room:?}");
        assert_eq!(no_room.held, 0.0);
        // Another worker, another access, or a resumed session opens its own way.
        let codex: ModelRef = "codex/gpt-6-luna".parse().unwrap();
        assert_eq!(meter.no_room_for(&codex, Access::ReadOnly, false), None);
        assert_eq!(meter.no_room_for(&haiku(), Access::Build, false), None);
        assert_eq!(meter.no_room_for(&haiku(), Access::ReadOnly, true), None);
    }

    /// Two sessions started together can't both count on one remainder: one started and not
    /// heard from yet holds what it costs to open.
    #[test]
    fn a_session_started_and_not_heard_from_holds_its_opening() {
        let mut meter = meter(1.0);
        meter.observe(&opened(1, "s-001", false));
        meter.observe(&reading(2, 1, "s-001", 0.36));
        meter.observe(&ended(3, 2, "s-001", 0.37));
        assert_eq!(meter.no_room_for(&haiku(), Access::ReadOnly, false), None);
        meter.observe(&opened(4, "s-002", false));
        let no_room = meter
            .no_room_for(&haiku(), Access::ReadOnly, false)
            .expect("no room");
        assert!((no_room.held - 0.36).abs() < 1e-9, "{no_room:?}");
        // Once it has read what it spent, that's in the run's spend instead.
        meter.observe(&reading(5, 3, "s-002", 0.20));
        assert_eq!(meter.no_room_for(&haiku(), Access::ReadOnly, false), None);
        // A resumed session learns its own opening, the least that one like it spent.
        meter.observe(&opened(6, "s-003", true));
        meter.observe(&reading(7, 4, "s-003", 0.02));
        meter.observe(&ended(8, 5, "s-003", 0.03));
        assert_eq!(meter.no_room_for(&haiku(), Access::ReadOnly, true), None);
        let mut nearly_spent = super::RunMeter::new(&SpendLimits {
            spend_per_run: Some(0.39),
            ..SpendLimits::default()
        });
        nearly_spent.observe(&opened(1, "s-001", false));
        nearly_spent.observe(&ended(2, 1, "s-001", 0.37));
        nearly_spent.observe(&opened(3, "s-002", true));
        nearly_spent.observe(&ended(4, 2, "s-002", 0.03));
        assert!(
            nearly_spent
                .no_room_for(&haiku(), Access::ReadOnly, true)
                .is_some()
        );
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
        assert!((line(LimitLineKind::Tasks) - 0.30).abs() < 1e-9);
        assert!((line(LimitLineKind::Review) - 0.10).abs() < 1e-9);
        assert_eq!(both.limit.unwrap().amount, 1.0);
    }

    #[test]
    fn an_unchanged_limit_is_not_sent_again() {
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
        assert_eq!(meter.limit().spent.cost_usd, 0.0);
    }

    #[test]
    fn a_spent_limit_holds_once_until_the_run_resumes() {
        let mut meter = meter(0.02);
        meter.observe(&started(1, "s-001", "worker"));
        let limit = meter.observe(&ended(2, 1, "s-001", 0.05)).unwrap();
        assert_eq!(limit.reserve_left.unwrap().amount, 0.0);
        assert_eq!(meter.left().unwrap().amount, 0.0);
        assert!(meter.start_holding());
        assert!(!meter.start_holding(), "one pause at a time");
        let resumed: RunEvent = serde_json::from_value(json!({
            "seq": 3, "ts": "1970-01-01T00:00:05Z", "run_id": "run-1", "task_id": null,
            "session": null, "source": "argonauts-code",
            "kind": {"type": "run", "data": {"event": "pause_cancelled"}},
        }))
        .unwrap();
        meter.observe(&resumed);
        assert!(meter.start_holding(), "still spent after you carry on");
    }

    #[test]
    fn with_no_limit_the_meter_counts_but_never_holds() {
        let mut meter = RunMeter::new(&SpendLimits::default());
        meter.observe(&started(1, "s-001", "worker"));
        let limit = meter.observe(&ended(2, 1, "s-001", 5.0)).unwrap();
        assert_eq!(limit.limit, None);
        assert!((limit.spent.cost_usd - 5.0).abs() < 1e-9);
        assert!(meter.left().is_none());
        assert!(!meter.start_holding());
    }
}
