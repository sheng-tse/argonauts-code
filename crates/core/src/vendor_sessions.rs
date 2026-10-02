//! The agent CLIs' own sessions for a folder, so `/resume` and the crew panel can list one
//! started outside Argonauts, and the chat can carry it on through the CLI's own resume.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::names::VendorId;
use crate::session::SessionRequest;

/// Where an adapter looks for what its CLI keeps on disk. Tests point `home` at a fixture, never
/// at the real home folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorScan {
    /// The repo or folder Argonauts runs in.
    pub folder: PathBuf,
    /// The home folder each CLI keeps its store under, such as `~/.claude` or `~/.codex`.
    pub home: PathBuf,
}

impl VendorScan {
    pub fn new(folder: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
            home: home.into(),
        }
    }
}

/// One of a CLI's own sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorSession {
    /// Adapter id, such as `claude-code`.
    pub vendor: VendorId,
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
}

impl VendorSession {
    pub fn reference(&self) -> VendorSessionRef {
        VendorSessionRef {
            vendor: self.vendor.clone(),
            id: self.id.clone(),
        }
    }
}

/// Names one CLI session. Written `<adapter>:<session id>`, as in `/resume codex:019a2f`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VendorSessionRef {
    pub vendor: VendorId,
    pub id: String,
}

impl VendorSessionRef {
    /// `None` unless both halves are plain ids. The id goes on the CLI's command line, so one
    /// starting with `-` would read as a flag.
    pub fn parse(text: &str) -> Option<Self> {
        let (vendor, id) = text.split_once(':')?;
        let vendor_ok = vendor.starts_with(|c: char| c.is_ascii_lowercase())
            && vendor
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        (vendor_ok && is_session_id(id)).then(|| Self {
            vendor: VendorId(vendor.to_owned()),
            id: id.to_owned(),
        })
    }
}

impl fmt::Display for VendorSessionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.vendor, self.id)
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

/// One turn of a CLI session's transcript, reprinted when it's resumed. Tool calls and their
/// output are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorTurn {
    pub from: Speaker,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    User,
    Agent,
}

/// How the chat's next turn carries on a CLI session: the CLI's own resume, such as `claude -p
/// --resume <id>` or `codex exec resume <id>`, in the folder the session ran in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeLaunch {
    /// The CLI's session id, for [`SessionRequest::resume_session`].
    pub session: String,
    pub workdir: PathBuf,
    /// The session's model, kept unless another is picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl ResumeLaunch {
    pub fn of(session: &VendorSession) -> Self {
        Self {
            session: session.id.clone(),
            workdir: session.folder.clone(),
            model: session.model.clone(),
        }
    }

    /// Points `request` at the session. A model already picked stays.
    pub fn apply(&self, request: &mut SessionRequest) {
        request.resume_session = Some(self.session.clone());
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

    fn session() -> VendorSession {
        VendorSession {
            vendor: VendorId("codex".into()),
            id: "019a2f-4c".into(),
            title: "Explain the input handling".into(),
            prompt: Some("Explain the input handling\nin detail".into()),
            folder: PathBuf::from("/repo"),
            updated: OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap(),
            model: Some("gpt-6.1-sol".into()),
            branch: Some("main".into()),
            last_message: Some("It reads keys in one loop.".into()),
        }
    }

    #[test]
    fn sessions_and_turns_round_trip() {
        let value = json!({
            "vendor": "codex",
            "id": "019a2f-4c",
            "title": "Explain the input handling",
            "prompt": "Explain the input handling\nin detail",
            "folder": "/repo",
            "updated": "2026-09-21T14:13:20Z",
            "model": "gpt-6.1-sol",
            "branch": "main",
            "last_message": "It reads keys in one loop."
        });
        let parsed: VendorSession = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(parsed, session());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);

        let bare = json!({"vendor": "claude-code", "id": "a1", "title": "Hi", "folder": "/r",
            "updated": "2026-09-21T14:13:20Z"});
        let parsed: VendorSession = serde_json::from_value(bare.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), bare);

        let turn = json!({"from": "agent", "text": "Done."});
        let parsed: VendorTurn = serde_json::from_value(turn.clone()).unwrap();
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
        let parsed = VendorSessionRef::parse("claude-code:0a1b-2c3d").unwrap();
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
            assert_eq!(VendorSessionRef::parse(bad), None, "{bad}");
        }
        assert!(!is_session_id(&"a".repeat(129)));
    }

    fn session_ref(vendor: &str, id: &str) -> VendorSessionRef {
        VendorSessionRef {
            vendor: VendorId(vendor.into()),
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
        assert_eq!(request.workdir, PathBuf::from("/repo"));
        assert_eq!(request.model.as_deref(), Some("gpt-6.1-sol"));

        let mut picked = SessionRequest {
            model: Some("gpt-6-astra".into()),
            ..SessionRequest::default()
        };
        launch.apply(&mut picked);
        assert_eq!(picked.model.as_deref(), Some("gpt-6-astra"));
    }
}
