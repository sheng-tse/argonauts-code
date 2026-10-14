//! The agent CLIs' own sessions for a folder, so `/resume` and the agent panel can list one
//! started outside Argonauts Code, and the conversation can carry it on through the CLI's own
//! resume.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::names::AgentId;
use crate::session::SessionRequest;

/// Where an adapter looks for what its CLI keeps on disk. Tests point `home` at a fixture, never
/// at the real home folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliScan {
    /// The repo or folder Argonauts Code runs in.
    pub folder: PathBuf,
    /// The home folder each CLI keeps its store under, such as `~/.claude` or `~/.codex`.
    pub home: PathBuf,
    /// Lists the sessions of every folder, each with the folder it ran in, not `folder`'s alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub every_folder: bool,
}

impl CliScan {
    pub fn new(folder: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
            home: home.into(),
            every_folder: false,
        }
    }

    /// The same scan over every folder's sessions.
    pub fn everywhere(mut self) -> Self {
        self.every_folder = true;
        self
    }

    /// Whether a session that ran in `cwd` belongs in the list. One that doesn't say where it
    /// ran is taken to be this folder's, since that's where its file was found.
    pub fn lists(&self, cwd: Option<&std::path::Path>) -> bool {
        self.every_folder || cwd.is_none_or(|cwd| cwd == self.folder)
    }
}

/// One of a CLI's own sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliSession {
    /// Adapter id, such as `claude-code`.
    pub agent_id: AgentId,
    /// The CLI's own session id, which its resume takes.
    pub id: String,
    /// The CLI's title for it, or else its first prompt, on one line.
    pub title: String,
    /// The first prompt in full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Where it ran, and so where it carries on.
    pub folder: PathBuf,
    /// When it was last used.
    #[serde(with = "time::serde::rfc3339")]
    pub updated: OffsetDateTime,
    /// The model of its last turn, as the CLI names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// What it said last, on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    /// How many prompts it took, which the list shows as its length.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub turns: u32,
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

impl CliSession {
    pub fn reference(&self) -> CliSessionRef {
        CliSessionRef {
            agent_id: self.agent_id.clone(),
            id: self.id.clone(),
        }
    }
}

/// Names one CLI session. Written `<adapter>:<session id>`, as in `/resume codex:019a2f`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CliSessionRef {
    pub agent_id: AgentId,
    pub id: String,
}

impl CliSessionRef {
    /// `None` unless both halves are plain ids. The id goes on the CLI's command line, so one
    /// starting with `-` would read as a flag.
    pub fn parse(text: &str) -> Option<Self> {
        let (agent_id, id) = text.split_once(':')?;
        let agent_ok = agent_id.starts_with(|c: char| c.is_ascii_lowercase())
            && agent_id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        (agent_ok && is_session_id(id)).then(|| Self {
            agent_id: AgentId(agent_id.to_owned()),
            id: id.to_owned(),
        })
    }
}

impl fmt::Display for CliSessionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.agent_id, self.id)
    }
}

/// Whether `id` is safe to hand a CLI as a session id.
pub fn is_session_id(id: &str) -> bool {
    id.len() <= 128
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// One turn of a CLI session's transcript, reprinted when it's resumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliTurn {
    pub from: Speaker,
    /// A person's words, or everything the agent wrote in the turn.
    pub text: String,
    /// An agent's turn as it happened: what it wrote, thought and ran, in order. A person's turn
    /// has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<CliStep>,
}

impl CliTurn {
    /// A person's words.
    pub fn prompt(text: impl Into<String>) -> Self {
        Self {
            from: Speaker::User,
            text: text.into(),
            steps: Vec::new(),
        }
    }

    /// An agent's turn with nothing in it yet.
    pub fn of_the_agent() -> Self {
        Self {
            from: Speaker::Agent,
            text: String::new(),
            steps: Vec::new(),
        }
    }

    /// More of what the agent wrote: a paragraph of `text`, and a step where it happened.
    pub fn wrote(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.text.push_str("\n\n");
        }
        self.text.push_str(text);
        self.steps.push(CliStep::Wrote {
            text: text.to_owned(),
        });
    }

    /// The call with `id`, to give its result or its patch.
    pub fn call_mut(&mut self, id: &str) -> Option<&mut CliCall> {
        self.steps.iter_mut().rev().find_map(|step| match step {
            CliStep::Ran(call) if call.id == id => Some(call),
            _ => None,
        })
    }
}

/// One thing an agent did in a turn of its transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum CliStep {
    Wrote {
        text: String,
    },
    /// Empty when the CLI kept only that it thought, not the words.
    Thought {
        text: String,
    },
    Ran(CliCall),
}

/// A tool call as a transcript records it, named and given as the CLI's live stream names and
/// gives it, so it draws the same row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    /// `None` when the transcript ends before the call does.
    pub ok: Option<bool>,
    pub output: Option<String>,
    /// The edits it made, each file's as unified hunks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<CliPatch>,
    /// What the subagent it started did, in order, where the CLI kept that.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<CliStep>,
}

impl CliCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, input: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            input,
            ok: None,
            output: None,
            patches: Vec::new(),
            steps: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliPatch {
    pub path: String,
    pub hunks: String,
    /// Whether the hunks' headers hold the file's own line numbers. A patch that names no
    /// lines, as Codex's do, draws without them.
    pub numbered: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    User,
    Agent,
}

/// How the conversation's next turn carries on a CLI session: the CLI's own resume, such as `claude
/// -p --resume <id>` or `codex exec resume <id>`, in the folder the session ran in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeLaunch {
    /// The CLI's session id, for [`SessionRequest::resume_session`].
    pub session: String,
    pub workdir: PathBuf,
    /// The session's model, kept unless another is picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Carry on a copy of the session, made by the CLI's own fork, so the session itself stays
    /// as it was.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fork: bool,
}

impl ResumeLaunch {
    pub fn of(session: &CliSession) -> Self {
        Self {
            session: session.id.clone(),
            workdir: session.folder.clone(),
            model: session.model.clone(),
            fork: false,
        }
    }

    /// The same launch on a copy of the session.
    pub fn forked(self) -> Self {
        Self { fork: true, ..self }
    }

    /// Points `request` at the session. A model already picked stays.
    pub fn apply(&self, request: &mut SessionRequest) {
        request.resume_session = Some(self.session.clone());
        request.fork_session = self.fork;
        request.workdir = self.workdir.clone();
        if request.model.is_none() {
            request.model = self.model.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn session() -> CliSession {
        CliSession {
            agent_id: AgentId("codex".into()),
            id: "019a2f-4c".into(),
            title: "Explain the input handling".into(),
            prompt: Some("Explain the input handling\nin detail".into()),
            folder: PathBuf::from("/repo"),
            updated: OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap(),
            model: Some("gpt-6.1-sol".into()),
            branch: Some("main".into()),
            last_message: Some("It reads keys in one loop.".into()),
            turns: 3,
        }
    }

    #[test]
    fn a_scan_lists_its_own_folder_or_every_folder() {
        let scan = CliScan::new("/repo", "/home");
        assert!(scan.lists(Some(std::path::Path::new("/repo"))));
        assert!(scan.lists(None));
        assert!(!scan.lists(Some(std::path::Path::new("/repo/sub"))));
        assert!(
            scan.everywhere()
                .lists(Some(std::path::Path::new("/other")))
        );
    }

    #[test]
    fn sessions_and_turns_round_trip() {
        let value = json!({
            "agent_id": "codex",
            "id": "019a2f-4c",
            "title": "Explain the input handling",
            "prompt": "Explain the input handling\nin detail",
            "folder": "/repo",
            "updated": "2026-09-21T14:13:20Z",
            "model": "gpt-6.1-sol",
            "branch": "main",
            "last_message": "It reads keys in one loop.",
            "turns": 3
        });
        let parsed: CliSession = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(parsed, session());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);

        let bare = json!({"agent_id": "claude-code", "id": "a1", "title": "Hi", "folder": "/r",
            "updated": "2026-09-21T14:13:20Z"});
        let parsed: CliSession = serde_json::from_value(bare.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), bare);

        let turn = json!({"from": "agent", "text": "Done."});
        let parsed: CliTurn = serde_json::from_value(turn.clone()).unwrap();
        assert_eq!(parsed.from, Speaker::Agent);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), turn);

        let launch = ResumeLaunch::of(&session());
        let value = serde_json::to_value(&launch).unwrap();
        assert_eq!(
            value,
            json!({"session": "019a2f-4c", "workdir": "/repo", "model": "gpt-6.1-sol"})
        );
        assert_eq!(
            serde_json::from_value::<ResumeLaunch>(value).unwrap(),
            launch
        );
    }

    #[test]
    fn references_parse_only_plain_ids() {
        let parsed = CliSessionRef::parse("claude-code:0a1b-2c3d").unwrap();
        assert_eq!(parsed, session_ref("claude-code", "0a1b-2c3d"));
        assert_eq!(parsed.to_string(), "claude-code:0a1b-2c3d");
        assert_eq!(session().reference(), session_ref("codex", "019a2f-4c"));
        for bad in [
            "run-20261001-101010-ab12",
            "codex:",
            ":abc",
            "codex:-p",
            "codex:a b",
            "Codex:abc",
            "codex:a;rm",
        ] {
            assert_eq!(CliSessionRef::parse(bad), None, "{bad}");
        }
        assert!(!is_session_id(&"a".repeat(129)));
    }

    fn session_ref(agent_id: &str, id: &str) -> CliSessionRef {
        CliSessionRef {
            agent_id: AgentId(agent_id.into()),
            id: id.into(),
        }
    }

    #[test]
    fn a_launch_points_the_request_at_the_session_and_keeps_a_picked_model() {
        let launch = ResumeLaunch::of(&session());
        let mut request = SessionRequest {
            workdir: PathBuf::from("/elsewhere"),
            ..SessionRequest::default()
        };
        launch.apply(&mut request);
        assert_eq!(request.resume_session.as_deref(), Some("019a2f-4c"));
        assert!(!request.fork_session);
        assert_eq!(request.workdir, PathBuf::from("/repo"));
        assert_eq!(request.model.as_deref(), Some("gpt-6.1-sol"));

        let mut picked = SessionRequest {
            model: Some("gpt-6-astra".into()),
            ..SessionRequest::default()
        };
        launch.apply(&mut picked);
        assert_eq!(picked.model.as_deref(), Some("gpt-6-astra"));
    }

    #[test]
    fn a_forked_launch_asks_for_a_copy_of_the_session() {
        let launch = ResumeLaunch::of(&session()).forked();
        let mut request = SessionRequest::default();
        launch.apply(&mut request);
        assert_eq!(request.resume_session.as_deref(), Some("019a2f-4c"));
        assert!(request.fork_session);
        assert_eq!(
            serde_json::to_value(&launch).unwrap()["fork"],
            json!(true),
            "a launch kept for later still forks"
        );
    }
}
