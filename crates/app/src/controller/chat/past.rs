//! Earlier conversations, read back from their journals at no token cost: the `/resume` list,
//! and the turns a resumed conversation reprints and primes a fresh session with.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use argonauts_core::app::{ChatEvent, ChatSummary, ReplayTurn, TurnReply};
use argonauts_core::artifacts::{self, Artifact};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::run::Party;
use argonauts_core::names::{ChatId, SessionKey, TurnId, WorkerRef};
use serde_json::Value;
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
    /// Nothing to carry on: no turn, and no run started or opened in it.
    pub(super) fn is_empty(&self) -> bool {
        self.turns.is_empty() && self.summary.runs == 0
    }

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

/// Every conversation in `chat_dir` with a turn or a run it started or opened, newest first.
pub(crate) fn list(chat_dir: &Path) -> Vec<ChatSummary> {
    let ids: BTreeSet<ChatId> = std::fs::read_dir(chat_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let extension = path.extension()?;
            if extension != "jsonl" && extension != "runs" {
                return None;
            }
            ChatId::parse(path.file_stem()?.to_str()?)
        })
        .collect();
    let mut chats: Vec<ChatSummary> = ids
        .iter()
        .filter_map(|chat| load(chat_dir, chat))
        .filter(|past| !past.is_empty())
        .map(|past| past.summary)
        .collect();
    chats.sort_by(|a, b| b.updated.cmp(&a.updated).then(b.chat.cmp(&a.chat)));
    chats
}

/// The conversation `chat`, or `None` when it has neither a journal nor runs.
pub(super) fn load(chat_dir: &Path, chat: &ChatId) -> Option<PastChat> {
    let store = ChatStore::open(chat_dir, chat);
    let runs = store.runs();
    let events = match store.read_events().ok()? {
        Some(events) => events,
        None if !runs.is_empty() => Vec::new(),
        None => return None,
    };
    // Opening a run writes only the runs file, and that's using the conversation too.
    let updated = [store.journal_path(), store.runs_path()]
        .into_iter()
        .filter_map(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        })
        .max()
        .map_or_else(OffsetDateTime::now_utc, OffsetDateTime::from);
    let turns = fold(events);
    let title = store
        .title()
        .or_else(|| turns.first().map(|turn| title_from_request(&turn.request)))
        .or_else(|| store.first_run_title())
        .unwrap_or_default();
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let summary = ChatSummary {
        chat: chat.clone(),
        title,
        updated,
        worker: turns.last().map(|turn| turn.worker.clone()),
        turns: count(turns.len()),
        runs: count(runs.len()),
    };
    Some(PastChat { summary, turns })
}

/// The pages the conversation `chat` published with Claude Code's Artifact tool, oldest first,
/// read off its journal's calls as the screen reads them off the live ones. A
/// page published again counts once, as the most recent. Only a line that names the tool, or
/// could answer a call of it, is parsed.
pub(super) fn artifacts(chat_dir: &Path, chat: &ChatId) -> Vec<Artifact> {
    let mut calls: HashMap<String, Value> = HashMap::new();
    let mut pages: Vec<Artifact> = Vec::new();
    ChatStore::open(chat_dir, chat).each_agent_line(|line| {
        let answers = calls.keys().any(|id| line.contains(id.as_str()));
        if !answers && !line.contains(artifacts::TOOL) {
            return;
        }
        let Ok(ChatEvent::Agent { event, .. }) = serde_json::from_str(line) else {
            return;
        };
        match event {
            AgentEvent::ToolStarted {
                id, name, input, ..
            } if artifacts::publishes(&name, &input) => {
                calls.insert(id, input);
            }
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                let page = calls
                    .remove(&id)
                    .filter(|_| ok)
                    .and_then(|input| artifacts::published(&input, output.as_deref()?));
                if let Some(page) = page {
                    pages.retain(|known| known.url != page.url);
                    pages.push(page);
                }
            }
            _ => {}
        }
    });
    pages
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
            ChatEvent::HandedOff { turn, to, .. } => {
                if let Some(past) = turns.iter_mut().find(|past| past.id == turn) {
                    past.worker = to;
                }
            }
            // What you sent while it worked is part of what the turn answered.
            ChatEvent::Joined { turn, text } => {
                if let Some(past) = turns.iter_mut().find(|past| past.id == turn) {
                    past.request = format!("{}\n\n{text}", past.request);
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
            continues: false,
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

    /// A conversation's pages come back from its journal: each call that
    /// published one, the latest of a page published twice, and none for a call that failed,
    /// only listed the artifacts, or belongs to another tool.
    #[test]
    fn a_journals_artifact_calls_read_back_as_its_pages() {
        let call = |id: &str, name: &str, input: Value| ChatEvent::Agent {
            turn: TurnId::numbered(1),
            event: AgentEvent::ToolStarted {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
                parent: None,
            },
        };
        let done = |id: &str, ok: bool, output: &str| ChatEvent::Agent {
            turn: TurnId::numbered(1),
            event: AgentEvent::ToolFinished {
                id: id.to_owned(),
                ok,
                output: Some(output.to_owned()),
                parent: None,
            },
        };
        let file = |path: &str| serde_json::json!({"file_path": path});
        let link = |id: &str| format!("https://claude.ai/code/artifact/{id}");
        let (overview, report) = (link("0b1e7c9a"), link("7d2c4b18"));
        let published = |path: &str, url: &str| format!("Published {path} at {url} (Version 1)");
        let dir = tempfile::tempdir().unwrap();
        assert!(artifacts(dir.path(), &ChatId::numbered(1)).is_empty());
        write(
            dir.path(),
            1,
            &[
                started(1, "write it up", "claude-code"),
                call("toolu_01", "Artifact", file("out/overview.html")),
                done("toolu_01", true, &published("out/overview.html", &overview)),
                call("toolu_02", "Read", file("out/overview.html")),
                done(
                    "toolu_02",
                    true,
                    &published("out/overview.html", &link("x")),
                ),
                call(
                    "toolu_03",
                    "Artifact",
                    serde_json::json!({"action": "list"}),
                ),
                done("toolu_03", true, &format!("1 artifact: {overview}")),
                call("toolu_04", "Artifact", file("out/broken.html")),
                done("toolu_04", false, "Publishing was refused"),
                call("toolu_05", "Artifact", file("out/report.html")),
                done("toolu_05", true, &published("out/report.html", &report)),
                call("toolu_06", "Artifact", file("out/overview.html")),
                done(
                    "toolu_06",
                    true,
                    &format!("Updated out/overview.html at {overview} (Version 2)"),
                ),
                finished(1, "Both are up."),
            ],
        );
        let pages = artifacts(dir.path(), &ChatId::numbered(1));
        let seen: Vec<(&str, &str, bool)> = pages
            .iter()
            .map(|page| (page.name.as_str(), page.url.as_str(), page.updated))
            .collect();
        assert_eq!(
            seen,
            [
                ("report", report.as_str(), false),
                ("overview", overview.as_str(), true)
            ]
        );
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

    /// One that only started or opened runs is listed, named after its first run, and opening a
    /// run in an older one makes that one the newest again. One claimed and never used isn't.
    #[test]
    fn a_conversation_with_only_runs_is_listed_by_its_last_change() {
        use crate::chat_store::ChatRun;
        use argonauts_core::RunId;
        use std::time::{Duration, SystemTime};

        fn touch(path: &Path, hours_ago: u64) {
            let at = SystemTime::now() - Duration::from_secs(hours_ago * 3600);
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(at).unwrap();
        }
        fn kept(run: &str, title: &str) -> ChatRun {
            ChatRun {
                run: RunId::new(run).unwrap(),
                title: title.to_owned(),
            }
        }

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), 1, &[started(1, "hi", "claude-code")]);
        let chatted = ChatStore::open(dir.path(), &ChatId::numbered(1));
        touch(chatted.journal_path(), 2);
        let only_runs = ChatStore::open(dir.path(), &ChatId::numbered(2));
        only_runs
            .keep_run(&kept("run-009", "fix the median"))
            .unwrap();
        touch(only_runs.runs_path(), 1);
        ChatStore::claim(dir.path()).unwrap();

        let chats = list(dir.path());
        let shown: Vec<(&str, &str, u32, u32)> = chats
            .iter()
            .map(|chat| {
                (
                    chat.chat.as_str(),
                    chat.title.as_str(),
                    chat.turns,
                    chat.runs,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("chat-002", "fix the median", 0, 1),
                ("chat-001", "hi", 1, 0)
            ]
        );
        assert!(!load(dir.path(), &ChatId::numbered(2)).unwrap().is_empty());

        chatted
            .keep_run(&kept("run-010", "rename the loader"))
            .unwrap();
        let chats = list(dir.path());
        assert_eq!(chats[0].chat.as_str(), "chat-001");
        assert_eq!(chats[0].title, "hi");
        assert_eq!(chats[0].runs, 1);
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
