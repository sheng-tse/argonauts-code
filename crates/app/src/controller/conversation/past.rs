//! Earlier conversations, read back from their journals at no token cost: the `/resume` list,
//! and the turns a resumed conversation reprints and primes a fresh session with.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use argonauts_core::app::{ConversationEvent, ConversationSummary, ReplayTurn, TurnReply};
use argonauts_core::artifacts::{self, Artifact};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::run::Party;
use argonauts_core::names::{ConversationId, ModelRef, SessionKey, TurnId};
use serde_json::Value;
use time::OffsetDateTime;

use super::excerpt::reply_text;
use crate::controller::recap::title_from_request;
use crate::conversation_store::ConversationStore;

/// How many of the last replay items a resumed conversation prints word for word, as a resumed
/// run does. Older ones fold into the Ctrl+O history.
pub(super) const SHOWN_TURNS: usize = 6;

/// One turn of an earlier conversation.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PastTurn {
    pub id: TurnId,
    pub request: String,
    pub worker: ModelRef,
    /// The model its CLI said it ran, for a turn whose agent named none.
    pub model: Option<String>,
    /// The answer, when the turn finished.
    pub reply: Option<TurnReply>,
}

impl PastTurn {
    /// Its agent on the model it ran, so a resumed conversation names it as the header does.
    fn ran(&self) -> ModelRef {
        let mut worker = self.worker.clone();
        if worker.model.is_none() {
            worker.model = self.model.clone();
        }
        worker
    }
}

/// An earlier conversation as its journal holds it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PastConversation {
    pub summary: ConversationSummary,
    pub turns: Vec<PastTurn>,
}

impl PastConversation {
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
                steps: Vec::new(),
            });
            if let Some(reply) = &turn.reply {
                items.push(ReplayTurn {
                    // Conversation turns have no run session; the turn names the answer instead.
                    from: Party::Agent {
                        session: SessionKey::parse(turn.id.as_str())
                            .unwrap_or_else(|| SessionKey::numbered(0)),
                        worker: turn.worker.clone(),
                    },
                    text: reply_text(reply).to_owned(),
                    steps: Vec::new(),
                });
            }
        }
        items
    }
}

/// Every conversation in `conversation_dir` with a turn or a run it started or opened, newest
/// first.
pub(crate) fn list(conversation_dir: &Path) -> Vec<ConversationSummary> {
    let ids: BTreeSet<ConversationId> = std::fs::read_dir(conversation_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let extension = path.extension()?;
            if extension != "jsonl" && extension != "runs" {
                return None;
            }
            ConversationId::parse(path.file_stem()?.to_str()?)
        })
        .collect();
    let mut conversations: Vec<ConversationSummary> = ids
        .iter()
        .filter_map(|conversation| load(conversation_dir, conversation))
        .filter(|past| !past.is_empty())
        .map(|past| past.summary)
        .collect();
    conversations.sort_by(|a, b| {
        b.updated
            .cmp(&a.updated)
            .then(b.conversation.cmp(&a.conversation))
    });
    conversations
}

/// The conversation `conversation`, or `None` when it has neither a journal nor runs.
pub(super) fn load(
    conversation_dir: &Path,
    conversation: &ConversationId,
) -> Option<PastConversation> {
    let store = ConversationStore::open(conversation_dir, conversation);
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
    let mut turns = fold(events);
    models_ran(&store, &mut turns);
    let title = store
        .title()
        .or_else(|| turns.first().map(|turn| title_from_request(&turn.request)))
        .or_else(|| store.first_run_title())
        .unwrap_or_default();
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let summary = ConversationSummary {
        conversation: conversation.clone(),
        title,
        updated,
        worker: turns.last().map(PastTurn::ran),
        turns: count(turns.len()),
        runs: count(runs.len()),
    };
    Some(PastConversation { summary, turns })
}

/// The pages the conversation `conversation` published with Claude Code's Artifact tool, oldest
/// first, read off its journal's calls as the screen reads them off the live ones. A page published
/// again counts once, as the most recent. Only a line that names the tool, or could answer a call
/// of it, is parsed.
pub(super) fn artifacts(conversation_dir: &Path, conversation: &ConversationId) -> Vec<Artifact> {
    let mut calls: HashMap<String, Value> = HashMap::new();
    let mut pages: Vec<Artifact> = Vec::new();
    ConversationStore::open(conversation_dir, conversation).each_agent_line(|line| {
        let answers = calls.keys().any(|id| line.contains(id.as_str()));
        if !answers && !line.contains(artifacts::TOOL) {
            return;
        }
        let Ok(ConversationEvent::Agent { event, .. }) = serde_json::from_str(line) else {
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
/// the worker it fell back to.
fn fold(events: Vec<ConversationEvent>) -> Vec<PastTurn> {
    let mut turns: Vec<PastTurn> = Vec::new();
    for event in events {
        match event {
            ConversationEvent::TurnStarted {
                turn, text, worker, ..
            } => match turns.iter_mut().find(|past| past.id == turn) {
                Some(past) => past.worker = worker,
                None => turns.push(PastTurn {
                    id: turn,
                    request: text,
                    worker,
                    model: None,
                    reply: None,
                }),
            },
            ConversationEvent::TurnFinished { turn, reply, .. } => {
                if let Some(past) = turns.iter_mut().find(|past| past.id == turn) {
                    past.reply = Some(reply);
                }
            }
            ConversationEvent::FellBack { turn, to, .. } => {
                if let Some(past) = turns.iter_mut().find(|past| past.id == turn) {
                    past.worker = to;
                }
            }
            // What you sent while it worked is part of what the turn answered.
            ConversationEvent::Joined { turn, text } => {
                if let Some(past) = turns.iter_mut().find(|past| past.id == turn) {
                    past.request = format!("{}\n\n{text}", past.request);
                }
            }
            ConversationEvent::Agent { .. }
            | ConversationEvent::TurnFailed { .. }
            | ConversationEvent::TurnCancelled { .. }
            | ConversationEvent::AnswerRetried { .. } => {}
        }
    }
    turns
}

/// The model each turn's CLI said it ran, from the first line of each session it started; a
/// later session of the same turn, as after a fallback, says it again.
fn models_ran(store: &ConversationStore, turns: &mut [PastTurn]) {
    store.each_agent_line(|line| {
        if !line.contains(r#""type":"init""#) {
            return;
        }
        let Ok(ConversationEvent::Agent {
            turn,
            event: AgentEvent::Init { model, .. },
        }) = serde_json::from_str(line)
        else {
            return;
        };
        if let Some(past) = turns.iter_mut().find(|past| past.id == turn) {
            past.model = model.filter(|model| !model.trim().is_empty());
        }
    });
}

fn turn_number(turn: &TurnId) -> Option<u32> {
    turn.as_str().rsplit('-').next()?.parse().ok()
}

/// `conversation-3` and `conversation-003` name the same conversation.
pub(crate) fn normalize(conversation: &ConversationId) -> ConversationId {
    conversation
        .as_str()
        .strip_prefix("conversation-")
        .and_then(|n| n.parse::<u32>().ok())
        .map_or_else(|| conversation.clone(), ConversationId::numbered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::Spend;

    fn started(n: u32, text: &str, worker: &str) -> ConversationEvent {
        ConversationEvent::TurnStarted {
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

    fn finished(n: u32, answer: &str) -> ConversationEvent {
        ConversationEvent::TurnFinished {
            turn: TurnId::numbered(n),
            reply: TurnReply::Answer {
                markdown: answer.to_owned(),
            },
            spend: Spend::default(),
            elapsed_ms: 10,
        }
    }

    fn write(conversation_dir: &Path, n: u32, events: &[ConversationEvent]) {
        let store = ConversationStore::open(conversation_dir, &ConversationId::numbered(n));
        for event in events {
            store.append(event).unwrap();
        }
    }

    /// A conversation's pages come back from its journal: each call that
    /// published one, the latest of a page published twice, and none for a call that failed,
    /// only listed the artifacts, or belongs to another tool.
    #[test]
    fn a_journals_artifact_calls_read_back_as_its_pages() {
        let call = |id: &str, name: &str, input: Value| ConversationEvent::Agent {
            turn: TurnId::numbered(1),
            event: AgentEvent::ToolStarted {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
                parent: None,
            },
        };
        let done = |id: &str, ok: bool, output: &str| ConversationEvent::Agent {
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
        assert!(artifacts(dir.path(), &ConversationId::numbered(1)).is_empty());
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
        let pages = artifacts(dir.path(), &ConversationId::numbered(1));
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
                ConversationEvent::TurnFailed {
                    turn: TurnId::numbered(2),
                    error: "down".to_owned(),
                },
                started(2, "what does the export do?", "codex"),
                finished(2, "It writes CSV."),
                started(3, "and json?", "codex"),
                ConversationEvent::TurnCancelled {
                    turn: TurnId::numbered(3),
                },
            ],
        );
        let past = load(dir.path(), &ConversationId::numbered(1)).unwrap();
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
        write(
            dir.path(),
            1,
            &[started(1, "first conversation", "claude-code")],
        );
        write(dir.path(), 2, &[]);
        std::fs::write(dir.path().join("conversation-002.jsonl"), "").unwrap();
        // Written later, so newer.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(dir.path(), 3, &[started(1, "second conversation", "codex")]);
        let conversations = list(dir.path());
        let ids: Vec<&str> = conversations
            .iter()
            .map(|conversation| conversation.conversation.as_str())
            .collect();
        assert_eq!(ids, vec!["conversation-003", "conversation-001"]);
        assert_eq!(conversations[0].title, "second conversation");
    }

    /// One that only started or opened runs is listed, named after its first run, and opening a
    /// run in an older one makes that one the newest again. One claimed and never used isn't.
    #[test]
    fn a_conversation_with_only_runs_is_listed_by_its_last_change() {
        use crate::conversation_store::ConversationRun;
        use argonauts_core::RunId;
        use std::time::{Duration, SystemTime};

        fn touch(path: &Path, hours_ago: u64) {
            let at = SystemTime::now() - Duration::from_secs(hours_ago * 3600);
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(at).unwrap();
        }
        fn kept(run: &str, title: &str) -> ConversationRun {
            ConversationRun {
                run: RunId::new(run).unwrap(),
                title: title.to_owned(),
            }
        }

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), 1, &[started(1, "hi", "claude-code")]);
        let chatted = ConversationStore::open(dir.path(), &ConversationId::numbered(1));
        touch(chatted.journal_path(), 2);
        let only_runs = ConversationStore::open(dir.path(), &ConversationId::numbered(2));
        only_runs
            .keep_run(&kept("run-009", "fix the median"))
            .unwrap();
        touch(only_runs.runs_path(), 1);
        ConversationStore::claim(dir.path()).unwrap();

        let conversations = list(dir.path());
        let shown: Vec<(&str, &str, u32, u32)> = conversations
            .iter()
            .map(|conversation| {
                (
                    conversation.conversation.as_str(),
                    conversation.title.as_str(),
                    conversation.turns,
                    conversation.runs,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("conversation-002", "fix the median", 0, 1),
                ("conversation-001", "hi", 1, 0)
            ]
        );
        assert!(
            !load(dir.path(), &ConversationId::numbered(2))
                .unwrap()
                .is_empty()
        );

        chatted
            .keep_run(&kept("run-010", "rename the loader"))
            .unwrap();
        let conversations = list(dir.path());
        assert_eq!(conversations[0].conversation.as_str(), "conversation-001");
        assert_eq!(conversations[0].title, "hi");
        assert_eq!(conversations[0].runs, 1);
    }

    #[test]
    fn short_and_padded_ids_name_one_conversation() {
        let short = ConversationId::parse("conversation-3").unwrap();
        assert_eq!(normalize(&short), ConversationId::numbered(3));
        assert_eq!(
            normalize(&ConversationId::numbered(12)),
            ConversationId::numbered(12)
        );
        let odd = ConversationId::parse("conversation-x").unwrap();
        assert_eq!(normalize(&odd), odd);
    }

    /// A conversation whose agent named no model is named by the one its CLI said it ran.
    #[test]
    fn a_resumed_conversation_names_the_model_its_cli_ran() {
        let dir = tempfile::tempdir().unwrap();
        let init = |model: Option<&str>| ConversationEvent::Agent {
            turn: TurnId::numbered(1),
            event: AgentEvent::Init {
                session_id: Some("671d00cf".to_owned()),
                model: model.map(str::to_owned),
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            },
        };
        write(
            dir.path(),
            1,
            &[
                started(1, "use a workflow for this", "claude-code"),
                init(Some("claude-haiku-5-5")),
                finished(1, "Starting a workflow."),
            ],
        );
        let past = load(dir.path(), &ConversationId::numbered(1)).unwrap();
        assert_eq!(
            past.summary.worker,
            Some("claude-code/claude-haiku-5-5".parse().unwrap())
        );
        // The turn keeps the agent it asked for, so the session still resumes by its CLI.
        assert_eq!(past.turns[0].worker, "claude-code".parse().unwrap());

        // An agent named with its model keeps that name, and a session that said nothing
        // leaves the CLI alone.
        write(
            dir.path(),
            2,
            &[
                started(1, "hi", "claude-code/opus"),
                init(Some("claude-opus-5-5")),
                finished(1, "Hi."),
            ],
        );
        let past = load(dir.path(), &ConversationId::numbered(2)).unwrap();
        assert_eq!(
            past.summary.worker,
            Some("claude-code/opus".parse().unwrap())
        );
        write(
            dir.path(),
            3,
            &[started(1, "hi", "codex"), init(None), finished(1, "Hi.")],
        );
        let past = load(dir.path(), &ConversationId::numbered(3)).unwrap();
        assert_eq!(past.summary.worker, Some("codex".parse().unwrap()));
    }
}
