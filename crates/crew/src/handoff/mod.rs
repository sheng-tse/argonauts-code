//! Handoffs with checkpoints: the committed work is tagged
//! `argonauts/<run>/<card>/handoff-<n>`, each commit's author is kept in a git note, a weaker
//! replacement leaves quality debt, `on_return` decides what happens when the original is back,
//! races run several candidates at once, and `/rewind` resets to a tag keeping the replaced work.

use argonauts_core::CardId;
use argonauts_core::events::run::Party;
use argonauts_core::limits::RecoveryAction;

use crate::card::machine::Trigger;
use crate::card::pipeline;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};

pub mod ask;
pub mod assign;
pub mod broken;
pub mod choose;
pub mod context;
pub mod debt;
pub mod defaults;
pub mod merge_return;
pub mod merge_rule;
pub mod notes;
pub mod on_return;
pub mod race;
pub mod revert;
pub mod rules;
pub mod summary;
pub mod tags;
pub mod upgrade;

/// Applies `HandOff`, `Race`, `Ask`, or `Wait` for a card whose agent stopped.
pub async fn apply(host: &Host, card: &CardId, action: RecoveryAction) -> EngineResult<()> {
    match action {
        RecoveryAction::HandOff {
            to,
            packet,
            checkpoint,
        } => {
            let record = rules::record(host, card)?;
            let stopped = rules::owner(&record);
            let to = choose::choose(host, &record, stopped.as_ref(), to);
            assign::hand_off(host, card, &to, &packet, checkpoint.is_some(), Party::Rules)
                .await
                .map(drop)
        }
        RecoveryAction::Race {
            to,
            packet,
            checkpoint,
        } => {
            let record = rules::record(host, card)?;
            let stopped = rules::owner(&record);
            let tag = assign::checkpoint(
                host,
                &record,
                stopped.as_ref(),
                &to,
                &packet,
                checkpoint.is_some(),
            )
            .await?;
            match race::run(host, card, &to, &packet).await? {
                Some(winner) => {
                    let since = tag.map_or(record.handoffs, |tag| tag.n);
                    let record = rules::record(host, card)?;
                    assign::settle(
                        host,
                        &record,
                        stopped.as_ref(),
                        &winner,
                        Party::Rules,
                        since,
                    )
                }
                None => pipeline::transition(
                    host,
                    card,
                    Trigger::Escalate,
                    "no race attempt passed its gates and review",
                )
                .map(drop),
            }
        }
        RecoveryAction::Ask(plan) => ask::ask(host, card, *plan).await,
        RecoveryAction::Wait { vendor, until } => ask::wait(host, card, vendor, until),
        other => Err(EngineError::Refused(format!(
            "a handoff can't apply {}",
            action_name(&other)
        ))),
    }
}

fn action_name(action: &RecoveryAction) -> &'static str {
    match action {
        RecoveryAction::Proceed => "proceed",
        RecoveryAction::ResumeSame { .. } => "resume the same session",
        RecoveryAction::Rotate { .. } => "rotate",
        RecoveryAction::HandOff { .. } => "hand off",
        RecoveryAction::Race { .. } => "race",
        RecoveryAction::Ask(_) => "ask",
        RecoveryAction::Wait { .. } => "wait",
        RecoveryAction::BudgetHit(_) => "a budget hit",
        RecoveryAction::Escalate { .. } => "escalate",
        RecoveryAction::Park(_) => "park",
    }
}
