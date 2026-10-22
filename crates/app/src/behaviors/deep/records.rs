//! Every action Argonauts Code recorded, read for the deeper map: each run session's tool calls
//! (`state/<project>/runs/<run>/events.jsonl`) and the main agent's own in your conversations
//! (`state/<project>/conversation/<id>.jsonl`). Reading writes nothing.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::action::{ActionKey, DeepAction, PatchCounts};
use crate::behaviors::turn_times::TurnTimes;
use argonauts_core::app::ConversationEvent;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::{repo_state, run_files};
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::{AgentEvent, Paths};

/// The run of a conversation's action, so pruning finds it when the conversation is deleted.
pub(crate) const CONVERSATION: &str = "conversation";

/// Every recorded action under `paths`, oldest project first, in the order each was taken.
pub(crate) fn recorded(paths: &Paths) -> Vec<DeepAction> {
    let mut actions = Vec::new();
    for project_dir in sorted_dirs(&paths.home().join("state")) {
        let Some(id) = project_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        let project = Project {
            repo: id.clone(),
            name: project_name(&id),
        };
        for run_dir in sorted_dirs(&project_dir.join(repo_state::RUNS)) {
            let run = run_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let events = read_events(&run_dir.join(run_files::EVENTS));
            actions.extend(run_actions(&events, &run, &project));
        }
        actions.extend(conversation_actions(
            &project_dir.join(repo_state::CONVERSATION),
            &project,
        ));
    }
    actions
}

/// The events of a run's log the map reads: sessions starting, and tool calls with their
/// patches and results. Most of a log is what the agents said, so a line is parsed only when it
/// could be one of these. A torn line is skipped.
fn read_events(path: &Path) -> Vec<RunEvent> {
    const WANTED: [&str; 5] = [
        "\"tool_started\"",
        "\"tool_finished\"",
        "\"file_patch\"",
        "\"started\"",
        "\"init\"",
    ];
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| WANTED.iter().any(|wanted| line.contains(wanted)))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Where an action was taken: the project's state folder, and the folder's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Project {
    /// Such as `council-3f9a1c2b4d5e`.
    pub repo: String,
    /// Such as `council`.
    pub name: String,
}

/// A project's state folder is its folder's name and a hash, `council-3f9a1c2b4d5e`.
pub(crate) fn project_name(id: &str) -> String {
    match id.rsplit_once('-') {
        Some((name, hash)) if hash.len() == 12 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            name.to_owned()
        }
        _ => id.to_owned(),
    }
}

fn sorted_dirs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Calls in flight, by session and call id, until each finishes.
#[derive(Default)]
struct Calls {
    order: Vec<(String, String)>,
    taken: HashMap<(String, String), DeepAction>,
}

impl Calls {
    fn started(&mut self, session: &str, id: &str, action: DeepAction) {
        let key = (session.to_owned(), id.to_owned());
        if self.taken.insert(key.clone(), action).is_none() {
            self.order.push(key);
        }
    }

    fn patched(&mut self, session: &str, id: &str, hunks: &str) {
        if let Some(action) = self.taken.get_mut(&(session.to_owned(), id.to_owned())) {
            action
                .patch
                .get_or_insert_with(PatchCounts::default)
                .add(PatchCounts::of_hunks(hunks));
        }
    }

    fn finished(&mut self, session: &str, id: &str, ok: bool) {
        if let Some(action) = self.taken.get_mut(&(session.to_owned(), id.to_owned())) {
            action.ok = Some(ok);
        }
    }

    fn into_actions(mut self) -> Vec<DeepAction> {
        self.order
            .iter()
            .filter_map(|key| self.taken.remove(key))
            .collect()
    }
}

/// Who ran a session: its CLI, and the model it reported or was started with.
#[derive(Debug, Clone, Default)]
struct Ran {
    cli: String,
    model: Option<String>,
}

pub(crate) fn run_actions(events: &[RunEvent], run: &str, project: &Project) -> Vec<DeepAction> {
    let mut ran: HashMap<&SessionKey, Ran> = HashMap::new();
    let mut calls = Calls::default();
    for event in events {
        let Some(session) = event.session.as_ref() else {
            continue;
        };
        match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => {
                ran.insert(session, ran_by(&started.worker));
            }
            RunEventKind::Agent(AgentEvent::Init {
                model: Some(model), ..
            }) => {
                ran.entry(session).or_default().model = Some(model.clone());
            }
            RunEventKind::Agent(AgentEvent::ToolStarted {
                id, name, input, ..
            }) => {
                let who = ran.get(session).cloned().unwrap_or_else(|| Ran {
                    cli: event.source.clone(),
                    model: None,
                });
                let action = DeepAction {
                    key: ActionKey {
                        repo: project.repo.clone(),
                        run: run.to_owned(),
                        session: session.to_string(),
                        call: id.clone(),
                    },
                    tool: name.clone(),
                    input: input.clone(),
                    ok: None,
                    patch: None,
                    at: event.ts,
                    project: project.name.clone(),
                    cli: who.cli,
                    model: who.model,
                };
                calls.started(session.as_str(), id, action);
            }
            RunEventKind::Agent(AgentEvent::FilePatch { id, hunks, .. }) => {
                calls.patched(session.as_str(), id, hunks);
            }
            RunEventKind::Agent(AgentEvent::ToolFinished { id, ok, .. }) => {
                calls.finished(session.as_str(), id, *ok);
            }
            _ => {}
        }
    }
    calls.into_actions()
}

fn ran_by(worker: &ModelRef) -> Ran {
    Ran {
        cli: worker.agent.clone(),
        model: worker.model.clone(),
    }
}

/// The main agent's own calls in each conversation of a project, each dated by its turn.
fn conversation_actions(dir: &Path, project: &Project) -> Vec<DeepAction> {
    let mut actions = Vec::new();
    let mut journals: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    journals.sort();
    for path in journals {
        let Some(id) = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
        else {
            continue;
        };
        let text = fs::read_to_string(&path).unwrap_or_default();
        let times = TurnTimes::read(&path, &text);
        let events: Vec<ConversationEvent> = text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        actions.extend(turn_actions(&events, &id, project, &times));
    }
    actions
}

pub(crate) fn turn_actions(
    events: &[ConversationEvent],
    conversation: &str,
    project: &Project,
    times: &TurnTimes,
) -> Vec<DeepAction> {
    let mut calls = Calls::default();
    let mut ran: HashMap<String, Ran> = HashMap::new();
    let run = format!("{CONVERSATION}/{conversation}");
    for event in events {
        match event {
            ConversationEvent::TurnStarted { turn, worker, .. } => {
                ran.insert(turn.to_string(), ran_by(worker));
            }
            ConversationEvent::Agent { turn, event } => {
                let turn = turn.to_string();
                match event {
                    AgentEvent::ToolStarted {
                        id, name, input, ..
                    } => {
                        let who = ran.get(&turn).cloned().unwrap_or_default();
                        let action = DeepAction {
                            key: ActionKey {
                                repo: project.repo.clone(),
                                run: run.clone(),
                                session: turn.clone(),
                                call: id.clone(),
                            },
                            tool: name.clone(),
                            input: input.clone(),
                            ok: None,
                            patch: None,
                            at: times.at(&turn),
                            project: project.name.clone(),
                            cli: who.cli,
                            model: who.model,
                        };
                        calls.started(&turn, id, action);
                    }
                    AgentEvent::FilePatch { id, hunks, .. } => calls.patched(&turn, id, hunks),
                    AgentEvent::ToolFinished { id, ok, .. } => calls.finished(&turn, id, *ok),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    calls.into_actions()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(session: Option<&str>, kind: serde_json::Value) -> RunEvent {
        let mut line = json!({
            "seq": 0, "ts": "2026-10-17T07:17:01Z", "run_id": "run-01", "source": "codex",
            "kind": kind,
        });
        if let Some(session) = session {
            line["session"] = json!(session);
        }
        serde_json::from_value(line).unwrap()
    }

    #[test]
    fn a_runs_calls_come_with_their_model_result_and_patch() {
        let events = vec![
            event(
                Some("s-001"),
                json!({"type": "session", "data": {"event": "started", "worker": "codex/gpt-6-luna",
                    "agent_id": "codex", "provider": "openai", "kind": "cli", "role": "worker",
                    "step": "build", "access": "build", "lifetime": "task", "resumed_from": null,
                    "follow_key": 1}}),
            ),
            event(
                Some("s-001"),
                json!({"type": "agent", "data": {"type": "tool_started", "id": "c1",
                    "name": "command_execution", "input": {"command": "git log -5"}, "parent": null}}),
            ),
            event(
                Some("s-001"),
                json!({"type": "agent", "data": {"type": "tool_started", "id": "c2",
                    "name": "file_change", "input": {"path": "tests/test_x.py"}, "parent": null}}),
            ),
            event(
                Some("s-001"),
                json!({"type": "agent", "data": {"type": "file_patch", "id": "c2",
                    "path": "tests/test_x.py", "hunks": "@@ -1 +1,2 @@\n a\n+b\n", "parent": null}}),
            ),
            event(
                Some("s-001"),
                json!({"type": "agent", "data": {"type": "tool_finished", "id": "c1", "ok": false,
                    "output": null, "parent": null}}),
            ),
        ];
        let council = Project {
            repo: "council-3f9a1c2b4d5e".to_owned(),
            name: "council".to_owned(),
        };
        let actions = run_actions(&events, "run-01", &council);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].key.call, "c1");
        assert_eq!(actions[0].cli, "codex");
        assert_eq!(actions[0].model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(actions[0].ok, Some(false));
        assert_eq!(
            actions[1].patch,
            Some(PatchCounts {
                added: 1,
                removed: 0
            })
        );
        assert_eq!(actions[1].ok, None);
        assert_eq!(actions[1].project, "council");
    }

    #[test]
    fn a_projects_name_is_its_folder_without_the_hash() {
        assert_eq!(project_name("council-3f9a1c2b4d5e"), "council");
        assert_eq!(
            project_name("rate-limit-demo-3f9a1c2b4d5e"),
            "rate-limit-demo"
        );
        assert_eq!(project_name("plain"), "plain");
    }

    /// A conversation in `project` whose one call ran `command`.
    fn conversation_in(home: &Path, project: &str, command: &str) {
        let dir = home.join("state").join(project).join("conversation");
        fs::create_dir_all(&dir).unwrap();
        let lines = [
            json!({"conversation": "turn_started", "turn": "turn-001", "text": "hi",
                   "worker": "codex/gpt-6-luna", "read_only": true, "commit": "abc",
                   "uncommitted": []}),
            json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_started",
                   "id": "item_1", "name": "command_execution", "input": {"command": command},
                   "parent": null}}),
        ];
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        fs::write(dir.join("conversation-001.jsonl"), text).unwrap();
    }

    #[test]
    fn the_same_conversation_in_two_projects_is_two_actions() {
        let home = tempfile::tempdir().unwrap();
        conversation_in(home.path(), "alpha-3f9a1c2b4d5e", "git log -3");
        conversation_in(home.path(), "beta-4e8b2d3c5f6a", "pytest");
        let actions = recorded(&Paths::new(home.path()));
        assert_eq!(actions.len(), 2);
        assert_ne!(actions[0].key, actions[1].key);
        assert_eq!(actions[0].key.repo, "alpha-3f9a1c2b4d5e");
        assert_eq!(actions[1].key.repo, "beta-4e8b2d3c5f6a");
        assert_eq!(
            (actions[0].key.run.as_str(), actions[0].key.call.as_str()),
            ("conversation/conversation-001", "item_1")
        );
    }

    #[test]
    fn every_project_and_conversation_under_the_home_is_read() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let state = home.path().join("state").join("council-3f9a1c2b4d5e");
        let run = state.join("runs").join("run-01");
        fs::create_dir_all(&run).unwrap();
        let started = json!({"seq": 0, "ts": "2026-10-17T07:17:01Z", "run_id": "run-01",
            "source": "claude-code", "session": "s-001",
            "kind": {"type": "agent", "data": {"type": "tool_started", "id": "t1", "name": "Read",
                "input": {"file_path": "src/a.rs"}, "parent": null}}});
        fs::write(run.join("events.jsonl"), format!("{started}\n")).unwrap();
        let conversations = state.join("conversation");
        fs::create_dir_all(&conversations).unwrap();
        let lines = [
            json!({"conversation": "turn_started", "turn": "t-01", "text": "hi",
                   "worker": "claude-code/opus", "read_only": false, "commit": "abc",
                   "uncommitted": []}),
            json!({"conversation": "agent", "turn": "t-01", "event": {"type": "tool_started",
                   "id": "x1", "name": "Bash", "input": {"command": "cargo test"}, "parent": null}}),
        ];
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        fs::write(conversations.join("c-01.jsonl"), text).unwrap();
        let actions = recorded(&paths);
        assert_eq!(actions.len(), 2, "{actions:?}");
        assert_eq!(actions[0].key.run, "run-01");
        assert_eq!(actions[0].cli, "claude-code");
        assert_eq!(actions[1].key.run, "conversation/c-01");
        assert_eq!(actions[1].model.as_deref(), Some("opus"));
        assert_eq!(actions[1].project, "council");
    }
}
