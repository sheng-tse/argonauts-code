//! Hodoscope's trajectory JSON (github.com/AR-FORUM/hodoscope): one file per agent session,
//! `{id, messages, metadata}`, with each action as one of the assistant's tool calls and its
//! end as the tool's reply. Hodoscope reads a folder of these and groups by any metadata
//! field, the model by default.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use argonauts_core::display_names::DisplayNames;
use serde_json::{Map, Value, json};

use super::actions::{Action, Outcome, SessionEnded};
use super::report;

/// Each session's trajectory, in the order its first action was taken.
pub(crate) fn trajectories(actions: &[Action], names: &DisplayNames) -> Vec<(String, Value)> {
    let mut sessions: BTreeMap<(&str, &str, String), Vec<&Action>> = BTreeMap::new();
    let mut order = Vec::new();
    for action in actions {
        let key = (
            action.repo.as_str(),
            action.run.as_str(),
            session_of(action),
        );
        let session = sessions.entry(key.clone()).or_default();
        if session.is_empty() {
            order.push(key);
        }
        session.push(action);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let session = sessions.remove(&key)?;
            let first = session.first()?;
            let id = format!("{}/{}/{}", first.project(), first.run, key.2);
            let file = format!("{}--{}--{}.json", key.0, key.1, key.2);
            Some((file, trajectory(id, &session, names)))
        })
        .collect()
}

/// The agent session an action was part of. The main agent goes on through every turn of a
/// conversation in one session of its CLI, so the turns are one trajectory, and a turn its
/// fallback took is another's.
fn session_of(action: &Action) -> String {
    if action.conversation {
        format!("main-{}", action.cli)
    } else {
        action.session.clone()
    }
}

fn trajectory(id: String, session: &[&Action], names: &DisplayNames) -> Value {
    let first = session[0];
    let mut messages = vec![json!({"role": "user", "content": first.agent})];
    for action in session {
        let arguments = match &action.input {
            Value::Object(_) => action.input.clone(),
            Value::Null => Value::Object(Map::new()),
            other => json!({ "input": other }),
        };
        messages.push(json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": action.call,
                "type": "function",
                "function": action.tool,
                "arguments": arguments,
            }],
        }));
        let reply = match action.ok {
            Some(true) => "ok",
            Some(false) => "failed",
            None => "",
        };
        messages.push(json!({
            "role": "tool",
            "tool_call_id": action.call,
            "content": reply,
        }));
    }
    let outcome = session.iter().rev().find_map(|action| action.outcome);
    let ended = session.iter().find_map(|action| action.ended);
    let model = names
        .model(&first.cli, first.model.as_deref())
        .unwrap_or_else(|| names.cli(&first.cli));
    json!({
        "id": id,
        "messages": messages,
        "metadata": {
            "model": model,
            "model_id": first.model,
            "cli": names.cli(&first.cli),
            "role": report::role_name(first.role),
            "agent": first.agent,
            "workflow": first.workflow,
            "task": first.task,
            "outcome": outcome.map(Outcome::name),
            "ended": ended.map(SessionEnded::name),
            "project": first.project(),
            "run": first.run,
        },
    })
}

/// Writes each session's trajectory into `dir`, which is made if missing. Returns how many.
pub(crate) fn write(dir: &Path, actions: &[Action], names: &DisplayNames) -> io::Result<usize> {
    fs::create_dir_all(dir)?;
    let trajectories = trajectories(actions, names);
    for (file, trajectory) in &trajectories {
        let text = serde_json::to_string_pretty(trajectory).map_err(io::Error::other)?;
        fs::write(dir.join(file), text + "\n")?;
    }
    Ok(trajectories.len())
}

#[cfg(test)]
mod tests {
    use argonauts_core::behaviors::{Behavior, Label};
    use argonauts_core::crew_stats::StatsRole;
    use time::macros::datetime;

    use super::*;

    fn action(session: &str, call: &str, tool: &str, input: Value, ok: Option<bool>) -> Action {
        Action {
            repo: "council-0123456789ab".to_owned(),
            run: "run-20261020-123036-4e50".to_owned(),
            session: session.to_owned(),
            call: call.to_owned(),
            agent: "build:per-api-key".to_owned(),
            cli: "codex".to_owned(),
            model: Some("gpt-6-luna".to_owned()),
            role: StatsRole::Worker,
            workflow: Some("council".to_owned()),
            task: Some("task-per-api-key".to_owned()),
            outcome: Some(Outcome::SentBack),
            at: datetime!(2026-10-20 12:30 UTC),
            tool: tool.to_owned(),
            what: String::new(),
            ok,
            behavior: Behavior::One(Label::GitHistory),
            ended: Some(SessionEnded::Failed),
            ended_at: Some(datetime!(2026-10-20 12:31 UTC)),
            settled_at: None,
            input,
            conversation: false,
            note: None,
        }
    }

    fn names() -> DisplayNames {
        DisplayNames {
            catalog: argonauts_registry::models::shipped_catalog().clone(),
            ..DisplayNames::default()
        }
    }

    #[test]
    fn one_trajectory_per_session_with_each_action_as_a_tool_call() {
        let actions = [
            action(
                "s-004",
                "item_1",
                "command_execution",
                json!({"command": "git show ef3f187"}),
                Some(true),
            ),
            action(
                "s-005",
                "item_1",
                "structured_output",
                json!({"summary": "done"}),
                None,
            ),
            action(
                "s-004",
                "item_2",
                "file_change",
                json!({"changes": []}),
                Some(false),
            ),
        ];
        let written = trajectories(&actions, &names());
        assert_eq!(written.len(), 2);
        let (file, first) = &written[0];
        assert_eq!(
            file,
            "council-0123456789ab--run-20261020-123036-4e50--s-004.json"
        );
        assert_eq!(
            first,
            &json!({
                "id": "council/run-20261020-123036-4e50/s-004",
                "messages": [
                    {"role": "user", "content": "build:per-api-key"},
                    {"role": "assistant", "content": "", "tool_calls": [{
                        "id": "item_1", "type": "function", "function": "command_execution",
                        "arguments": {"command": "git show ef3f187"},
                    }]},
                    {"role": "tool", "tool_call_id": "item_1", "content": "ok"},
                    {"role": "assistant", "content": "", "tool_calls": [{
                        "id": "item_2", "type": "function", "function": "file_change",
                        "arguments": {"changes": []},
                    }]},
                    {"role": "tool", "tool_call_id": "item_2", "content": "failed"},
                ],
                "metadata": {
                    "model": "GPT 6 Luna",
                    "model_id": "gpt-6-luna",
                    "cli": "Codex",
                    "role": "worker",
                    "agent": "build:per-api-key",
                    "workflow": "council",
                    "task": "task-per-api-key",
                    "outcome": "sent_back",
                    "ended": "failed",
                    "project": "council",
                    "run": "run-20261020-123036-4e50",
                },
            })
        );
        assert_eq!(written[1].1["messages"][2]["content"], "");
    }

    #[test]
    fn a_conversations_turns_are_one_trajectory_of_its_main_agent() {
        let turn = |session: &str, call: &str| Action {
            run: "conversation-001".to_owned(),
            agent: "main agent".to_owned(),
            role: StatsRole::Main,
            workflow: None,
            task: None,
            outcome: None,
            ended: None,
            ended_at: None,
            settled_at: None,
            conversation: true,
            ..action(
                session,
                call,
                "Read",
                json!({"file_path": "a.rs"}),
                Some(true),
            )
        };
        let actions = [turn("t-001", "item_1"), turn("t-002", "item_1")];
        let written = trajectories(&actions, &names());
        assert_eq!(written.len(), 1, "{written:?}");
        assert_eq!(
            written[0].0,
            "council-0123456789ab--conversation-001--main-codex.json"
        );
        let messages = written[0].1["messages"].as_array().unwrap();
        assert_eq!(
            messages.len(),
            5,
            "the user's line, then both calls and replies"
        );
        assert_eq!(written[0].1["metadata"]["ended"], Value::Null);
    }

    #[test]
    fn the_files_are_written_into_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("hodoscope");
        let actions = [action("s-004", "item_1", "Read", Value::Null, Some(true))];
        assert_eq!(write(&out, &actions, &names()).unwrap(), 1);
        let file = out.join("council-0123456789ab--run-20261020-123036-4e50--s-004.json");
        let read: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(read["messages"][1]["tool_calls"][0]["arguments"], json!({}));
    }
}
