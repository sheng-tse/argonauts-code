//! A conversation's files: `conversation/<conversation>.jsonl` holds its `ConversationEvent`s,
//! `conversation/<conversation>.json` the CLI session to resume, so `/resume` can carry the
//! conversation on in a later app session, `conversation/<conversation>.title` the name `/rename`
//! gave it, and `conversation/<conversation>.runs` the runs it started or opened, with their
//! titles. Like a run's files, they are yours alone and have secrets redacted, since the
//! conversation reads your checkout.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::RunId;
use argonauts_core::agents::agent_sessions::AgentSessionRef;
use argonauts_core::app::ConversationEvent;
use argonauts_core::base::clock;
use argonauts_core::base::names::{ConversationId, ModelRef};
use argonauts_core::base::paths::{create_private_dir_all, write_private_file};
use argonauts_core::safety::redact::{redact_json, redact_secrets};
use serde::{Deserialize, Serialize};

/// How an agent event's line starts, `{"conversation":"agent",...}`, so reading turns back skips it
/// unparsed.
const AGENT_TAG: &str = r#"{"conversation":"agent""#;

/// `conversation/<conversation>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationSession {
    pub worker: ModelRef,
    pub cli_session: String,
    /// The session is one to branch from, not the conversation's own: the next turn carries
    /// on a copy of it, made by the CLI's own fork, and the copy's id takes its place here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fork: bool,
}

/// `conversation/<conversation>.place.json`: a conversation that carries on an agent's session from
/// a run works where that session ran, with that session's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationPlace {
    pub agent: AgentSessionRef,
    pub worker: ModelRef,
    pub folder: PathBuf,
    /// The branch of the run's worktree, when it ran in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The run's worktree was gone, so Argonauts Code made it again for this conversation, and
    /// removes it once you're done with it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub made_again: bool,
}

/// A run a conversation started or opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationRun {
    pub run: RunId,
    /// Names a conversation that has no turn of its own.
    pub title: String,
}

pub(crate) struct ConversationStore {
    dir: PathBuf,
    journal: PathBuf,
    session: PathBuf,
    place: PathBuf,
    title: PathBuf,
    runs: PathBuf,
}

impl ConversationStore {
    /// When the journal was first written, which is when the conversation started. `None`
    /// before its first words.
    pub(crate) fn started(&self) -> Option<time::OffsetDateTime> {
        let meta = std::fs::metadata(&self.journal).ok()?;
        let at = meta.created().or_else(|_| meta.modified()).ok()?;
        Some(time::OffsetDateTime::from(at))
    }

    pub(crate) fn open(conversation_dir: &Path, conversation: &ConversationId) -> Self {
        Self {
            dir: conversation_dir.to_owned(),
            journal: conversation_dir.join(format!("{conversation}.jsonl")),
            session: conversation_dir.join(format!("{conversation}.json")),
            place: conversation_dir.join(format!("{conversation}.place.json")),
            title: conversation_dir.join(format!("{conversation}.title")),
            runs: conversation_dir.join(format!("{conversation}.runs")),
        }
    }

    /// The number after the highest conversation in `conversation_dir`, so a new app session never
    /// reuses an older conversation's files.
    pub(crate) fn next_id(conversation_dir: &Path) -> ConversationId {
        ConversationId::numbered(highest_number(conversation_dir).saturating_add(1))
    }

    /// A new conversation's id, set aside by creating its empty journal, so two app sessions
    /// opened at the same moment never share one, nor the runs it keeps.
    pub(crate) fn claim(conversation_dir: &Path) -> anyhow::Result<ConversationId> {
        create_private_dir_all(conversation_dir)
            .with_context(|| format!("creating {}", conversation_dir.display()))?;
        let mut number = highest_number(conversation_dir);
        loop {
            number = number
                .checked_add(1)
                .context("no conversation number is left")?;
            let id = ConversationId::numbered(number);
            let journal = Self::open(conversation_dir, &id).journal;
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&journal)
            {
                Ok(_) => return Ok(id),
                // Another app session took this number first.
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err).with_context(|| format!("creating {}", journal.display()));
                }
            }
        }
    }

    /// Nothing was ever written to the conversation: no turn in its journal, and no session,
    /// place, title, or run.
    pub(crate) fn is_unused(&self) -> bool {
        let no_turn = fs::metadata(&self.journal).map_or(true, |meta| meta.len() == 0);
        no_turn
            && [&self.session, &self.place, &self.title, &self.runs]
                .iter()
                .all(|path| !path.exists())
    }

    /// Gives the id back when nothing was ever written to the conversation, so opening the app
    /// and leaving it leaves no empty journal behind.
    pub(crate) fn release_if_empty(&self) {
        let empty = fs::metadata(&self.journal).is_ok_and(|meta| meta.len() == 0);
        if empty && let Err(err) = fs::remove_file(&self.journal) {
            tracing::warn!("removing {}: {err}", self.journal.display());
        }
    }

    pub(crate) fn journal_path(&self) -> &Path {
        &self.journal
    }

    pub(crate) fn append(&self, event: &ConversationEvent) -> anyhow::Result<()> {
        let mut value = serde_json::to_value(event).context("encoding a conversation event")?;
        redact_json(&mut value);
        // A turn's start says when it was, which the Behaviors map dates its actions by. Every
        // reader of the event leaves the extra key alone.
        if let (ConversationEvent::TurnStarted { .. }, Some(line)) = (event, value.as_object_mut())
        {
            let now = clock::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .context("writing a turn's time")?;
            line.insert(
                crate::behaviors::turn_times::STARTED_AT.to_owned(),
                serde_json::Value::String(now),
            );
        }
        let mut line = serde_json::to_vec(&value).context("encoding a conversation event")?;
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
    pub(crate) fn events(&self) -> anyhow::Result<Vec<ConversationEvent>> {
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

    /// Every event recorded, oldest first, for drawing the conversation again. A line that
    /// doesn't parse, such as a torn last line, is skipped, and no journal has none.
    pub(crate) fn every_event(&self) -> Vec<ConversationEvent> {
        fs::read_to_string(&self.journal)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The turns' own events, oldest first, leaving out what the agents did along the way, which
    /// makes up most of a journal. `None` when there's no journal. A line that doesn't parse,
    /// such as a torn last line, is skipped.
    pub(crate) fn read_events(&self) -> anyhow::Result<Option<Vec<ConversationEvent>>> {
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

    /// Every line of the journal, oldest first and still unparsed, for a reader that parses only
    /// the few it needs. No journal, or one that can't be read, has none.
    pub(crate) fn each_line(&self, see: impl FnMut(&str)) {
        if let Ok(text) = fs::read_to_string(&self.journal) {
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .for_each(see);
        }
    }

    /// A line of what an agent did along the way, which most of a journal is.
    pub(crate) fn is_agent_line(line: &str) -> bool {
        line.starts_with(AGENT_TAG)
    }

    /// Walks what the agents did along the way, a journal line at a time and still unparsed,
    /// oldest first. They make up most of a journal, so a reader after one kind of call parses
    /// only the lines that could be it. No journal, or one that can't be read, has none.
    pub(crate) fn each_agent_line(&self, see: impl FnMut(&str)) {
        if let Ok(text) = fs::read_to_string(&self.journal) {
            text.lines()
                .filter(|line| line.starts_with(AGENT_TAG))
                .for_each(see);
        }
    }

    /// `None` when there's none yet, or the file can't be read, which only means the next turn
    /// starts a fresh session.
    pub(crate) fn session(&self) -> Option<ConversationSession> {
        let text = fs::read_to_string(&self.session).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub(crate) fn save_session(&self, session: &ConversationSession) -> anyhow::Result<()> {
        let text =
            serde_json::to_vec_pretty(session).context("encoding the conversation session")?;
        self.replace(&self.session, &text)
    }

    /// `None` for a conversation that works in your checkout.
    pub(crate) fn place(&self) -> Option<ConversationPlace> {
        let text = fs::read_to_string(&self.place).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub(crate) fn save_place(&self, place: &ConversationPlace) -> anyhow::Result<()> {
        let text =
            serde_json::to_vec_pretty(place).context("encoding where the conversation works")?;
        self.replace(&self.place, &text)
    }

    /// Written aside and renamed, so a crash never leaves half a file.
    fn replace(&self, path: &Path, text: &[u8]) -> anyhow::Result<()> {
        self.make_dir()?;
        let partial = path.with_extension("json.partial");
        write_private_file(&partial, text)
            .with_context(|| format!("writing {}", partial.display()))?;
        fs::rename(&partial, path).with_context(|| format!("writing {}", path.display()))
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

    pub(crate) fn runs_path(&self) -> &Path {
        &self.runs
    }

    /// The runs this conversation started or opened, oldest first.
    pub(crate) fn runs(&self) -> Vec<RunId> {
        self.kept_runs().into_iter().map(|kept| kept.run).collect()
    }

    /// The title of its first run that has one, which names a conversation with no turn.
    pub(crate) fn first_run_title(&self) -> Option<String> {
        self.kept_runs()
            .into_iter()
            .map(|kept| kept.title)
            .find(|title| !title.is_empty())
    }

    /// One run a line, its id and then its title. A line that doesn't start with a run id is
    /// skipped.
    fn kept_runs(&self) -> Vec<ConversationRun> {
        fs::read_to_string(&self.runs)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let (run, title) = line.split_once(' ').unwrap_or((line, ""));
                Some(ConversationRun {
                    run: RunId::new(run).ok()?,
                    title: title.trim().to_owned(),
                })
            })
            .collect()
    }

    /// Adds a run to the conversation's runs, once.
    pub(crate) fn keep_run(&self, kept: &ConversationRun) -> anyhow::Result<()> {
        if self.runs().contains(&kept.run) {
            return Ok(());
        }
        // The title is your request's, so it's redacted, and kept to one line.
        let title: String = redact_secrets(&kept.title)
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        self.make_dir()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.runs)
            .with_context(|| format!("opening {}", self.runs.display()))?;
        writeln!(file, "{} {}", kept.run, title.trim())
            .with_context(|| format!("writing {}", self.runs.display()))
    }

    /// Starts `to` with this conversation's journal, its runs, and where it works, for `/branch`
    /// and `/fork`. The CLI session stays with this one, since two conversations must never
    /// share it.
    pub(crate) fn copy_to(&self, to: &ConversationStore) -> anyhow::Result<()> {
        to.make_dir()?;
        let journal = match fs::read(&self.journal) {
            Ok(journal) => journal,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", self.journal.display()));
            }
        };
        write_private_file(&to.journal, &journal)
            .with_context(|| format!("writing {}", to.journal.display()))?;
        match fs::read(&self.runs) {
            Ok(runs) => write_private_file(&to.runs, &runs)
                .with_context(|| format!("writing {}", to.runs.display()))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", self.runs.display()));
            }
        }
        match self.place() {
            Some(place) => to.save_place(&place),
            None => Ok(()),
        }
    }

    /// Every conversation in `conversation_dir` that works where an agent's session ran, by its id.
    pub(crate) fn places(conversation_dir: &Path) -> Vec<(ConversationId, ConversationPlace)> {
        let mut places: Vec<(ConversationId, ConversationPlace)> = fs::read_dir(conversation_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let id = ConversationId::parse(name.to_str()?.strip_suffix(".place.json")?)?;
                let place = ConversationStore::open(conversation_dir, &id).place()?;
                Some((id, place))
            })
            .collect();
        places.sort_by(|a, b| a.0.cmp(&b.0));
        places
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

/// The highest conversation number in `conversation_dir`, counting every file of a conversation, so
/// one that has only runs or a place still holds its number.
fn highest_number(conversation_dir: &Path) -> u32 {
    fs::read_dir(conversation_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let (id, _) = name.to_str()?.split_once('.')?;
            id.strip_prefix("conversation-")?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::base::names::TurnId;

    fn kept(run: &str, title: &str) -> ConversationRun {
        ConversationRun {
            run: RunId::new(run).unwrap(),
            title: title.to_owned(),
        }
    }

    #[test]
    fn keeps_each_run_once_and_holds_the_id_of_a_conversation_with_only_runs() {
        let dir = tempfile::tempdir().unwrap();
        let conversation_dir = dir.path().join("conversation");
        let store = ConversationStore::open(&conversation_dir, &ConversationId::numbered(1));
        assert!(store.runs().is_empty());
        assert_eq!(store.first_run_title(), None);
        let first = kept("run-001", "add --json\nto export");
        let second = kept("run-002", "fix the median");
        store.keep_run(&first).unwrap();
        store.keep_run(&second).unwrap();
        store.keep_run(&first).unwrap();
        assert_eq!(store.runs(), [first.run, second.run]);
        assert_eq!(
            store.first_run_title().as_deref(),
            Some("add --json to export")
        );
        // A new conversation never takes another's runs.
        assert_eq!(
            ConversationStore::next_id(&conversation_dir).as_str(),
            "conversation-002"
        );

        // A copy for /branch or /fork keeps them, with their titles.
        let copy = ConversationStore::open(&conversation_dir, &ConversationId::numbered(2));
        store.copy_to(&copy).unwrap();
        assert_eq!(copy.runs(), store.runs());
        assert_eq!(copy.first_run_title(), store.first_run_title());
    }

    /// Two app sessions opened at once, before either writes a thing, still get a conversation
    /// each, and one left unused gives its number back.
    #[test]
    fn a_claimed_id_is_never_shared_and_an_unused_one_goes_back() {
        let dir = tempfile::tempdir().unwrap();
        let conversation_dir = dir.path().join("conversation");
        let first = ConversationStore::claim(&conversation_dir).unwrap();
        let second = ConversationStore::claim(&conversation_dir).unwrap();
        assert_eq!(first.as_str(), "conversation-001");
        assert_eq!(second.as_str(), "conversation-002");

        let used = ConversationStore::open(&conversation_dir, &first);
        used.keep_run(&kept("run-001", "hi")).unwrap();
        used.release_if_empty();
        let unused = ConversationStore::open(&conversation_dir, &second);
        unused.release_if_empty();
        assert!(!unused.journal_path().exists());
        // The first keeps its number by its runs, the second's is free again.
        assert_eq!(
            ConversationStore::claim(&conversation_dir)
                .unwrap()
                .as_str(),
            "conversation-002"
        );

        // A conversation that only carries on an agent's session holds its number too.
        write_private_file(&conversation_dir.join("conversation-007.place.json"), b"{}").unwrap();
        assert_eq!(
            ConversationStore::next_id(&conversation_dir).as_str(),
            "conversation-008"
        );
    }

    #[test]
    fn keeps_events_and_the_session_to_resume() {
        let dir = tempfile::tempdir().unwrap();
        let conversation_dir = dir.path().join("conversation");
        let conversation = ConversationStore::next_id(&conversation_dir);
        assert_eq!(conversation.as_str(), "conversation-001");
        let store = ConversationStore::open(&conversation_dir, &conversation);
        assert_eq!(store.session(), None);
        let event = ConversationEvent::TurnCancelled {
            turn: TurnId::numbered(1),
        };
        store.append(&event).unwrap();
        store.append(&event).unwrap();
        assert_eq!(store.events().unwrap(), vec![event.clone(), event]);
        let session = ConversationSession {
            worker: "claude-code".parse().unwrap(),
            cli_session: "v-1".to_owned(),
            fork: false,
        };
        store.save_session(&session).unwrap();
        assert_eq!(store.session(), Some(session));
        assert_eq!(
            ConversationStore::next_id(&conversation_dir).as_str(),
            "conversation-002"
        );
        store.forget_session().unwrap();
        store.forget_session().unwrap();
        assert_eq!(store.session(), None);
    }

    #[test]
    fn reading_back_skips_agent_events_and_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConversationStore::open(dir.path(), &ConversationId::numbered(1));
        assert_eq!(store.read_events().unwrap(), None);
        let cancelled = ConversationEvent::TurnCancelled {
            turn: TurnId::numbered(1),
        };
        store.append(&cancelled).unwrap();
        store
            .append(&ConversationEvent::Agent {
                turn: TurnId::numbered(1),
                event: argonauts_core::runs::event::AgentEvent::Reasoning {
                    text: "hm".to_owned(),
                },
            })
            .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.journal_path())
            .unwrap();
        file.write_all(br#"{"conversation":"turn_started","tu"#)
            .unwrap();
        assert_eq!(store.read_events().unwrap(), Some(vec![cancelled]));
    }

    #[test]
    fn files_are_private_and_redacted() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state").join("repo");
        let conversation_dir = state.join("conversation");
        let store = ConversationStore::open(&conversation_dir, &ConversationId::numbered(1));
        // Built at runtime so no string in this file looks like a real credential.
        let secret = format!("ghp_{}", "a".repeat(36));
        store
            .append(&ConversationEvent::TurnFailed {
                turn: TurnId::numbered(1),
                error: format!("the agent printed {secret}"),
            })
            .unwrap();
        store
            .save_session(&ConversationSession {
                worker: "claude-code".parse().unwrap(),
                cli_session: "v-1".to_owned(),
                fork: false,
            })
            .unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&conversation_dir), 0o700);
        assert_eq!(mode(store.journal_path()), 0o600);
        assert_eq!(mode(&store.session), 0o600);
        let text = fs::read_to_string(store.journal_path()).unwrap();
        assert!(!text.contains(&secret), "{text}");
        assert!(text.contains("[redacted github token]"), "{text}");
    }
}
