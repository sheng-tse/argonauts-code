//! What you do to one card: reassigning it, remembering a handoff pick as a rule, and its
//! handoff checkpoints (`/rewind`).

use argonauts_core::api::ApiError;
use argonauts_core::app::{AppEvent, ChoiceKind, ViewData};
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointPacket};
use argonauts_core::events::{CheckpointEvent, RunEvent, RunEventKind, SessionEvent};
use argonauts_core::names::{CheckpointId, RoleName, VendorId, WorkerRef};
use argonauts_core::settings::handoff::CrewRole;
use argonauts_core::{CardId, NoteLevel, RunId};

use super::views::run_events;
use super::{Handled, NO_APPROVALS, on_screen};
use crate::choices;
use crate::controller::runs::{RunChoice, RunsState};
use crate::controller::{Ctx, Effect, ScopeRequest};
use crate::views;

/// From a picker: within the role's rules, else a confirm and `Approvals::reassign_overriding`.
pub(crate) async fn reassign(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    card: CardId,
    to: WorkerRef,
) -> Handled {
    match ctx
        .services
        .conductor
        .reassign(&run, &card, to.clone())
        .await
    {
        Ok(()) => {
            let name = ctx.names.worker(&to);
            ctx.notice(
                NoteLevel::Info,
                format!("Moving {card} to {name}, with its work saved and a fallback packet."),
            );
            Ok(Vec::new())
        }
        // The worker breaks one of the role's rules, which only you can override.
        Err(ApiError::Refused(rule)) => {
            confirm_override(ctx, runs, run, card, to, rule);
            Ok(Vec::new())
        }
        Err(err) => super::refused(ctx, err, || format!("Couldn't reassign {card}")),
    }
}

fn confirm_override(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    card: CardId,
    to: WorkerRef,
    rule: String,
) {
    let name = ctx.names.worker(&to);
    if ctx.services.approvals.is_none() {
        ctx.notice(
            NoteLevel::Warn,
            format!("{name} can't take {card}: {rule}. {NO_APPROVALS}"),
        );
        return;
    }
    let id = ctx.next_choice_id();
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::ConfirmSafetySetting,
        format!("Give {card} to {name} anyway?"),
        vec![
            format!("{name} breaks a rule of the task's role: {rule}."),
            "Overriding it applies to this task only, and the run's log records it.".to_owned(),
        ],
        vec![
            choices::option('y', "Override the rule", AnswerAction::Approve, false),
            choices::option(
                'n',
                "Keep the task where it is",
                AnswerAction::Cancel,
                false,
            ),
        ],
    );
    runs.choices.insert(
        id,
        RunChoice::OverrideRule {
            run,
            card,
            to,
            rule,
        },
    );
    ctx.send(AppEvent::Choice { choice });
}

/// `/reassign <card> <worker>` or `/reassign --from <vendor> <worker>`.
pub(crate) async fn slash_reassign(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    card: Option<CardId>,
    from: Option<VendorId>,
    to: WorkerRef,
) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    let cards = match (card, from) {
        (Some(card), _) => vec![card],
        (None, Some(vendor)) => {
            let summary = match ctx.services.conductor.status(&run).await {
                Ok(summary) => summary,
                Err(err) => {
                    return super::refused(ctx, err, || "Couldn't read the run's tasks".to_owned());
                }
            };
            let cards: Vec<CardId> = summary
                .cards
                .into_iter()
                .filter(|brief| {
                    let with_vendor = brief
                        .owner
                        .as_ref()
                        .is_some_and(|owner| owner.vendor() == vendor);
                    with_vendor
                        && !matches!(brief.state, CardState::Integrated | CardState::Cancelled)
                })
                .map(|brief| brief.card)
                .collect();
            if cards.is_empty() {
                let name = ctx.names.harness(&vendor.0);
                ctx.notice(
                    NoteLevel::Info,
                    format!("No open task of {run} is with {name}."),
                );
            }
            cards
        }
        (None, None) => {
            ctx.notice(
                NoteLevel::Info,
                "Name a task or an agent: /reassign <task> <worker>, or /reassign --from <agent> \
                 <worker>.",
            );
            return Ok(Vec::new());
        }
    };
    let mut effects = Vec::new();
    for card in cards {
        effects.extend(reassign(ctx, runs, run.clone(), card, to.clone()).await?);
    }
    Ok(effects)
}

/// Saves an answered handoff card's pick as a replacement rule, after the scope question.
pub(crate) async fn remember_pick(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    id: CheckpointId,
) -> Handled {
    remember(ctx, runs, &run, &id, None)
}

/// `pick` is the answer just sent; without it, the pick the run's log recorded.
pub(super) fn remember(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: &RunId,
    id: &CheckpointId,
    pick: Option<WorkerRef>,
) -> Handled {
    let events = run_events(ctx, runs, run);
    let Some(rule) = handoff_rule(&events, id, pick) else {
        ctx.notice(
            NoteLevel::Info,
            format!("{id} has no fallback pick to remember."),
        );
        return Ok(Vec::new());
    };
    Ok(vec![Effect::AskScope(Box::new(rule))])
}

/// `roles.<role>.handoff.replace.<vendor>.with = [pick]` for the stopped worker's role and
/// vendor. Only the `with` list changes, so the table's strategy and `on_return` stay.
pub(super) fn handoff_rule(
    events: &[RunEvent],
    id: &CheckpointId,
    pick: Option<WorkerRef>,
) -> Option<ScopeRequest> {
    let plan = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if &opened.id == id => {
            match &opened.packet {
                CheckpointPacket::Handoff(plan) => Some(plan),
                _ => None,
            }
        }
        _ => None,
    })?;
    let pick = pick.or_else(|| recorded_pick(events, id))?;
    let role = stopped_role(events, &plan.card, &plan.stopped);
    let key = format!(
        "fallback.{}.replace.{}.with",
        CrewRole::of(role).as_str(),
        plan.stopped.vendor()
    );
    let value = toml::Value::Array(vec![toml::Value::String(pick.to_string())]).to_string();
    Some(ScopeRequest {
        key,
        value,
        command: "fallback".to_owned(),
    })
}

fn recorded_pick(events: &[RunEvent], id: &CheckpointId) -> Option<WorkerRef> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) if &decided.id == id => {
            match &decided.action {
                AnswerAction::Candidate { worker } => Some(worker.clone()),
                _ => None,
            }
        }
        _ => None,
    })
}

/// The role the stopped worker had on the card; an owner when the log doesn't say.
fn stopped_role(events: &[RunEvent], card: &CardId, stopped: &WorkerRef) -> RoleName {
    let sessions = events.iter().rev().filter_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started))
            if event.card_id.as_ref() == Some(card) =>
        {
            Some(started)
        }
        _ => None,
    });
    let mut any = None;
    for started in sessions {
        if &started.worker == stopped {
            return started.role;
        }
        any.get_or_insert(started.role);
    }
    any.unwrap_or(RoleName::Worker)
}

/// `/rewind [card]`: a picker of the card's handoff checkpoints, or every card's. Picking one
/// sends `UiCommand::Rewind`, which asks before it reverts.
pub(crate) async fn checkpoints(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    card: Option<CardId>,
) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    let events = run_events(ctx, runs, &run);
    let checkpoints = views::checkpoint_rows(&events, card.as_ref());
    if checkpoints.is_empty() {
        let text = match &card {
            Some(card) => format!("{card} has no fallback checkpoints to go back to."),
            None => format!("No task of {run} has a fallback checkpoint to go back to."),
        };
        ctx.notice(NoteLevel::Info, text);
        return Ok(Vec::new());
    }
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Checkpoints {
            run,
            card,
            checkpoints,
        }),
    });
    Ok(Vec::new())
}

/// The checkpoint picked in `/rewind`: a `ConfirmRevert` card first.
pub(crate) async fn revert(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    card: CardId,
    checkpoint: u32,
) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    let id = ctx.next_choice_id();
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::ConfirmRevert,
        format!("Revert {card} to fallback-{checkpoint}?"),
        vec![
            format!("Its branch goes back to argonauts/{run}/{card}/fallback-{checkpoint}."),
            format!(
                "The work after it stays on argonauts/{run}/{card}-replaced-{checkpoint}, so \
                 nothing is lost."
            ),
        ],
        choices::options(ChoiceKind::ConfirmRevert),
    );
    runs.choices.insert(
        id,
        RunChoice::Revert {
            run,
            card,
            checkpoint,
        },
    );
    ctx.send(AppEvent::Choice { choice });
    Ok(Vec::new())
}
