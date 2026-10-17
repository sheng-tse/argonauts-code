//! Fallbacks with checkpoints: the committed work is tagged
//! `argonauts/<run>/<task>/fallback-<n>`, each commit's author is kept in a git note, a weaker
//! fallback leaves quality debt, `on_return` decides what happens when the original is back,
//! races run several candidates at once, and `/rewind` resets to a tag keeping the replaced work.

use argonauts_core::TaskId;
use argonauts_core::events::run::Party;
use argonauts_core::limits::RecoveryAction;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::task::machine::Trigger;
use crate::task::pipeline;

pub mod ask;
pub mod assign;
pub mod broken;
pub mod builders_provider;
pub mod choose;
pub mod context;
pub mod debt;
pub mod defaults;
pub mod down;
pub mod judge;
pub mod merge_return;
pub mod merge_rule;
pub mod notes;
pub mod on_return;
pub mod race;
pub mod rewind;
pub mod rules;
pub mod summary;
pub mod tags;
pub mod upgrade;

/// Applies `Fallback`, `Race`, `Ask`, or `Wait` for a task whose agent stopped.
pub async fn apply(host: &Host, task: &TaskId, action: RecoveryAction) -> EngineResult<()> {
    match action {
        RecoveryAction::Fallback {
            to,
            note,
            checkpoint,
        } => {
            let record = rules::record(host, task)?;
            let stopped = rules::worker(&record);
            let to = choose::choose(host, &record, stopped.as_ref(), to);
            assign::fallback(host, task, &to, &note, checkpoint.is_some(), Party::Rules)
                .await
                .map(drop)
        }
        RecoveryAction::Race {
            to,
            pick,
            note,
            checkpoint,
        } => {
            let record = rules::record(host, task)?;
            let stopped = rules::worker(&record);
            let tag = assign::checkpoint(
                host,
                &record,
                stopped.as_ref(),
                &to,
                &note,
                checkpoint.is_some(),
            )
            .await?;
            match race::run(host, task, &to, pick, &note).await? {
                Some(winner) => {
                    let since = tag.map_or(record.fallbacks, |tag| tag.n);
                    let record = rules::record(host, task)?;
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
                    task,
                    Trigger::AskYou,
                    "no fallback's try passed its checks and review",
                )
                .map(drop),
            }
        }
        RecoveryAction::Ask(plan) => ask::ask(host, task, *plan).await,
        RecoveryAction::Wait {
            agent_id,
            until,
            kept_off,
        } => {
            let kept_on_its_cli = kept_off.is_some();
            ask::wait(host, task, agent_id.clone(), until, kept_off)?;
            // Only a task that would have gone elsewhere has something to ask you.
            if kept_on_its_cli {
                down::ask_in_background(host, &agent_id)?;
            }
            Ok(())
        }
        other => Err(EngineError::Refused(format!(
            "a fallback can't apply {}",
            action_name(&other)
        ))),
    }
}

fn action_name(action: &RecoveryAction) -> &'static str {
    match action {
        RecoveryAction::Proceed => "proceed",
        RecoveryAction::ResumeSame { .. } => "resume the same session",
        RecoveryAction::Rotate { .. } => "rotate",
        RecoveryAction::Fallback { .. } => "fall back",
        RecoveryAction::Race { .. } => "race",
        RecoveryAction::Ask(_) => "ask",
        RecoveryAction::Wait { .. } => "wait",
        RecoveryAction::LimitHit(_) => "a limit hit",
        RecoveryAction::AskYou { .. } => "ask_you",
        RecoveryAction::RunWaits(_) => "wait",
    }
}
