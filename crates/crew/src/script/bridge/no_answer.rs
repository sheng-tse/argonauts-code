//! Why a call's session ended well yet gave no answer, in the words its row shows: "ended
//! without an answer", and what it did instead when the run's journal says, such as trying to
//! edit files in a session that could only read them.

use argonauts_core::event::AgentEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::run_files;
use argonauts_core::names::SessionKey;
use argonauts_core::session_runner::Access;

use crate::engine::host::Host;

/// How the reason starts. What follows it, after a colon, is what the agent did instead.
pub(super) const ENDED_WITHOUT_AN_ANSWER: &str = "ended without an answer";

/// Tools that change files, as each CLI names them: Claude Code's, Codex's patch, OpenCode's,
/// and agy's.
const EDIT_TOOLS: &[&str] = &[
    "edit",
    "write",
    "multiedit",
    "notebookedit",
    "apply_patch",
    "patch",
    "edit_file",
    "write_file",
    "replace",
    "file_change",
];

pub(super) fn why(host: &Host, session: &SessionKey) -> String {
    match did_instead(host, session) {
        Some(instead) => format!("{ENDED_WITHOUT_AN_ANSWER}: {instead}"),
        None => ENDED_WITHOUT_AN_ANSWER.to_owned(),
    }
}

/// What the session did in place of answering, when that explains the missing answer.
fn did_instead(host: &Host, session: &SessionKey) -> Option<&'static str> {
    let access = host.read(|state| {
        state
            .sessions
            .by_key
            .get(session)
            .map(|record| record.started.access)
    })?;
    if !matches!(access, Access::ReadOnly | Access::NoTools) {
        return None;
    }
    tried_to_edit(host, session).then_some("it tried to edit files in a read-only session")
}

/// Whether the session called a tool that changes files. Read from the journal only when an
/// answer is missing, which is rare, so the run keeps no count of its own.
fn tried_to_edit(host: &Host, session: &SessionKey) -> bool {
    let path = host.repo().run_dir.join(run_files::EVENTS);
    argonauts_core::journal::read_all(&path)
        .is_ok_and(|back| called_an_edit_tool(&back.events, session))
}

fn called_an_edit_tool(events: &[RunEvent], session: &SessionKey) -> bool {
    events.iter().any(|event| {
        event.session.as_ref() == Some(session)
            && matches!(
                &event.kind,
                RunEventKind::Agent(AgentEvent::ToolStarted { name, .. }) if edits(name)
            )
    })
}

fn edits(tool: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    EDIT_TOOLS.contains(&tool.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One line of a run's journal, as OpenCode's read-only challenger wrote it.
    fn tool(session: &str, name: &str) -> RunEvent {
        serde_json::from_value(serde_json::json!({
            "seq": 911,
            "ts": "2026-10-20T12:25:01Z",
            "run_id": "run-20261020-122145-c25f",
            "session": session,
            "source": "opencode",
            "kind": {"type": "agent", "data": {
                "type": "tool_started", "id": "call_1", "name": name,
                "input": {"filePath": "ratelimit/bucket.py"}, "parent": null,
            }},
        }))
        .expect("a journal line")
    }

    #[test]
    fn an_edit_tool_the_session_called_is_found_in_the_journal() {
        let session = SessionKey::numbered(9);
        let read = [tool("s-009", "read"), tool("s-003", "edit")];
        assert!(
            !called_an_edit_tool(&read, &session),
            "another session's edit"
        );
        let edited = [tool("s-009", "read"), tool("s-009", "edit")];
        assert!(called_an_edit_tool(&edited, &session));
    }

    #[test]
    fn each_clis_edit_tools_count_and_reading_ones_dont() {
        for tool in [
            "Edit",
            "Write",
            "MultiEdit",
            "apply_patch",
            "edit",
            "write_file",
        ] {
            assert!(edits(tool), "{tool}");
        }
        for tool in ["Read", "read", "Bash", "grep", "glob", "web_fetch"] {
            assert!(!edits(tool), "{tool}");
        }
    }
}
