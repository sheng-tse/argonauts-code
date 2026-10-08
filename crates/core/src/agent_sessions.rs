//! The sessions a run's agents ran in. Every agent Argonauts starts is a real session of its CLI,
//! so `/resume` lists each under its run, and one that finished carries on through its CLI's own
//! resume as a direct conversation, in the folder it ran in. Helpers a CLI starts inside its
//! session (subagents, its workflows, Codex threads) are listed under it, and come back when the
//! session does, never on their own.

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::RunId;
use crate::events::session::{SubagentKind, SubagentStatus};
use crate::names::{RoleName, SessionKey, WorkerRef};

/// Names one agent session of a run. Written `<run>/<session>`, as in `/resume run-03/s-002`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AgentSessionRef {
    pub run: RunId,
    pub session: SessionKey,
}

impl AgentSessionRef {
    /// `None` unless both halves are plain ids, so neither reads as a flag.
    pub fn parse(text: &str) -> Option<Self> {
        let (run, session) = text.split_once('/')?;
        if !session.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return None;
        }
        Some(Self {
            run: RunId::new(run).ok()?,
            session: SessionKey::parse(session)?,
        })
    }
}

impl fmt::Display for AgentSessionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.run, self.session)
    }
}

/// One agent's session in a run, as `/resume` lists it under the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSession {
    pub run: RunId,
    pub session: SessionKey,
    pub role: RoleName,
    /// Its name in the run, such as `build:slugify`. A run from before agents had names gives
    /// what it worked on: its card, or its step when it had none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub worker: WorkerRef,
    /// What it said last, on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated: OffsetDateTime,
    pub resume: Resume,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub helpers: Vec<Helper>,
}

impl AgentSession {
    pub fn reference(&self) -> AgentSessionRef {
        AgentSessionRef {
            run: self.run.clone(),
            session: self.session.clone(),
        }
    }

    /// The helpers its CLI started, counted by kind, such as `2 subagents · 1 workflow`. Empty
    /// when it started none.
    pub fn helper_counts(&self) -> String {
        let mut counts: Vec<(SubagentKind, usize)> = Vec::new();
        for helper in &self.helpers {
            match counts.iter_mut().find(|(kind, _)| *kind == helper.kind) {
                Some((_, count)) => *count += 1,
                None => counts.push((helper.kind, 1)),
            }
        }
        counts
            .into_iter()
            .map(|(kind, count)| {
                let noun = match kind {
                    SubagentKind::Subagent => "subagent",
                    SubagentKind::Workflow => "workflow",
                    SubagentKind::Thread => "thread",
                };
                if count == 1 {
                    format!("1 {noun}")
                } else {
                    format!("{count} {noun}s")
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Whether a session carries on through its CLI's own resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resume {
    /// Its run ended, and its CLI can pick it up again.
    Ready,
    /// It's still running, so taking it over is how to work in it.
    Working,
    /// It finished, but its run hasn't ended, and the run's next round on its card resumes the
    /// same CLI session. So the run still owns it: taking it over works in it, and it carries on
    /// here once the run ends.
    RunOwns,
    /// Its transcript is offered instead, and `why` says so in a few words.
    Cannot { why: String },
}

/// A helper its CLI started inside the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Helper {
    /// The CLI's own id for it, such as a tool call id or a Codex thread id.
    pub id: String,
    pub kind: SubagentKind,
    /// Its name or type, such as `explorer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub status: SubagentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_reference_reads_run_slash_session() {
        let reference = AgentSessionRef::parse("run-03/s-002").unwrap();
        assert_eq!(reference.run.as_str(), "run-03");
        assert_eq!(reference.session.as_str(), "s-002");
        assert_eq!(reference.to_string(), "run-03/s-002");
        for bad in [
            "run-03",
            "run-03/",
            "/s-002",
            "run-03/s 2",
            "run-03/-p",
            "-p/s-002",
        ] {
            assert!(AgentSessionRef::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn helpers_are_counted_by_kind() {
        let helper = |id: &str, kind: SubagentKind| Helper {
            id: id.into(),
            kind,
            label: None,
            status: SubagentStatus::Done,
            last_message: None,
        };
        let mut session: AgentSession = serde_json::from_value(json!({
            "run": "run-03", "session": "s-002", "role": "worker", "worker": "claude-code",
            "updated": "2026-09-21T14:13:20Z", "resume": {"kind": "run_owns"}
        }))
        .unwrap();
        assert_eq!(session.resume, Resume::RunOwns);
        assert_eq!(session.helper_counts(), "");
        session.helpers = vec![
            helper("toolu_01", SubagentKind::Subagent),
            helper("wf_01", SubagentKind::Workflow),
            helper("toolu_02", SubagentKind::Subagent),
        ];
        assert_eq!(session.helper_counts(), "2 subagents · 1 workflow");
    }

    #[test]
    fn a_session_round_trips() {
        let value = json!({
            "run": "run-03",
            "session": "s-002",
            "role": "worker",
            "label": "task-001",
            "worker": "claude-code/opus",
            "last_message": "Fixed both functions.",
            "updated": "2026-09-21T14:13:20Z",
            "resume": {"kind": "cannot", "why": "Codex can't resume"},
            "helpers": [{"id": "toolu_01", "kind": "subagent", "label": "explorer",
                "status": "done"}]
        });
        let parsed: AgentSession = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(parsed.reference().to_string(), "run-03/s-002");
        assert_eq!(
            parsed.resume,
            Resume::Cannot {
                why: "Codex can't resume".into()
            }
        );
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    }
}
