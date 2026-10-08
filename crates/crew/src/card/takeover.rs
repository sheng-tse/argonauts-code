//! Reassign, take over, and stopping one session.

use std::path::PathBuf;
use std::time::Duration;

use argonauts_core::api::{CheckpointAnswer, TakeOverTicket};
use argonauts_core::events::card::{CardEvent, CardState};
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind, CheckpointOpened};
use argonauts_core::events::run::Party;
use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
use argonauts_core::worker::SessionControl;
use argonauts_core::{CardId, DiffStat};

use crate::card::machine::Trigger;
use crate::card::pipeline::transition;
use crate::card::support::{
    newest_owner_session, owner_of, progress_of, record_card, record_of, state_of, wait_until,
};
use crate::card::worktree;
use crate::checkpoints;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::roles;
use crate::state::cards::CardRecord;

/// How long `/reassign` waits for the owner's report after the wrap-up before it stops the
/// session: briefly, since you asked for the card to move now.
const WRAP_UP_WAIT: Duration = Duration::from_secs(30);
/// How long a take-over waits for the session to stop after its tool call.
const STOP_WAIT: Duration = Duration::from_secs(30);

/// Stops one session; its card goes to `NeedsDecision` with an escalation-style checkpoint.
pub async fn stop_session(host: &Host, session: &SessionKey) -> EngineResult<()> {
    let card = host.read(|state| state.sessions.get(session).and_then(|s| s.card.clone()));
    // The card moves first, so its pipeline sees your stop when the session ends.
    if let Some(card) = &card
        && state_of(host, card)? == CardState::Running
    {
        transition(
            host,
            card,
            Trigger::NeedsDecision,
            "you stopped its session",
        )?;
    }
    host.control(session, SessionControl::Stop)
        .map_err(EngineError::Refused)
}

/// Sends the wrap-up, waits briefly for the report, stops, commits, builds the packet, and
/// starts the new owner. Only candidates the role allows, unless you override one rule for this
/// card (already checked against the token, and logged).
///
/// The owner changes at once; the card's pipeline commits the stopped session's work and
/// starts the new owner with the packet.
pub async fn reassign(
    host: &Host,
    card: &CardId,
    to: WorkerRef,
    rule_override: Option<String>,
) -> Result<(), String> {
    reassign_card(host, card, to, rule_override)
        .await
        .map_err(|err| err.to_string())
}

async fn reassign_card(
    host: &Host,
    card: &CardId,
    to: WorkerRef,
    rule_override: Option<String>,
) -> EngineResult<()> {
    let record = record_of(host, card)?;
    if matches!(record.state, CardState::Integrated | CardState::Cancelled) {
        return Err(EngineError::Refused(format!(
            "{card} is {:?}, so it can't move",
            record.state
        )));
    }
    let from = owner_of(host, &record);
    if from == to {
        return Err(EngineError::Refused(format!("{to} already owns {card}")));
    }
    if let Some(rule) = broken_rule(host, &record, &from, &to)
        && rule_override.is_none()
    {
        return Err(EngineError::Refused(rule));
    }
    let event = CardEvent::Reassigned {
        from: Some(from),
        to: to.clone(),
        by: Party::User,
        rule_override,
    };
    record_card(host, card, event)?;
    let reason = format!("you reassigned it to {to}");
    match record.state {
        CardState::Running => wrap_up_and_stop(host, card).await?,
        CardState::Failed => {
            transition(host, card, Trigger::Retry, &reason)?;
        }
        CardState::Escalated => {
            transition(host, card, Trigger::NewOwner, &reason)?;
        }
        CardState::NeedsDecision => {
            transition(host, card, Trigger::Decided, &reason)?;
        }
        _ => {}
    }
    Ok(())
}

/// Why `to` may not own the card: the role doesn't list it, it shares a family with one of the
/// card's reviewers, or another role rule.
fn broken_rule(
    host: &Host,
    record: &CardRecord,
    from: &WorkerRef,
    to: &WorkerRef,
) -> Option<String> {
    let allowed = roles::candidates(host.settings(), RoleName::Worker);
    if !allowed.is_empty() && !allowed.contains(to) {
        return Some(format!("roles.worker doesn't list {to}"));
    }
    let family = roles::family(host, to);
    if let Some(reviewer) = record
        .spec
        .reviewers
        .iter()
        .find(|reviewer| roles::family(host, reviewer) == family)
    {
        return Some(format!(
            "{to} is the same provider as {reviewer}, who reviews {}",
            record.spec.id
        ));
    }
    roles::conflict(RoleName::Worker, to, Some(from), &|w| {
        roles::family(host, w)
    })
}

async fn wrap_up_and_stop(host: &Host, card: &CardId) -> EngineResult<()> {
    let Some(session) = live_session(host, card) else {
        return Ok(());
    };
    if host.control(&session, SessionControl::WrapUp).is_ok()
        && ended_within(host, &session, WRAP_UP_WAIT).await?
    {
        return Ok(());
    }
    let _ = host.control(&session, SessionControl::Stop);
    Ok(())
}

/// Pauses the card (`StopAfterTool`), commits, and returns the vendor's take-over command in
/// the card's worktree.
pub async fn take_over(host: &Host, card: &CardId) -> Result<TakeOverTicket, String> {
    take_over_card(host, card)
        .await
        .map_err(|err| err.to_string())
}

async fn take_over_card(host: &Host, card: &CardId) -> EngineResult<TakeOverTicket> {
    let record = record_of(host, card)?;
    // The handoff card this take-over answers takes the card over too, and finds it yours.
    if held_by_you(host, card)? {
        let tree = worktree::ensure(host, card).await?;
        return Ok(ticket(host, card, owner_of(host, &record), tree.path));
    }
    let open = matches!(
        record.state,
        CardState::Running | CardState::NeedsDecision | CardState::Escalated
    );
    if !open {
        return Err(EngineError::Refused(format!(
            "{card} is {:?}; only a running, stopped, or needs-you task can be taken over",
            record.state
        )));
    }
    let owner = owner_of(host, &record);
    let tree = worktree::ensure(host, card).await?;
    let live = live_session(host, card);
    transition(host, card, Trigger::TakeOver, "you took it over")?;
    if let Some(session) = &live {
        let stopped = host.control(session, SessionControl::StopAfterTool).is_ok()
            && ended_within(host, session, STOP_WAIT).await?;
        if !stopped {
            let _ = host.control(session, SessionControl::Stop);
            ended_within(host, session, STOP_WAIT).await?;
        }
    }
    worktree::commit_in(host, card, &tree, live.as_ref(), Some(owner.clone())).await?;
    let ticket = ticket(host, card, owner, tree.path);
    let argv = ticket.command.as_ref().map(|spec| {
        std::iter::once(spec.program.clone())
            .chain(spec.args.iter().cloned())
            .collect()
    });
    record_card(host, card, CardEvent::TakenOver { command: argv })?;
    answer_handoff_card(host, card);
    Ok(ticket)
}

/// A take-over is the "Take over myself" a stopped card's handoff card offers, so that card
/// closes with it. Its handler then finds the card already yours.
fn answer_handoff_card(host: &Host, card: &CardId) {
    let open: Vec<CheckpointOpened> = host.read(|state| {
        state
            .checkpoints
            .by_id
            .values()
            .filter(|record| {
                record.is_open()
                    && record.opened.kind == CheckpointKind::Handoff
                    && record.opened.card.as_ref() == Some(card)
            })
            .map(|record| record.opened.clone())
            .collect()
    });
    for opened in open {
        let answer = CheckpointAnswer {
            id: opened.id,
            action: AnswerAction::TakeOver,
            text: None,
            evidence_sha256: opened.evidence_sha256,
            cards: Vec::new(),
        };
        if let Err(why) = checkpoints::answer(host, answer) {
            tracing::warn!(
                "the fallback question of {card} stayed open after the take-over: {why}"
            );
        }
    }
}

/// Taken over and not handed back yet.
fn held_by_you(host: &Host, card: &CardId) -> EngineResult<bool> {
    Ok(state_of(host, card)? == CardState::TakenOver
        && progress_of(host, card).take_over_end.is_none())
}

/// The owner's vendor UI on its newest session, in the card's worktree.
fn ticket(host: &Host, card: &CardId, owner: WorkerRef, worktree: PathBuf) -> TakeOverTicket {
    let vendor_session =
        host.read(|state| newest_owner_session(state, card).and_then(|s| s.vendor_session.clone()));
    let command = match (vendor_session, host.services().workers.get(&owner)) {
        (Some(id), Ok(worker)) => worker.takeover_command(&id, &worktree),
        _ => None,
    };
    TakeOverTicket {
        card: card.clone(),
        command,
        worktree,
        vendor: owner.worker,
    }
}

/// Commits your changes and opens the take-over-return checkpoint: let the owner continue
/// (told what changed) or keep it yours (gates and review run when you say done).
///
/// This records the end of your take-over; the card's pipeline opens the checkpoint, so the
/// command never waits on your answer.
pub async fn take_over_ended(host: &Host, card: &CardId) -> EngineResult<()> {
    let record = record_of(host, card)?;
    if record.state != CardState::TakenOver || progress_of(host, card).take_over_end.is_some() {
        return Ok(());
    }
    let tree = worktree::ensure(host, card).await?;
    let commit = worktree::commit_in(host, card, &tree, None, None).await?;
    let (changed, sha) = match commit {
        Some(info) => (info.stat, info.sha),
        None => (DiffStat::default(), None),
    };
    let ended = CardEvent::TakeOverEnded {
        changed,
        commit: sha,
    };
    record_card(host, card, ended)?;
    Ok(())
}

/// The card's owner session that is still running, if any.
fn live_session(host: &Host, card: &CardId) -> Option<SessionKey> {
    host.read(|state| {
        newest_owner_session(state, card)
            .filter(|session| session.end.is_none())
            .map(|session| session.key.clone())
    })
}

/// True when the session's end is recorded within `limit`.
async fn ended_within(host: &Host, session: &SessionKey, limit: Duration) -> EngineResult<bool> {
    let ended = |state: &crate::state::RunState| {
        state
            .sessions
            .get(session)
            .is_none_or(|record| record.end.is_some())
    };
    match tokio::time::timeout(limit, wait_until(host, ended)).await {
        Ok(result) => result.map(|()| true),
        Err(_) => Ok(false),
    }
}
