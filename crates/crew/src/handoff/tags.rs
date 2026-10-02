//! Checkpoint tags at each handoff. A tag stays put while the card's branch moves. Its message
//! records the time, the original worker, why it stopped, and who took over.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardEvent;
use argonauts_core::handoff::CheckpointTag;
use argonauts_core::names::WorkerRef;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::handoff::rules;

/// Tags the card's last commit as the next `handoff-<n>` and records `Card::CheckpointTagged`.
pub async fn tag(
    host: &Host,
    card: &CardId,
    from: &WorkerRef,
    to: &[WorkerRef],
    reason: &str,
) -> EngineResult<CheckpointTag> {
    let record = rules::record(host, card)?;
    let branch = record
        .branch
        .ok_or_else(|| EngineError::Refused(format!("{card} has no branch to tag yet")))?;
    let root = host.repo().root.clone();
    let commit = host.git().resolve(&root, &branch).await?;
    let tag = CheckpointTag {
        run: host.run_id().clone(),
        card: card.clone(),
        n: record.handoffs + 1,
    };
    let text = message(host.now(), from, to, reason);
    host.git()
        .tag(&root, &tag.tag_name(), &commit, &text)
        .await?;
    host.record(
        Some(card),
        None,
        RunEventKind::Card(CardEvent::CheckpointTagged {
            n: tag.n,
            commit,
            from: from.clone(),
            to: to.to_vec(),
            reason: reason.to_owned(),
        }),
    )?;
    Ok(tag)
}

/// The tag's message: when, who stopped, why, and who took over.
pub fn message(at: OffsetDateTime, from: &WorkerRef, to: &[WorkerRef], reason: &str) -> String {
    let when = at.format(&Rfc3339).unwrap_or_else(|_| at.to_string());
    let to = to
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("Handoff checkpoint\n\ntime: {when}\nfrom: {from}\nreason: {reason}\nto: {to}\n")
}

/// The card's checkpoints and the commit each one marks, oldest first, for `/rewind`.
pub async fn list(host: &Host, card: &CardId) -> EngineResult<Vec<(CheckpointTag, String)>> {
    let record = rules::record(host, card)?;
    let root = host.repo().root.clone();
    let mut found = Vec::new();
    for n in 1..=record.handoffs {
        let tag = CheckpointTag {
            run: host.run_id().clone(),
            card: card.clone(),
            n,
        };
        if let Some(commit) = host.git().tag_target(&root, &tag.tag_name()).await? {
            found.push((tag, commit));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn the_message_says_when_who_why_and_to_whom() {
        let text = message(
            datetime!(2026-09-30 15:45 UTC),
            &WorkerRef::new("claude-code", Some("opus".into())),
            &[
                WorkerRef::new("codex", Some("gpt-5.6".into())),
                WorkerRef::new("agy", None),
            ],
            "usage limit, resets at 3:45 PM",
        );
        assert!(text.contains("time: 2026-09-30T15:45:00Z"), "{text}");
        assert!(text.contains("from: claude-code/opus"));
        assert!(text.contains("reason: usage limit, resets at 3:45 PM"));
        assert!(text.contains("to: codex/gpt-5.6, agy"));
    }
}
