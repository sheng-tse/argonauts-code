//! A card that hit its budget: `budget.on_hit` hands it off or escalates it by rule, or opens
//! the budget checkpoint, where you (or your rules, within the reserve) add budget, hand it
//! off, or escalate. A budget hit is never a failure and uses no retry.

use argonauts_core::CardId;
use argonauts_core::budget::{
    BudgetAction, BudgetDimension, BudgetLimit, BudgetStatus, WorkLimits, WorkUsed,
};
use argonauts_core::events::card::{CardBudget, CardEvent, CardSpec, CardState};
use argonauts_core::events::checkpoint::{
    AnswerAction, BudgetPacket, CheckpointKind, CheckpointPacket, FreeText,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::session::{EndReason, SessionEnd};
use argonauts_core::events::{BudgetEvent, RunEventKind};
use argonauts_core::names::RoleName;
use argonauts_core::policy::{BudgetFacts, EvidenceFacts};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit, OnHit};

use crate::card::escalation::wait_decision;
use crate::card::machine::Trigger;
use crate::card::pipeline::transition;
use crate::card::recovery;
use crate::card::support::{newest_owner_session, owner_of, record_card, record_of};
use crate::checkpoints::CheckpointRequest;
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::state::RunState;
use crate::state::cards::CardRecord;

/// How much more budget "more budget" adds without an amount in your note.
const STEP: f64 = 0.5;

/// Applies `budget.on_hit` to a card waiting in `NeedsDecision`.
pub(crate) async fn on_hit(host: &Host, card: &CardId, on_hit: OnHit) -> EngineResult<()> {
    match on_hit {
        OnHit::Handoff => {
            // Out of NeedsDecision at once, as an answered budget card does, or the pipeline
            // decides the old owner's stop again and hands the card on a second time.
            if hand_off(host, card, Party::Rules)? {
                let reason = "handed off after a budget hit";
                transition(host, card, Trigger::Decided, reason)?;
            } else {
                let reason = "the card hit its budget and no other agent can take it";
                transition(host, card, Trigger::Escalate, reason)?;
            }
            Ok(())
        }
        OnHit::Escalate => {
            transition(host, card, Trigger::Escalate, "the card hit its budget")?;
            Ok(())
        }
        OnHit::Ask | OnHit::Extend => ask(host, card).await,
    }
}

/// `Budget(Hit)`: the card's work is committed, and `budget.on_hit` decides next.
pub(crate) fn record_hit(host: &Host, card: &CardId) -> EngineResult<()> {
    let record = record_of(host, card)?;
    let (dimension, status) = hit_of(host, &record);
    let hit = BudgetEvent::Hit {
        card: card.clone(),
        dimension,
        status,
    };
    host.record(Some(card), None, RunEventKind::Budget(hit))?;
    Ok(())
}

/// What the card ran out of, from its owner's last session, and where its meters stood then.
fn hit_of(host: &Host, record: &CardRecord) -> (BudgetDimension, BudgetStatus) {
    let card = &record.spec.id;
    let end = host.read(|state| newest_owner_session(state, card).and_then(|s| s.end.clone()));
    let dimension = end
        .as_ref()
        .and_then(|end| match end.reason {
            EndReason::BudgetHit { dimension } => Some(dimension),
            _ => None,
        })
        .unwrap_or(BudgetDimension::Spend);
    let budget = in_force(host, &record.spec);
    (dimension, status(&budget, end.as_ref(), dimension))
}

/// The budget the card's sessions run under: its own limits, and the settings' per-card ones
/// where it has none, as a card made without limits of its own goes by them.
fn in_force(host: &Host, spec: &CardSpec) -> CardBudget {
    let limits = host
        .services()
        .limits
        .session_limits(Some(spec), RoleName::Owner);
    let own = &spec.budget;
    CardBudget {
        limit: own.limit.or(limits.budget),
        max_minutes: own.max_minutes.or(limits.max_minutes),
        max_tool_calls: own.max_tool_calls.or(limits.max_tool_calls),
        inactivity_min: own.inactivity_min,
    }
}

/// The budget checkpoint. With `extend`, your approval rules may answer it from the reserve.
async fn ask(host: &Host, card: &CardId) -> EngineResult<()> {
    let record = record_of(host, card)?;
    let (dimension, status) = hit_of(host, &record);
    let reserve_left = host.read(|state| {
        state
            .run
            .budget
            .as_ref()
            .and_then(|budget| budget.reserve_left)
    });
    let packet = BudgetPacket {
        card: Some(card.clone()),
        status,
        dimension,
        reserve_left,
    };
    let request = CheckpointRequest {
        at: None,
        kind: CheckpointKind::Budget,
        title: format!("{card} hit its budget · {}", record.spec.title),
        card: Some(card.clone()),
        packet: CheckpointPacket::Budget(packet),
        facts: EvidenceFacts {
            budget: host.read(budget_facts),
            ..EvidenceFacts::default()
        },
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: None,
        free_text: FreeText::Answer,
        authors: vec![owner_of(host, &record)],
    };
    let Some(decided) = wait_decision(host, card, CardState::NeedsDecision, request).await? else {
        return Ok(());
    };
    match &decided.action {
        AnswerAction::MoreBudget | AnswerAction::Approve | AnswerAction::Retry => {
            let mut spec = record.spec.clone();
            let budget = in_force(host, &spec);
            spec.budget = extended(&budget, dimension, decided.text.as_deref());
            let more = added(&budget, &spec.budget, dimension);
            record_card(host, card, CardEvent::Created(Box::new(spec)))?;
            if let Some(amount) = more {
                let event = BudgetEvent::Extended {
                    card: card.clone(),
                    amount,
                    by: decided.by.clone(),
                };
                host.record(Some(card), None, RunEventKind::Budget(event))?;
            }
            transition(host, card, Trigger::Decided, "more budget")?;
        }
        AnswerAction::HandOff | AnswerAction::OtherCandidates | AnswerAction::Candidate { .. } => {
            let moved = match &decided.action {
                AnswerAction::Candidate { worker } => {
                    reassign(host, card, worker.clone(), decided.by.clone())?;
                    true
                }
                _ => hand_off(host, card, decided.by.clone())?,
            };
            if moved {
                transition(
                    host,
                    card,
                    Trigger::Decided,
                    "handed off after a budget hit",
                )?;
            } else {
                transition(host, card, Trigger::Escalate, "no other agent can take it")?;
            }
        }
        _ => {
            transition(
                host,
                card,
                Trigger::Escalate,
                "you escalated the budget hit",
            )?;
        }
    }
    Ok(())
}

fn hand_off(host: &Host, card: &CardId, by: Party) -> EngineResult<bool> {
    let record = record_of(host, card)?;
    match recovery::any_other(host, &record) {
        Some(to) => {
            reassign(host, card, to, by)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn reassign(
    host: &Host,
    card: &CardId,
    to: argonauts_core::names::WorkerRef,
    by: Party,
) -> EngineResult<()> {
    let record = record_of(host, card)?;
    let event = CardEvent::Reassigned {
        from: Some(owner_of(host, &record)),
        to,
        by,
        rule_override: None,
    };
    record_card(host, card, event)?;
    Ok(())
}

/// What the cards not yet done would cost, against what's left of the run's reserve. `None`,
/// which asks you, when the reserve or a card's budget isn't known in the reserve's unit.
pub(crate) fn budget_facts(state: &RunState) -> Option<BudgetFacts> {
    let reserve_left = state.run.budget.as_ref()?.reserve_left?;
    let mut remaining = 0.0;
    for record in state.cards.by_id.values() {
        if matches!(record.state, CardState::Integrated | CardState::Cancelled) {
            continue;
        }
        let limit = record.spec.budget.limit?;
        if limit.unit != reserve_left.unit {
            return None;
        }
        remaining += limit.amount;
    }
    Some(BudgetFacts {
        remaining_cards: BudgetAmount {
            unit: reserve_left.unit,
            amount: remaining,
        },
        reserve_left,
    })
}

/// Whether one more attempt at the card, at its own budget, fits in what's left of the
/// reserve. Always with budgets off; `None` when either isn't known in the same unit.
pub(crate) fn fits_reserve(state: &RunState, record: &CardRecord) -> Option<bool> {
    let budget = state.run.budget.as_ref()?;
    if budget.limit.is_none() {
        return Some(true);
    }
    let reserve = budget.reserve_left?;
    let cost = record.spec.budget.limit?;
    (cost.unit == reserve.unit).then_some(cost.amount <= reserve.amount)
}

/// The card's budget with more room on the dimension it hit: the amount in your note, or half
/// again what it had.
pub(crate) fn extended(
    budget: &CardBudget,
    dimension: BudgetDimension,
    note: Option<&str>,
) -> CardBudget {
    let asked = note.and_then(|text| {
        text.trim()
            .trim_start_matches('$')
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<f64>().ok())
    });
    let grow = |current: f64| asked.unwrap_or(current * (1.0 + STEP));
    let mut budget = budget.clone();
    match dimension {
        BudgetDimension::Minutes => {
            budget.max_minutes = budget.max_minutes.map(grow);
        }
        BudgetDimension::ToolCalls => {
            budget.max_tool_calls = budget
                .max_tool_calls
                .map(|calls| grow(calls as f64).round() as u64);
        }
        BudgetDimension::Spend => {
            budget.limit = budget.limit.as_ref().map(|limit| BudgetAmount {
                unit: limit.unit,
                amount: grow(limit.amount),
            });
        }
    }
    budget
}

/// How much more `after` allows than `before` in `dimension`. A tool-call count has no budget
/// unit, so that extension shows only in the card's new budget.
fn added(
    before: &CardBudget,
    after: &CardBudget,
    dimension: BudgetDimension,
) -> Option<BudgetAmount> {
    let (unit, from, to) = match dimension {
        BudgetDimension::Spend => {
            let (before, after) = (before.limit?, after.limit?);
            (after.unit, before.amount, after.amount)
        }
        BudgetDimension::Minutes => (BudgetUnit::Minutes, before.max_minutes?, after.max_minutes?),
        BudgetDimension::ToolCalls => return None,
    };
    (to > from).then_some(BudgetAmount {
        unit,
        amount: to - from,
    })
}

fn status(
    budget: &CardBudget,
    end: Option<&SessionEnd>,
    dimension: BudgetDimension,
) -> BudgetStatus {
    BudgetStatus {
        level: BudgetAction::Stop,
        limit: budget
            .limit
            .as_ref()
            .map(|limit| match limit.unit {
                BudgetUnit::Usd => BudgetLimit::Usd(limit.amount),
                BudgetUnit::Tokens => BudgetLimit::Tokens(limit.amount.max(0.0).round() as u64),
                BudgetUnit::Quota | BudgetUnit::Minutes => BudgetLimit::None,
            })
            .unwrap_or_default(),
        spend: end.map(|end| end.spend).unwrap_or_default(),
        fraction_used: None,
        work: WorkUsed {
            minutes: end.map_or(0.0, |end| end.duration_ms as f64 / 60_000.0),
            tool_calls: end.map_or(0, |end| end.tool_calls),
        },
        work_limits: WorkLimits {
            max_minutes: budget.max_minutes,
            max_tool_calls: budget.max_tool_calls,
        },
        binding: Some(dimension),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::vendor::RunBudget;

    use super::*;
    use crate::testutil::card_spec;

    fn usd(amount: f64) -> BudgetAmount {
        BudgetAmount {
            unit: BudgetUnit::Usd,
            amount,
        }
    }

    /// A run with a $10 limit and `reserve` left, and one card per entry of `cards`: its state
    /// and its budget.
    fn run(reserve: Option<BudgetAmount>, cards: &[(CardState, Option<BudgetAmount>)]) -> RunState {
        let mut state = RunState::default();
        state.run.budget = Some(RunBudget {
            spent: Default::default(),
            limit: Some(usd(10.0)),
            lines: Vec::new(),
            reserve_left: reserve,
        });
        for (n, (card_state, limit)) in cards.iter().enumerate() {
            let mut spec = card_spec(&format!("card-{:03}", n + 1), &[], None);
            spec.budget.limit = *limit;
            let mut record = CardRecord::new(spec);
            record.state = *card_state;
            state.cards.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    #[test]
    fn budget_facts_sum_the_cards_not_yet_done() {
        let state = run(
            Some(usd(3.0)),
            &[
                (CardState::Integrated, Some(usd(5.0))),
                (CardState::Cancelled, None),
                (CardState::NeedsDecision, Some(usd(2.0))),
                (CardState::Waiting, Some(usd(1.5))),
            ],
        );
        let facts = budget_facts(&state).unwrap();
        assert_eq!(facts.remaining_cards, usd(3.5));
        assert_eq!(facts.reserve_left, usd(3.0));
    }

    #[test]
    fn budget_facts_are_missing_when_anything_is_unknown() {
        let no_reserve = run(None, &[(CardState::Ready, Some(usd(1.0)))]);
        assert_eq!(budget_facts(&no_reserve), None);
        let unpriced = run(Some(usd(3.0)), &[(CardState::Ready, None)]);
        assert_eq!(budget_facts(&unpriced), None);
        let tokens = BudgetAmount {
            unit: BudgetUnit::Tokens,
            amount: 1_000.0,
        };
        let other_unit = run(Some(usd(3.0)), &[(CardState::Ready, Some(tokens))]);
        assert_eq!(budget_facts(&other_unit), None);
        assert_eq!(budget_facts(&RunState::default()), None);
    }

    #[test]
    fn one_more_attempt_fits_the_reserve_at_the_cards_budget() {
        let card = |state: &RunState| state.cards.by_id.values().next().unwrap().clone();
        let fits = run(Some(usd(3.0)), &[(CardState::Escalated, Some(usd(2.0)))]);
        assert_eq!(fits_reserve(&fits, &card(&fits)), Some(true));
        let too_much = run(Some(usd(1.0)), &[(CardState::Escalated, Some(usd(2.0)))]);
        assert_eq!(fits_reserve(&too_much, &card(&too_much)), Some(false));
        let unknown = run(None, &[(CardState::Escalated, Some(usd(2.0)))]);
        assert_eq!(fits_reserve(&unknown, &card(&unknown)), None);
        let mut off = run(None, &[(CardState::Escalated, None)]);
        off.run.budget.as_mut().unwrap().limit = None;
        assert_eq!(fits_reserve(&off, &card(&off)), Some(true));
    }

    #[test]
    fn more_budget_takes_your_amount_or_half_again() {
        let budget = CardBudget {
            limit: Some(BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: 2.0,
            }),
            max_minutes: Some(30.0),
            max_tool_calls: Some(100),
            inactivity_min: None,
        };
        let more = extended(&budget, BudgetDimension::Spend, None);
        assert_eq!(more.limit.unwrap().amount, 3.0);
        assert_eq!(
            more.max_minutes,
            Some(30.0),
            "only the dimension that hit grows"
        );
        let asked = extended(&budget, BudgetDimension::Spend, Some("$5 please"));
        assert_eq!(asked.limit.unwrap().amount, 5.0);
        assert_eq!(
            extended(&budget, BudgetDimension::Minutes, None).max_minutes,
            Some(45.0)
        );
        assert_eq!(
            extended(&budget, BudgetDimension::ToolCalls, Some("250")).max_tool_calls,
            Some(250)
        );
    }

    #[test]
    fn the_extension_is_what_the_hit_dimension_gained() {
        let budget = CardBudget {
            limit: Some(usd(2.0)),
            max_minutes: Some(30.0),
            max_tool_calls: Some(100),
            inactivity_min: None,
        };
        let more = |dimension, note| {
            let after = extended(&budget, dimension, note);
            added(&budget, &after, dimension)
        };
        assert_eq!(more(BudgetDimension::Spend, None), Some(usd(1.0)));
        assert_eq!(
            more(BudgetDimension::Minutes, None),
            Some(BudgetAmount {
                unit: BudgetUnit::Minutes,
                amount: 15.0,
            })
        );
        assert_eq!(more(BudgetDimension::ToolCalls, Some("250")), None);
        // Less than it had is no extension.
        assert_eq!(more(BudgetDimension::Spend, Some("$1")), None);
    }
}
