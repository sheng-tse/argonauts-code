//! Earlier conversations, read back from their journals at no token cost: the `/resume` list,
//! and the turns a resumed conversation reprints and primes a fresh session with.

use std::path::Path;

use argonauts_core::app::{ChatEvent, ChatSummary, ReplayTurn, TurnReply};
use argonauts_core::events::run::Party;
use argonauts_core::names::{ChatId, SessionKey, TurnId, WorkerRef};
use time::OffsetDateTime;

use super::excerpt::reply_text;
use crate::chat_store::ChatStore;
use crate::controller::recap::title_from_request;

/// How many of the last replay items a resumed conversation prints word for word, as a resumed
/// run does. Older ones fold into the Ctrl+O history.
pub(super) const SHOWN_TURNS: usize = 6;

/// One turn of an earlier conversation.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PastTurn {
    pub id: TurnId,
    pub request: String,
    pub worker: WorkerRef,
    /// The answer, when the turn finished.
    pub reply: Option<TurnReply>,
}

/// An earlier conversation as its journal holds it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PastChat {
    pub summary: ChatSummary,
    pub turns: Vec<PastTurn>,
}

impl PastChat {
    /// The turns a fresh session is primed with: from the last `/compact` on, since its
    /// summary stands for everything before it.
    pub(super) fn since_compacted(&self) -> &[PastTurn] {
        let start = self
            .turns
            .iter()
            .rposition(|turn| {
                super::side::is_compact_request(&turn.request) && turn.reply.is_some()
            })
            .unwrap_or(0);
        &self.turns[start..]
    }

    /// The highest turn number, so the next turn never reuses an id.
    pub(super) fn last_turn_number(&self) -> u32 {
        self.turns
            .iter()
            .filter_map(|turn| turn_number(&turn.id))
            .max()
            .unwrap_or(0)
    }

    /// Each request and its answer as you saw them, oldest first. A stopped or failed turn
    /// shows only the request.
    pub(super) fn replay(&self) -> Vec<ReplayTurn> {
        let mut items = Vec::new();
        for turn in &self.turns {
            items.push(ReplayTurn {
                from: Party::User,
                text: turn.request.clone(),
            });
            if let Some(reply) = &turn.reply {
                items.push(ReplayTurn {
                    // Chat turns have no run session; the turn names the answer instead.
                    from: Party::Agent {
                        session: SessionKey::parse(turn.id.as_str())
                            .unwrap_or_else(|| SessionKey::numbered(0)),
                        worker: turn.worker.clone(),
                    },
                    text: reply_text(reply).to_owned(),
                });
            }
        }
        items
    }
}

/// Every conversation in `chat_dir` that has at least one turn, newest first.
pub(crate) fn list(chat_dir: &Path) -> Vec<ChatSummary> {
    let mut chats: Vec<ChatSummary> = std::fs::read_dir(chat_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "jsonl" {
                return None;
            }
            let chat = ChatId::parse(path.file_stem()?.to_str()?)?;
            let past = load(chat_dir, &chat)?;
            (past.summary.turns > 0).then_some(past.summary)
        })
        .collect();
    chats.sort_by(|a, b| b.updated.cmp(&a.updated).then(b.chat.cmp(&a.chat)));
    chats
}

/// The conversation `chat`, or `None` when it has no journal.
pub(super) fn load(chat_dir: &Path, chat: &ChatId) -> Option<PastChat> {
    let store = ChatStore::open(chat_dir, chat);
    let events = store.read_events().ok()??;
    let updated = std::fs::metadata(store.journal_path())
        .and_then(|meta| meta.modified())
        .map(OffsetDateTime::from)
        .unwrap_or_else(|_| OffsetDateTime::now_utc());
    let turns = fold(events);
    let summary = ChatSummary {
        chat: chat.clone(),
        title: store.title().unwrap_or_else(|| {
            turns
                .first()
                .map(|turn| title_from_request(&turn.request))
                .unwrap_or_default()
        }),
        updated,
        worker: turns.last().map(|turn| turn.worker.clone()),
        turns: u32::try_from(turns.len()).unwrap_or(u32::MAX),
    };
    Some(PastChat { summary, turns })
}

/// Turns in the order they started. A fallback records a turn again under the same id, naming
/// the worker that took it over.
fn fold(events: Vec<ChatEvent>) -> Vec<PastTurn> {
    let mut turns: Vec<PastTurn> = Vec::new();
    for event in events {
        match event {
            ChatEvent::TurnStarted {
                turn, text, worker, ..
            } => match turns.iter_mut().find(|past| past.id == turn) {
                Some(past) => past.worker = worker,
                None => turns.push(PastTurn {
                    id: turn,
                    request: text,
                    worker,
                    reply: None,
                }),
            },
            ChatEvent::TurnFinished { turn, reply, .. } => {
                if let Some(past) = turns.iter_mut().find(|past| past.id == turn) {
                    past.reply = Some(reply);
                }
            }
            ChatEvent::Agent { .. }
            | ChatEvent::TurnFailed { .. }
            | ChatEvent::TurnCancelled { .. } => {}
        }
    }
    turns
}

fn turn_number(turn: &TurnId) -> Option<u32> {
    turn.as_str().rsplit('-').next()?.parse().ok()
}

/// `chat-3` and `chat-003` name the same conversation.
pub(crate) fn normalize(chat: &ChatId) -> ChatId {
    chat.as_str()
        .strip_prefix("chat-")
        .and_then(|n| n.parse::<u32>().ok())
        .map_or_else(|| chat.clone(), ChatId::numbered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::Spend;

    fn started(n: u32, text: &str, worker: &str) -> ChatEvent {
        ChatEvent::TurnStarted {
            turn: TurnId::numbered(n),
            text: text.to_owned(),
            worker: worker.parse().unwrap(),
            read_only: true,
            commit: "abc".to_owned(),
            uncommitted: Vec::new(),
            settings_sha256: None,
        }
    }

    fn finished(n: u32, answer: &str) -> ChatEvent {
        ChatEvent::TurnFinished {
            turn: TurnId::numbered(n),
            reply: TurnReply::Answer {
                markdown: answer.to_owned(),
            },
            spend: Spend::default(),
            elapsed_ms: 10,
        }
    }

    fn write(chat_dir: &Path, n: u32, events: &[ChatEvent]) {
        let store = ChatStore::open(chat_dir, &ChatId::numbered(n));
        for event in events {
            store.append(event).unwrap();
        }
    }

    #[test]
    fn a_journal_reads_back_as_turns_with_a_title_and_the_last_agent() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            1,
            &[
                started(1, "hi", "claude-code"),
                finished(1, "Hi. Ask me anything."),
                started(2, "what does the export do?", "claude-code"),
                ChatEvent::TurnFailed {
                    turn: TurnId::numbered(2),
                    error: "down".to_owned(),
                },
                started(2, "what does the export do?", "codex"),
                finished(2, "It writes CSV."),
                started(3, "and json?", "codex"),
                ChatEvent::TurnCancelled {
                    turn: TurnId::numbered(3),
                },
            ],
        );
        let past = load(dir.path(), &ChatId::numbered(1)).unwrap();
        assert_eq!(past.summary.title, "hi");
        assert_eq!(past.summary.turns, 3);
        assert_eq!(past.summary.worker, Some("codex".parse().unwrap()));
        assert_eq!(past.last_turn_number(), 3);
        assert_eq!(past.turns[1].worker, "codex".parse().unwrap());
        let replay = past.replay();
        let replay: Vec<(bool, &str)> = replay
            .iter()
            .map(|turn| (turn.from == Party::User, turn.text.as_str()))
            .collect();
        assert_eq!(
            replay,
            vec![
                (true, "hi"),
                (false, "Hi. Ask me anything."),
                (true, "what does the export do?"),
                (false, "It writes CSV."),
                (true, "and json?"),
            ]
        );
    }

    #[test]
    fn the_list_is_newest_first_and_skips_empty_or_missing_journals() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list(&dir.path().join("none")).is_empty());
        write(dir.path(), 1, &[started(1, "first chat", "claude-code")]);
        write(dir.path(), 2, &[]);
        std::fs::write(dir.path().join("chat-002.jsonl"), "").unwrap();
        // Written later, so newer.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(dir.path(), 3, &[started(1, "second chat", "codex")]);
        let chats = list(dir.path());
        let ids: Vec<&str> = chats.iter().map(|chat| chat.chat.as_str()).collect();
        assert_eq!(ids, vec!["chat-003", "chat-001"]);
        assert_eq!(chats[0].title, "second chat");
    }

    #[test]
    fn short_and_padded_ids_name_one_conversation() {
        let short = ChatId::parse("chat-3").unwrap();
        assert_eq!(normalize(&short), ChatId::numbered(3));
        assert_eq!(normalize(&ChatId::numbered(12)), ChatId::numbered(12));
        let odd = ChatId::parse("chat-x").unwrap();
        assert_eq!(normalize(&odd), odd);
    }
}
