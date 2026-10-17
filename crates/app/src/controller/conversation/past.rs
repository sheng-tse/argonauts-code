//! Earlier conversations, read back from their journals at no token cost: the `/resume` list,
//! which runs each started, and the turns a resumed conversation primes a fresh session with.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use argonauts_core::app::{ConversationEvent, ConversationSummary, TurnReply};
use argonauts_core::artifacts::{self, Artifact};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::context::ContextUsage;
use argonauts_core::names::{ConversationId, ModelRef, SessionKey, TurnId};
use argonauts_core::views::{ContextRow, RecordedContext};
use argonauts_core::{RunId, Spend};
use serde_json::Value;
use time::OffsetDateTime;

use crate::controller::recap::title_from_request;
use crate::conversation_store::ConversationStore;

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
    /// The runs it started, oldest first.
    pub runs: Vec<RunId>,
}

/// How a conversation's record left its session's context, counted as the live row counts it.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct PastContext {
    pub used: u64,
    pub window: Option<u64>,
    pub tool_calls: u32,
    pub spent: Spend,
    /// The CLI's own last count, where the record kept one.
    pub usage: Option<Box<ContextUsage>>,
    /// When the last turn ended, where the record says.
    pub at: Option<OffsetDateTime>,
}

/// What the agents did that the row counts: most of a journal is the rest, which isn't parsed.
const FILLS_AND_CALLS: [&str; 2] = [r#""type":"context_fill""#, r#""type":"tool_started""#];

impl PastContext {
    /// Every turn's fills, calls and spend in order, from the conversation's record. A turn
    /// that compacted leaves the next session empty. `None` before any turn said.
    pub(super) fn read(store: &ConversationStore) -> Option<Self> {
        let mut left: Option<Self> = None;
        let mut compacting: Vec<TurnId> = Vec::new();
        store.each_line(|line| {
            let counts = !ConversationStore::is_agent_line(line)
                || FILLS_AND_CALLS.iter().any(|tag| line.contains(tag));
            if counts && let Ok(event) = serde_json::from_str(line) {
                Self::take(&mut left, &mut compacting, event);
            }
        });
        left
    }

    fn take(left: &mut Option<Self>, compacting: &mut Vec<TurnId>, event: ConversationEvent) {
        match event {
            ConversationEvent::TurnStarted { turn, text, .. }
                if super::side::is_compact_request(&text) =>
            {
                compacting.push(turn);
            }
            ConversationEvent::Agent { event, .. } => {
                let context = left.get_or_insert_default();
                match event {
                    AgentEvent::ToolStarted { .. } => context.tool_calls += 1,
                    AgentEvent::ContextFill { used, size, .. } => {
                        context.used = used;
                        context.window = size.or(context.window);
                    }
                    _ => {}
                }
            }
            ConversationEvent::TurnFinished { turn, spend, .. } => {
                let context = left.get_or_insert_default();
                context.spent = context.spent.plus(&spend);
                if compacting.contains(&turn) {
                    context.used = 0;
                    context.usage = None;
                }
            }
            ConversationEvent::ContextLeft {
                at,
                used,
                window,
                usage,
                ..
            } => {
                let context = left.get_or_insert_default();
                context.used = used;
                context.window = window.or(context.window);
                context.usage = usage;
                context.at = Some(at);
            }
            // The CLI's count was of the session that ran out.
            ConversationEvent::FellBack { .. } => {
                if let Some(context) = left.as_mut() {
                    context.usage = None;
                }
            }
            _ => {}
        }
    }
}

/// The conversation's row on the Context tab as its record left it, until a turn runs again:
/// its last turn's session on the model it ran.
pub(super) fn recorded_row(
    store: &ConversationStore,
    past: &PastConversation,
) -> Option<ContextRow> {
    let context = PastContext::read(store)?;
    let last = past.turns.last()?;
    let session = SessionKey::parse(&format!("{}-{}", past.summary.conversation, last.id))?;
    let window = context
        .window
        .or(context.usage.as_ref().map(|usage| usage.window));
    Some(ContextRow {
        run: None,
        session,
        label: "conversation".to_owned(),
        worker: last.ran(),
        used: context.used,
        window,
        cap: None,
        tool_calls: context.tool_calls,
        spent: context.spent,
        rotations: 0,
        estimated: false,
        counts: false,
        categories: Vec::new(),
        recorded: Some(RecordedContext {
            at: context.at.unwrap_or(past.summary.updated),
            turns: past.summary.turns,
        }),
    })
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
}

/// Every conversation in `conversation_dir` with a turn or a run it started, newest first. One
/// that started a run in `waiting` is marked as waiting on you.
pub(crate) fn list(conversation_dir: &Path, waiting: &[RunId]) -> Vec<ConversationSummary> {
    let homes = run_homes(conversation_dir);
    let mut conversations: Vec<ConversationSummary> = conversation_ids(conversation_dir)
        .iter()
        .filter_map(|conversation| load_with(conversation_dir, conversation, &homes))
        .filter(|past| !past.is_empty())
        .map(|past| {
            let mut summary = past.summary;
            summary.needs_you = past.runs.iter().any(|run| waiting.contains(run));
            summary
        })
        .collect();
    conversations.sort_by(|a, b| {
        b.updated
            .cmp(&a.updated)
            .then(b.conversation.cmp(&a.conversation))
    });
    conversations
}

/// The conversation that started `run`: of those that list it, the first, since a conversation
/// that opened another's run lists it after the one that started it.
pub(crate) fn home_of(conversation_dir: &Path, run: &RunId) -> Option<ConversationId> {
    run_homes(conversation_dir).remove(run)
}

/// Each run, by the conversation that started it.
fn run_homes(conversation_dir: &Path) -> HashMap<RunId, ConversationId> {
    let mut homes = HashMap::new();
    for conversation in conversation_ids(conversation_dir) {
        for run in ConversationStore::open(conversation_dir, &conversation).runs() {
            homes.entry(run).or_insert_with(|| conversation.clone());
        }
    }
    homes
}

/// The conversations that have a journal or runs, oldest first.
fn conversation_ids(conversation_dir: &Path) -> Vec<ConversationId> {
    let mut ids: Vec<ConversationId> = std::fs::read_dir(conversation_dir)
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
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ids.sort_by_key(number);
    ids
}

/// `conversation-012` is 12, so `conversation-1000` comes after it.
fn number(conversation: &ConversationId) -> u32 {
    conversation
        .as_str()
        .strip_prefix("conversation-")
        .and_then(|n| n.parse().ok())
        .unwrap_or(u32::MAX)
}

/// The conversation `conversation`, or `None` when it has neither a journal nor runs.
pub(super) fn load(
    conversation_dir: &Path,
    conversation: &ConversationId,
) -> Option<PastConversation> {
    load_with(conversation_dir, conversation, &run_homes(conversation_dir))
}

/// [`load`], with each run's conversation read already. Only the runs it started are its own:
/// one it only opened belongs to the conversation that started it.
fn load_with(
    conversation_dir: &Path,
    conversation: &ConversationId,
    homes: &HashMap<RunId, ConversationId>,
) -> Option<PastConversation> {
    let store = ConversationStore::open(conversation_dir, conversation);
    let runs: Vec<RunId> = store
        .runs()
        .into_iter()
        .filter(|run| homes.get(run).is_none_or(|home| home == conversation))
        .collect();
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
        needs_you: false,
    };
    Some(PastConversation {
        summary,
        turns,
        runs,
    })
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
            | ConversationEvent::AnswerRetried { .. }
            | ConversationEvent::ContextLeft { .. }
            | ConversationEvent::CommandTyped { .. }
            | ConversationEvent::CommandReplied { .. } => {}
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
        let requests: Vec<&str> = past
            .turns
            .iter()
            .map(|turn| turn.request.as_str())
            .collect();
        assert_eq!(requests, ["hi", "what does the export do?", "and json?"]);
        assert!(past.turns[2].reply.is_none());
    }

    #[test]
    fn the_list_is_newest_first_and_skips_empty_or_missing_journals() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list(&dir.path().join("none"), &[]).is_empty());
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
        let conversations = list(dir.path(), &[]);
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

        let conversations = list(dir.path(), &[]);
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
        let conversations = list(dir.path(), &[]);
        assert_eq!(conversations[0].conversation.as_str(), "conversation-001");
        assert_eq!(conversations[0].title, "hi");
        assert_eq!(conversations[0].runs, 1);
    }

    /// A run belongs to the conversation that started it: one that only opened it lists none of
    /// its own, and the run's home is the first that listed it.
    #[test]
    fn a_run_belongs_to_the_conversation_that_started_it() {
        use crate::conversation_store::ConversationRun;

        let dir = tempfile::tempdir().unwrap();
        let run = RunId::new("run-009").unwrap();
        let kept = ConversationRun {
            run: run.clone(),
            title: "fix the median".to_owned(),
        };
        write(
            dir.path(),
            1,
            &[
                started(1, "fix the median", "claude-code"),
                finished(1, "On it."),
            ],
        );
        for n in [1, 2] {
            ConversationStore::open(dir.path(), &ConversationId::numbered(n))
                .keep_run(&kept)
                .unwrap();
        }
        assert_eq!(home_of(dir.path(), &run), Some(ConversationId::numbered(1)));
        let listed = list(dir.path(), std::slice::from_ref(&run));
        let ids: Vec<&str> = listed
            .iter()
            .map(|summary| summary.conversation.as_str())
            .collect();
        assert_eq!(ids, ["conversation-001"]);
        assert!(listed[0].needs_you, "its run waits on you");
        assert_eq!(listed[0].runs, 1);
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

    /// The council session's own record, its home folder made neutral.
    const COUNCIL: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tui/golden/resumed_council/conversation-001.jsonl"
    ));

    /// Claude Code's count of a session at 75% of 1M, as its adapter keeps it.
    const COUNTED: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapters/claude-code/tests/data/context-usage-full.usage.json"
    ));

    /// An older record keeps no count of its own: the row comes from the fills, calls and
    /// spend of its turns, as the live row added them up, at the time the record last changed.
    #[test]
    fn a_record_without_counts_rebuilds_the_row_from_its_turns() {
        let dir = tempfile::tempdir().unwrap();
        let id = ConversationId::numbered(1);
        let store = ConversationStore::open(dir.path(), &id);
        for line in COUNCIL.lines() {
            store
                .append(&serde_json::from_str::<ConversationEvent>(line).unwrap())
                .unwrap();
        }
        let past = load(dir.path(), &id).unwrap();
        let row = recorded_row(&store, &past).expect("the record says how full");
        assert_eq!(
            (row.used, row.window, row.tool_calls),
            (25_517, Some(1_000_000), 2)
        );
        assert_eq!(
            (row.spent.input_tokens, row.spent.output_tokens),
            (73_708, 916)
        );
        assert!((row.spent.cost_usd - 0.004_032_7).abs() < 1e-9);
        assert_eq!(row.worker, "claude-code/claude-haiku-5-5".parse().unwrap());
        assert_eq!(row.run, None);
        let recorded = row.recorded.expect("read from the record");
        assert_eq!(recorded.turns, 1);
        assert_eq!(recorded.at, past.summary.updated);
    }

    /// Where the record kept how a turn left the context, that is the count, with the CLI's
    /// own; a later compact leaves the next session empty, and a fallback's session has no
    /// count of its own yet.
    #[test]
    fn the_last_turn_s_count_is_what_the_row_reads() {
        let usage: ContextUsage = serde_json::from_str(COUNTED).unwrap();
        let at = OffsetDateTime::from_unix_timestamp(1_760_400_000).unwrap();
        let left = |n: u32, used: u64| ConversationEvent::ContextLeft {
            turn: TurnId::numbered(n),
            at,
            used,
            window: Some(1_000_000),
            usage: Some(Box::new(usage.clone())),
        };
        let fill = |n: u32, used: u64| ConversationEvent::Agent {
            turn: TurnId::numbered(n),
            event: AgentEvent::ContextFill {
                used,
                size: Some(1_000_000),
                sent: None,
            },
        };
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            1,
            &[
                started(1, "hi", "claude-code/claude-opus-5-5"),
                fill(1, 700_000),
                left(1, usage.total_tokens),
                finished(1, "Hi."),
            ],
        );
        let store = |n: u32| ConversationStore::open(dir.path(), &ConversationId::numbered(n));
        let context = PastContext::read(&store(1)).unwrap();
        assert_eq!(context.usage.as_deref(), Some(&usage));
        assert_eq!(context.at, Some(at));
        let past = load(dir.path(), &ConversationId::numbered(1)).unwrap();
        let row = recorded_row(&store(1), &past).unwrap();
        assert_eq!(row.used, usage.total_tokens);
        assert_eq!(row.recorded.unwrap().at, at);

        write(
            dir.path(),
            2,
            &[
                started(1, "hi", "claude-code/claude-opus-5-5"),
                left(1, usage.total_tokens),
                finished(1, "Hi."),
                started(2, "/compact", "claude-code/claude-opus-5-5"),
                finished(2, "The summary."),
            ],
        );
        let context = PastContext::read(&store(2)).unwrap();
        assert_eq!((context.used, context.usage), (0, None));

        write(
            dir.path(),
            3,
            &[
                started(1, "hi", "claude-code/claude-opus-5-5"),
                left(1, usage.total_tokens),
                ConversationEvent::FellBack {
                    turn: TurnId::numbered(1),
                    from: "claude-code".parse().unwrap(),
                    to: "codex".parse().unwrap(),
                    why: "hit its usage limit".to_owned(),
                    told: "your request".to_owned(),
                    signed_out: None,
                },
                finished(1, "Hi."),
            ],
        );
        assert_eq!(PastContext::read(&store(3)).unwrap().usage, None);
    }
}
