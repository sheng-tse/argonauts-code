//! A chat's files: `chat/<chat>.jsonl` holds its `ChatEvent`s, `chat/<chat>.json` the
//! vendor session to resume, so `/resume` can carry the conversation on in a later app
//! session, and `chat/<chat>.title` the name `/rename` gave it. Like a run's files, they are
//! the owner's alone and have secrets redacted, since the chat reads your checkout.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::app::ChatEvent;
use argonauts_core::names::{ChatId, WorkerRef};
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::redact::redact_json;
use serde::{Deserialize, Serialize};

/// How an agent event's line starts, `{"chat":"agent",...}`, so reading turns back skips it
/// unparsed.
const AGENT_TAG: &str = r#"{"chat":"agent""#;

/// `chat/<chat>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatSession {
    pub worker: WorkerRef,
    pub vendor_session: String,
}

pub(crate) struct ChatStore {
    dir: PathBuf,
    journal: PathBuf,
    session: PathBuf,
    title: PathBuf,
}

impl ChatStore {
    pub(crate) fn open(chat_dir: &Path, chat: &ChatId) -> Self {
        Self {
            dir: chat_dir.to_owned(),
            journal: chat_dir.join(format!("{chat}.jsonl")),
            session: chat_dir.join(format!("{chat}.json")),
            title: chat_dir.join(format!("{chat}.title")),
        }
    }

    /// The number after the highest chat in `chat_dir`, so a new app session never reuses an
    /// older chat's files.
    pub(crate) fn next_id(chat_dir: &Path) -> ChatId {
        let highest = fs::read_dir(chat_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let stem = Path::new(&name).file_stem()?.to_str()?.to_owned();
                stem.strip_prefix("chat-")?.parse::<u32>().ok()
            })
            .max()
            .unwrap_or(0);
        ChatId::numbered(highest.saturating_add(1))
    }

    pub(crate) fn journal_path(&self) -> &Path {
        &self.journal
    }

    pub(crate) fn append(&self, event: &ChatEvent) -> anyhow::Result<()> {
        let mut value = serde_json::to_value(event).context("encoding a chat event")?;
        redact_json(&mut value);
        let mut line = serde_json::to_vec(&value).context("encoding a chat event")?;
        line.push(b'\n');
        self.make_dir()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.journal)
            .with_context(|| format!("opening {}", self.journal.display()))?;
        file.write_all(&line)
            .with_context(|| format!("writing {}", self.journal.display()))
    }

    /// Every event recorded so far, oldest first.
    #[cfg(test)]
    pub(crate) fn events(&self) -> anyhow::Result<Vec<ChatEvent>> {
        let text = match fs::read_to_string(&self.journal) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", self.journal.display()));
            }
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .with_context(|| format!("a bad line in {}", self.journal.display()))
            })
            .collect()
    }

    /// The turns' own events, oldest first, leaving out what the agents did along the way, which
    /// makes up most of a journal. `None` when there's no journal. A line that doesn't parse,
    /// such as a torn last line, is skipped.
    pub(crate) fn read_events(&self) -> anyhow::Result<Option<Vec<ChatEvent>>> {
        let text = match fs::read_to_string(&self.journal) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", self.journal.display()));
            }
        };
        let events = text
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with(AGENT_TAG))
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Ok(Some(events))
    }

    /// `None` when there's none yet, or the file can't be read, which only means the next turn
    /// starts a fresh session.
    pub(crate) fn session(&self) -> Option<ChatSession> {
        let text = fs::read_to_string(&self.session).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub(crate) fn save_session(&self, session: &ChatSession) -> anyhow::Result<()> {
        let text = serde_json::to_vec_pretty(session).context("encoding the chat session")?;
        self.make_dir()?;
        // Written aside and renamed, so a crash never leaves half a file.
        let partial = self.session.with_extension("json.partial");
        write_private_file(&partial, &text)
            .with_context(|| format!("writing {}", partial.display()))?;
        fs::rename(&partial, &self.session)
            .with_context(|| format!("writing {}", self.session.display()))
    }

    fn make_dir(&self) -> anyhow::Result<()> {
        create_private_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))
    }

    /// The name `/rename` gave the conversation, if it has one.
    pub(crate) fn title(&self) -> Option<String> {
        let text = fs::read_to_string(&self.title).ok()?;
        let title = text.trim();
        (!title.is_empty()).then(|| title.to_owned())
    }

    pub(crate) fn save_title(&self, title: &str) -> anyhow::Result<()> {
        self.make_dir()?;
        write_private_file(&self.title, title.as_bytes())
            .with_context(|| format!("writing {}", self.title.display()))
    }

    /// Starts `to` with this conversation's journal, for `/branch` and `/fork`. The vendor
    /// session stays with this one, since two conversations must never share it.
    pub(crate) fn copy_to(&self, to: &ChatStore) -> anyhow::Result<()> {
        to.make_dir()?;
        let journal = match fs::read(&self.journal) {
            Ok(journal) => journal,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", self.journal.display()));
            }
        };
        write_private_file(&to.journal, &journal)
            .with_context(|| format!("writing {}", to.journal.display()))
    }

    /// The next turn starts a fresh session.
    pub(crate) fn forget_session(&self) -> anyhow::Result<()> {
        match fs::remove_file(&self.session) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("removing {}", self.session.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::names::TurnId;

    #[test]
    fn keeps_events_and_the_session_to_resume() {
        let dir = tempfile::tempdir().unwrap();
        let chat_dir = dir.path().join("chat");
        let chat = ChatStore::next_id(&chat_dir);
        assert_eq!(chat.as_str(), "chat-001");
        let store = ChatStore::open(&chat_dir, &chat);
        assert_eq!(store.session(), None);
        let event = ChatEvent::TurnCancelled {
            turn: TurnId::numbered(1),
        };
        store.append(&event).unwrap();
        store.append(&event).unwrap();
        assert_eq!(store.events().unwrap(), vec![event.clone(), event]);
        let session = ChatSession {
            worker: "claude-code".parse().unwrap(),
            vendor_session: "v-1".to_owned(),
        };
        store.save_session(&session).unwrap();
        assert_eq!(store.session(), Some(session));
        assert_eq!(ChatStore::next_id(&chat_dir).as_str(), "chat-002");
        store.forget_session().unwrap();
        store.forget_session().unwrap();
        assert_eq!(store.session(), None);
    }

    #[test]
    fn reading_back_skips_agent_events_and_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path(), &ChatId::numbered(1));
        assert_eq!(store.read_events().unwrap(), None);
        let cancelled = ChatEvent::TurnCancelled {
            turn: TurnId::numbered(1),
        };
        store.append(&cancelled).unwrap();
        store
            .append(&ChatEvent::Agent {
                turn: TurnId::numbered(1),
                event: argonauts_core::event::AgentEvent::Reasoning {
                    text: "hm".to_owned(),
                },
            })
            .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.journal_path())
            .unwrap();
        file.write_all(br#"{"chat":"turn_started","tu"#).unwrap();
        assert_eq!(store.read_events().unwrap(), Some(vec![cancelled]));
    }

    #[test]
    fn files_are_private_and_redacted() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state").join("repo");
        let chat_dir = state.join("chat");
        let store = ChatStore::open(&chat_dir, &ChatId::numbered(1));
        // Built at runtime so no string in this file looks like a real credential.
        let secret = format!("ghp_{}", "a".repeat(36));
        store
            .append(&ChatEvent::TurnFailed {
                turn: TurnId::numbered(1),
                error: format!("the agent printed {secret}"),
            })
            .unwrap();
        store
            .save_session(&ChatSession {
                worker: "claude-code".parse().unwrap(),
                vendor_session: "v-1".to_owned(),
            })
            .unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&chat_dir), 0o700);
        assert_eq!(mode(store.journal_path()), 0o600);
        assert_eq!(mode(&store.session), 0o600);
        let text = fs::read_to_string(store.journal_path()).unwrap();
        assert!(!text.contains(&secret), "{text}");
        assert!(text.contains("[redacted github token]"), "{text}");
    }
}
