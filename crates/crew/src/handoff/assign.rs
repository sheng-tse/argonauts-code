//! Giving a card to a new owner: the checkpoint tag, the packet the new owner starts from, the
//! reassignment, and quality debt when the new owner is weaker than the strongest one before it.

use std::path::PathBuf;
use std::slice;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardEvent;
use argonauts_core::events::run::Party;
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::handoff::{CheckpointTag, QualityDebt};
use argonauts_core::limits::HandoffPacket;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::handoff::{HandoffMode, UnknownTier};
use argonauts_core::{CardId, sha256_hex};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::handoff::{broken, debt, notes, rules, tags};
use crate::state::cards::CardRecord;

/// Hands the card to `to`: tags its committed work first when `tag` is set, then the packet,
/// the reassignment, and any debt. The new owner continues at the card's next attempt.
pub async fn hand_off(
    host: &Host,
    card: &CardId,
    to: &WorkerRef,
    packet: &HandoffPacket,
    tag: bool,
    by: Party,
) -> EngineResult<Option<CheckpointTag>> {
    let record = rules::record(host, card)?;
    let from = rules::owner(&record);
    let checkpoint = checkpoint(
        host,
        &record,
        from.as_ref(),
        slice::from_ref(to),
        packet,
        tag,
    )
    .await?;
    let mut packet = packet.clone();
    if let Some(kept_on) = broken::set_aside(host, &record).await? {
        packet.notes.push(format!(
            "Its work failed the gates, so it was kept on {kept_on} and the card starts again \
             from its base."
        ));
    }
    write_packet(host, card, &packet).await?;
    let since = checkpoint.as_ref().map_or(record.handoffs, |tag| tag.n);
    settle(host, &record, from.as_ref(), to, by, since)?;
    Ok(checkpoint)
}

/// Tags the card's work before anyone else touches it, when checkpoints are on and there is
/// work on a branch to tag.
pub async fn checkpoint(
    host: &Host,
    record: &CardRecord,
    from: Option<&WorkerRef>,
    to: &[WorkerRef],
    packet: &HandoffPacket,
    tag: bool,
) -> EngineResult<Option<CheckpointTag>> {
    notes::catch_up(host, &record.spec.id).await?;
    let (Some(from), true, Some(_)) = (from, tag, record.branch.as_ref()) else {
        return Ok(None);
    };
    let reason = if packet.why_stopped.is_empty() {
        "handed off"
    } else {
        packet.why_stopped.as_str()
    };
    tags::tag(host, &record.spec.id, from, to, reason)
        .await
        .map(Some)
}

/// Saves the packet beside the run's log and records it, so the next attempt starts from it.
pub async fn write_packet(host: &Host, card: &CardId, packet: &HandoffPacket) -> EngineResult<()> {
    let record = rules::record(host, card)?;
    let text = serde_json::to_string_pretty(packet)?;
    let path = packet_path(host, card, record.reassigned + 1);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(&path, &text).await?;
    host.record(
        Some(card),
        None,
        RunEventKind::Handoff(HandoffEvent::Packet {
            card: card.clone(),
            path,
            sha256: sha256_hex(text.as_bytes()),
        }),
    )?;
    Ok(())
}

fn packet_path(host: &Host, card: &CardId, n: u32) -> PathBuf {
    host.repo()
        .run_dir
        .join("handoffs")
        .join(format!("{card}-{n}.json"))
}

/// Records the new owner and any debt, then puts the card back in line.
pub fn settle(
    host: &Host,
    record: &CardRecord,
    from: Option<&WorkerRef>,
    to: &WorkerRef,
    by: Party,
    since: u32,
) -> EngineResult<()> {
    let card = &record.spec.id;
    let rules = rules::applied_for(host, &record.spec, from.unwrap_or(to));
    // Only your pick on the card was asked; rules, a timed-out ask, or the approver hand off
    // as auto does.
    let mode = match by {
        Party::User => rules.mode,
        _ => HandoffMode::Auto,
    };
    host.record(
        Some(card),
        None,
        RunEventKind::Card(CardEvent::Reassigned {
            from: from.cloned(),
            to: to.clone(),
            by: by.clone(),
            rule_override: None,
        }),
    )?;
    host.record(
        Some(card),
        None,
        RunEventKind::Handoff(HandoffEvent::HandedOff {
            card: card.clone(),
            to: to.clone(),
            mode,
            by,
            context: Some(rules.context),
        }),
    )?;
    if let Some(debt) = debt_for(host, record, from, to, since, rules.unknown_tier) {
        host.record(
            Some(card),
            None,
            RunEventKind::Card(CardEvent::QualityDebt(debt)),
        )?;
    }
    rules::back_in_line(host, card, &format!("handed to {to}"))
}

/// Debt when `to` is weaker than the strongest owner the card had: its planned owner, the one
/// that stopped, and the one an earlier debt already waits for. An earlier debt keeps its start.
pub fn debt_for(
    host: &Host,
    record: &CardRecord,
    from: Option<&WorkerRef>,
    to: &WorkerRef,
    since: u32,
    unknown: UnknownTier,
) -> Option<QualityDebt> {
    let previous = [
        record.spec.owner.as_ref(),
        from,
        record.debt.as_ref().map(|debt| &debt.needs),
    ];
    let needs = debt::strongest(host, &record.spec, previous.into_iter().flatten(), unknown)?;
    if !debt::weaker(host, &record.spec, to, &needs, unknown) {
        return None;
    }
    Some(QualityDebt {
        since: record.debt.as_ref().map_or(since, |debt| debt.since),
        by: to.clone(),
        needs,
    })
}
