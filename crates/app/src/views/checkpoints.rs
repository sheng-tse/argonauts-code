use argonauts_core::CardId;
use argonauts_core::events::card::CardEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::views::CheckpointRow;

/// `/rewind`: each handoff checkpoint of `card`, or of every card, oldest first.
pub(crate) fn checkpoint_rows(events: &[RunEvent], card: Option<&CardId>) -> Vec<CheckpointRow> {
    let mut rows: Vec<CheckpointRow> = Vec::new();
    for event in events {
        let Some(this_card) = &event.card_id else {
            continue;
        };
        if card.is_some_and(|wanted| wanted != this_card) {
            continue;
        }
        match &event.kind {
            RunEventKind::Card(CardEvent::CheckpointTagged {
                n,
                commit,
                from,
                to,
                reason,
            }) => rows.push(CheckpointRow {
                card: this_card.clone(),
                n: *n,
                at: event.ts,
                commit: commit.clone(),
                from: from.clone(),
                to: to.clone(),
                reason: sanitize_for_terminal(reason),
                reverted_here: false,
            }),
            RunEventKind::Card(CardEvent::Reverted { to, .. }) => {
                if let Some(row) = rows
                    .iter_mut()
                    .rev()
                    .find(|row| row.card == *this_card && row.n == *to)
                {
                    row.reverted_here = true;
                }
            }
            _ => {}
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::card_event;
    use argonauts_core::names::WorkerRef;

    fn tagged(seq: u64, card: &CardId, n: u32, to: &str) -> RunEvent {
        card_event(
            seq,
            card,
            CardEvent::CheckpointTagged {
                n,
                commit: "0123456789abcdef".to_owned(),
                from: WorkerRef::new("claude-code", Some("opus".to_owned())),
                to: vec![WorkerRef::new(to, None)],
                reason: "usage limit, resets at 3:45 PM".to_owned(),
            },
        )
    }

    #[test]
    fn checkpoint_rows_list_each_handoff_oldest_first() {
        let export = CardId::new("task-001").unwrap();
        let docs = CardId::new("task-002").unwrap();
        let events = vec![
            tagged(0, &export, 1, "codex"),
            tagged(1, &docs, 1, "antigravity-cli"),
            tagged(2, &export, 2, "opencode"),
            card_event(
                3,
                &export,
                CardEvent::Reverted {
                    to: 1,
                    kept_on: "argonauts/run/task-001-replaced-1".to_owned(),
                },
            ),
        ];
        let all = checkpoint_rows(&events, None);
        assert_eq!(all.len(), 3);
        let mine = checkpoint_rows(&events, Some(&export));
        let picked: Vec<(u32, &str, bool)> = mine
            .iter()
            .map(|row| (row.n, row.to[0].worker.as_str(), row.reverted_here))
            .collect();
        assert_eq!(picked, [(1, "codex", true), (2, "opencode", false)]);
        assert_eq!(mine[0].from.to_string(), "claude-code/opus");
        assert_eq!(mine[0].reason, "usage limit, resets at 3:45 PM");
        assert!(checkpoint_rows(&events, Some(&CardId::new("task-003").unwrap())).is_empty());
    }
}
