//! A task that reached a limit you set. `limits.when_reached = "fallback"` gives it to a
//! fallback, and when none can take it opens the limit approval, where you (or your rules,
//! within the reserve) add to the limit, give it to a fallback, or stop it; in autopilot that
//! approval is settled without you. `"stop"` stops the task with its work kept. A reached
//! limit is never a failure and uses no retry.

use argonauts_core::TaskId;
use argonauts_core::base::names::RoleName;
use argonauts_core::models::meter::{
    LimitDimension, MeterAction, MeterLimit, SpendStatus, WorkLimits, WorkUsed,
};
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalKind, ApprovalPacket, FreeText, LimitPacket,
};
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::session::{EndReason, SessionEnd};
use argonauts_core::runs::events::task::{TaskEvent, TaskLimit, TaskSpec, TaskState};
use argonauts_core::runs::events::{RunEventKind, SpendEvent};
use argonauts_core::safety::policy::{EvidenceFacts, SpendFacts};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit, OnHit};

use crate::approvals::ApprovalRequest;
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::machine::Trigger;
use crate::task::pipeline::transition;
use crate::task::recovery;
use crate::task::stuck::wait_decision;
use crate::task::support::{called, newest_worker_session, record_of, record_task, worker_of};

/// How much "raise the limit" adds without an amount in your note.
const STEP: f64 = 0.5;

/// Applies `limits.when_reached` to a task waiting in `NeedsDecision`.
pub(crate) async fn on_hit(host: &Host, task: &TaskId, on_hit: OnHit) -> EngineResult<()> {
    match on_hit {
        OnHit::Fallback => {
            // Out of NeedsDecision at once, as an answered limit question does, or the pipeline
            // decides the old worker's stop again and falls back a second time.
            if fallback(host, task, Party::Rules)? {
                let reason = "fell back after a limit hit";
                transition(host, task, Trigger::Decided, reason)?;
                return Ok(());
            }
            ask(host, task).await
        }
        OnHit::Stop => {
            transition(host, task, Trigger::AskYou, "the task hit its limit")?;
            Ok(())
        }
    }
}

/// `Limit(Hit)`: the task's work is committed, and `limits.when_reached` decides next.
pub(crate) fn record_hit(host: &Host, task: &TaskId) -> EngineResult<()> {
    let record = record_of(host, task)?;
    let (dimension, status) = hit_of(host, &record);
    let hit = SpendEvent::Hit {
        task: task.clone(),
        dimension,
        status,
    };
    host.record(Some(task), None, RunEventKind::Spend(hit))?;
    Ok(())
}

/// What the task ran out of, from its worker's last session, and where its meters stood then.
fn hit_of(host: &Host, record: &TaskEntry) -> (LimitDimension, SpendStatus) {
    let task = &record.spec.id;
    let end = host.read(|state| newest_worker_session(state, task).and_then(|s| s.end.clone()));
    let dimension = end
        .as_ref()
        .and_then(|end| match end.reason {
            EndReason::LimitHit { dimension } => Some(dimension),
            _ => None,
        })
        .unwrap_or(LimitDimension::Spend);
    let limit = in_force(host, &record.spec);
    (dimension, status(&limit, end.as_ref(), dimension))
}

/// The limit the task's sessions run under: its own limits, and the settings' per-task ones
/// where it has none, as a task made without limits of its own goes by them.
fn in_force(host: &Host, spec: &TaskSpec) -> TaskLimit {
    let limits = host
        .services()
        .limits
        .session_limits(Some(spec), RoleName::Worker);
    let own = &spec.limit;
    TaskLimit {
        limit: own.limit.or(limits.limit),
        max_minutes: own.max_minutes.or(limits.max_minutes),
        max_tool_calls: own.max_tool_calls.or(limits.max_tool_calls),
        inactivity_min: own.inactivity_min,
    }
}

/// The limit approval, for a task no fallback can take. Your approval rules may answer it
/// from the reserve, and autopilot settles it without you.
async fn ask(host: &Host, task: &TaskId) -> EngineResult<()> {
    let record = record_of(host, task)?;
    let (dimension, status) = hit_of(host, &record);
    let reserve_left = host.read(|state| {
        state
            .run
            .spend
            .as_ref()
            .and_then(|limit| limit.reserve_left)
    });
    let packet = LimitPacket {
        task: Some(task.clone()),
        status,
        dimension,
        reserve_left,
    };
    let request = ApprovalRequest {
        at: None,
        kind: ApprovalKind::Limit,
        title: format!("{} hit its limit", called(&record)),
        task: Some(task.clone()),
        packet: ApprovalPacket::Limit(packet),
        facts: EvidenceFacts {
            spend: host.read(spend_facts),
            ..EvidenceFacts::default()
        },
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: None,
        free_text: FreeText::Answer,
        authors: vec![worker_of(host, &record)],
    };
    let Some(decided) = wait_decision(host, task, TaskState::NeedsDecision, request).await? else {
        return Ok(());
    };
    match &decided.action {
        AnswerAction::RaiseLimit | AnswerAction::Approve | AnswerAction::Retry => {
            let mut spec = record.spec.clone();
            let limit = in_force(host, &spec);
            spec.limit = extended(&limit, dimension, decided.text.as_deref());
            let more = added(&limit, &spec.limit, dimension);
            record_task(host, task, TaskEvent::Created(Box::new(spec)))?;
            if let Some(amount) = more {
                let event = SpendEvent::Extended {
                    task: task.clone(),
                    amount,
                    by: decided.by.clone(),
                };
                host.record(Some(task), None, RunEventKind::Spend(event))?;
            }
            transition(host, task, Trigger::Decided, "raise the limit")?;
        }
        AnswerAction::Fallback | AnswerAction::OtherCandidates | AnswerAction::Candidate { .. } => {
            let moved = match &decided.action {
                AnswerAction::Candidate { worker } => {
                    reassign(host, task, worker.clone(), decided.by.clone())?;
                    true
                }
                _ => fallback(host, task, decided.by.clone())?,
            };
            if moved {
                transition(host, task, Trigger::Decided, "fell back after a limit hit")?;
            } else {
                transition(host, task, Trigger::AskYou, "no other agent can take it")?;
            }
        }
        _ => {
            transition(
                host,
                task,
                Trigger::AskYou,
                "you asked to decide the limit hit",
            )?;
        }
    }
    Ok(())
}

fn fallback(host: &Host, task: &TaskId, by: Party) -> EngineResult<bool> {
    let record = record_of(host, task)?;
    match recovery::any_other(host, &record) {
        Some(to) => {
            reassign(host, task, to, by)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn reassign(
    host: &Host,
    task: &TaskId,
    to: argonauts_core::base::names::ModelRef,
    by: Party,
) -> EngineResult<()> {
    let record = record_of(host, task)?;
    let event = TaskEvent::Reassigned {
        from: Some(worker_of(host, &record)),
        to,
        by,
        rule_override: None,
    };
    record_task(host, task, event)?;
    Ok(())
}

/// What the tasks not yet done would cost, against what's left of the run's reserve. `None`,
/// which asks you, when the reserve or a task's limit isn't known in the reserve's unit.
pub(crate) fn spend_facts(state: &RunState) -> Option<SpendFacts> {
    let reserve_left = state.run.spend.as_ref()?.reserve_left?;
    let mut remaining = 0.0;
    for record in state.tasks.by_id.values() {
        if matches!(record.state, TaskState::Merged | TaskState::Cancelled) {
            continue;
        }
        let limit = record.spec.limit.limit?;
        if limit.unit != reserve_left.unit {
            return None;
        }
        remaining += limit.amount;
    }
    Some(SpendFacts {
        remaining_tasks: LimitAmount {
            unit: reserve_left.unit,
            amount: remaining,
        },
        reserve_left,
    })
}

/// Whether one more try at the task, at its own limit, fits in what's left of the
/// reserve. Always with limits off; `None` when either isn't known in the same unit.
pub(crate) fn fits_reserve(state: &RunState, record: &TaskEntry) -> Option<bool> {
    let limit = state.run.spend.as_ref()?;
    if limit.limit.is_none() {
        return Some(true);
    }
    let reserve = limit.reserve_left?;
    let cost = record.spec.limit.limit?;
    (cost.unit == reserve.unit).then_some(cost.amount <= reserve.amount)
}

/// The task's limit with more room on the dimension it hit: the amount in your note, or half
/// again what it had.
pub(crate) fn extended(
    limit: &TaskLimit,
    dimension: LimitDimension,
    note: Option<&str>,
) -> TaskLimit {
    let asked = note.and_then(|text| {
        text.trim()
            .trim_start_matches('$')
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<f64>().ok())
    });
    let grow = |current: f64| asked.unwrap_or(current * (1.0 + STEP));
    let mut limit = limit.clone();
    match dimension {
        LimitDimension::Minutes => {
            limit.max_minutes = limit.max_minutes.map(grow);
        }
        LimitDimension::ToolCalls => {
            limit.max_tool_calls = limit
                .max_tool_calls
                .map(|calls| grow(calls as f64).round() as u64);
        }
        LimitDimension::Spend => {
            limit.limit = limit.limit.as_ref().map(|limit| LimitAmount {
                unit: limit.unit,
                amount: grow(limit.amount),
            });
        }
    }
    limit
}

/// How much more `after` allows than `before` in `dimension`. A tool-call count has no limit
/// unit, so that extension shows only in the task's new limit.
fn added(before: &TaskLimit, after: &TaskLimit, dimension: LimitDimension) -> Option<LimitAmount> {
    let (unit, from, to) = match dimension {
        LimitDimension::Spend => {
            let (before, after) = (before.limit?, after.limit?);
            (after.unit, before.amount, after.amount)
        }
        LimitDimension::Minutes => (LimitUnit::Minutes, before.max_minutes?, after.max_minutes?),
        LimitDimension::ToolCalls => return None,
    };
    (to > from).then_some(LimitAmount {
        unit,
        amount: to - from,
    })
}

fn status(limit: &TaskLimit, end: Option<&SessionEnd>, dimension: LimitDimension) -> SpendStatus {
    SpendStatus {
        level: MeterAction::Stop,
        limit: limit
            .limit
            .as_ref()
            .map(|limit| match limit.unit {
                LimitUnit::Usd => MeterLimit::Usd(limit.amount),
                LimitUnit::Tokens => MeterLimit::Tokens(limit.amount.max(0.0).round() as u64),
                LimitUnit::PlanUsage | LimitUnit::Minutes => MeterLimit::None,
            })
            .unwrap_or_default(),
        spend: end.map(|end| end.spend).unwrap_or_default(),
        fraction_used: None,
        work: WorkUsed {
            minutes: end.map_or(0.0, |end| end.duration_ms as f64 / 60_000.0),
            tool_calls: end.map_or(0, |end| end.tool_calls),
        },
        work_limits: WorkLimits {
            max_minutes: limit.max_minutes,
            max_tool_calls: limit.max_tool_calls,
        },
        binding: Some(dimension),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::agent_status::RunSpend;

    use super::*;
    use crate::testing::task_spec;

    fn usd(amount: f64) -> LimitAmount {
        LimitAmount {
            unit: LimitUnit::Usd,
            amount,
        }
    }

    /// A run with a $10 limit and `reserve` left, and one task per entry of `tasks`: its state
    /// and its limit.
    fn run(reserve: Option<LimitAmount>, tasks: &[(TaskState, Option<LimitAmount>)]) -> RunState {
        let mut state = RunState::default();
        state.run.spend = Some(RunSpend {
            spent: Default::default(),
            limit: Some(usd(10.0)),
            lines: Vec::new(),
            reserve_left: reserve,
        });
        for (n, (task_state, limit)) in tasks.iter().enumerate() {
            let mut spec = task_spec(&format!("task-{:03}", n + 1), &[], None);
            spec.limit.limit = *limit;
            let mut record = TaskEntry::new(spec);
            record.state = *task_state;
            state.tasks.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    #[test]
    fn limit_facts_sum_the_tasks_not_yet_done() {
        let state = run(
            Some(usd(3.0)),
            &[
                (TaskState::Merged, Some(usd(5.0))),
                (TaskState::Cancelled, None),
                (TaskState::NeedsDecision, Some(usd(2.0))),
                (TaskState::Waiting, Some(usd(1.5))),
            ],
        );
        let facts = spend_facts(&state).unwrap();
        assert_eq!(facts.remaining_tasks, usd(3.5));
        assert_eq!(facts.reserve_left, usd(3.0));
    }

    #[test]
    fn limit_facts_are_missing_when_anything_is_unknown() {
        let no_reserve = run(None, &[(TaskState::Ready, Some(usd(1.0)))]);
        assert_eq!(spend_facts(&no_reserve), None);
        let unpriced = run(Some(usd(3.0)), &[(TaskState::Ready, None)]);
        assert_eq!(spend_facts(&unpriced), None);
        let tokens = LimitAmount {
            unit: LimitUnit::Tokens,
            amount: 1_000.0,
        };
        let other_unit = run(Some(usd(3.0)), &[(TaskState::Ready, Some(tokens))]);
        assert_eq!(spend_facts(&other_unit), None);
        assert_eq!(spend_facts(&RunState::default()), None);
    }

    #[test]
    fn one_more_try_fits_the_reserve_at_the_tasks_limit() {
        let task = |state: &RunState| state.tasks.by_id.values().next().unwrap().clone();
        let fits = run(Some(usd(3.0)), &[(TaskState::NeedsYou, Some(usd(2.0)))]);
        assert_eq!(fits_reserve(&fits, &task(&fits)), Some(true));
        let too_much = run(Some(usd(1.0)), &[(TaskState::NeedsYou, Some(usd(2.0)))]);
        assert_eq!(fits_reserve(&too_much, &task(&too_much)), Some(false));
        let unknown = run(None, &[(TaskState::NeedsYou, Some(usd(2.0)))]);
        assert_eq!(fits_reserve(&unknown, &task(&unknown)), None);
        let mut off = run(None, &[(TaskState::NeedsYou, None)]);
        off.run.spend.as_mut().unwrap().limit = None;
        assert_eq!(fits_reserve(&off, &task(&off)), Some(true));
    }

    #[test]
    fn more_limit_takes_your_amount_or_half_again() {
        let limit = TaskLimit {
            limit: Some(LimitAmount {
                unit: LimitUnit::Usd,
                amount: 2.0,
            }),
            max_minutes: Some(30.0),
            max_tool_calls: Some(100),
            inactivity_min: None,
        };
        let more = extended(&limit, LimitDimension::Spend, None);
        assert_eq!(more.limit.unwrap().amount, 3.0);
        assert_eq!(
            more.max_minutes,
            Some(30.0),
            "only the dimension that hit grows"
        );
        let asked = extended(&limit, LimitDimension::Spend, Some("$5 please"));
        assert_eq!(asked.limit.unwrap().amount, 5.0);
        assert_eq!(
            extended(&limit, LimitDimension::Minutes, None).max_minutes,
            Some(45.0)
        );
        assert_eq!(
            extended(&limit, LimitDimension::ToolCalls, Some("250")).max_tool_calls,
            Some(250)
        );
    }

    #[test]
    fn the_extension_is_what_the_hit_dimension_gained() {
        let limit = TaskLimit {
            limit: Some(usd(2.0)),
            max_minutes: Some(30.0),
            max_tool_calls: Some(100),
            inactivity_min: None,
        };
        let more = |dimension, note| {
            let after = extended(&limit, dimension, note);
            added(&limit, &after, dimension)
        };
        assert_eq!(more(LimitDimension::Spend, None), Some(usd(1.0)));
        assert_eq!(
            more(LimitDimension::Minutes, None),
            Some(LimitAmount {
                unit: LimitUnit::Minutes,
                amount: 15.0,
            })
        );
        assert_eq!(more(LimitDimension::ToolCalls, Some("250")), None);
        // Less than it had is no extension.
        assert_eq!(more(LimitDimension::Spend, Some("$1")), None);
    }
}
