//! Run events and the append-only JSONL log that stores them. The log is the run's source of
//! truth: the UI renders from it and resume rebuilds from it.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::event::AgentEvent;
use crate::ids::{RunId, TaskId};
use crate::meter::SpendStatus;
use crate::names::ModelRef;
use crate::paths::create_private_dir_all;
use crate::session::SessionRole;
use crate::task::Report;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    /// When it happened, in UTC.
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub run_id: RunId,
    pub task_id: Option<TaskId>,
    /// What produced it, such as an adapter id like `codex`, or `argonauts-code`.
    pub source: String,
    pub kind: RunEventKind,
}

impl RunEvent {
    pub fn now(
        run_id: RunId,
        task_id: Option<TaskId>,
        source: impl Into<String>,
        kind: RunEventKind,
    ) -> Self {
        Self {
            ts: OffsetDateTime::now_utc(),
            run_id,
            task_id,
            source: source.into(),
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RunEventKind {
    RunStarted(RunStarted),
    Agent(AgentEvent),
    Phase(PhaseEvent),
    SessionStarted(SessionStarted),
    SessionEnded(SessionEnded),
    CheckResult(CheckResult),
    Commit(CommitInfo),
    Report(Report),
    Approval(Approval),
    Decision(DecisionRecord),
    Spend(SpendStatus),
    Note(Note),
    RunFinished(RunFinished),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStarted {
    pub task: String,
    pub mode: String,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub repo_root: PathBuf,
    pub base_branch: Option<String>,
    pub base_commit: String,
    /// SHA-256 of the resolved config.
    pub config_hash: String,
    pub config: Value,
    pub version: String,
}

/// Steps of the Captain pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Intake,
    Goal,
    Worktree,
    Agent,
    Commit,
    Checks,
    Decision,
    Merge,
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Started,
    Finished,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseEvent {
    pub phase: Phase,
    pub status: PhaseStatus,
    /// One line of detail for the UI.
    pub detail: Option<String>,
}

/// An agent session as launched. Environment values are never recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStarted {
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub role: SessionRole,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub prompt: String,
    /// SHA-256 of the approved goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    Limit,
    User,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnded {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub duration_ms: u64,
    pub stopped_by: Option<StopCause>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub command: Vec<String>,
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// The tail of its combined output.
    pub output: String,
}

/// Line counts from `git diff --stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffStat {
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
}

impl DiffStat {
    pub fn is_empty(&self) -> bool {
        self.files_changed == 0 && self.insertions == 0 && self.deletions == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    /// The task branch, such as `argonauts/run-01/task-001`.
    pub branch: String,
    /// The new commit, or `None` when there was nothing to commit.
    pub sha: Option<String>,
    pub message: String,
    pub stat: DiffStat,
    /// The model whose work this commits, so the diff view and quality debt can tell
    /// fallbacks' commits apart. `None` is your own, such as take-over work, and so is
    /// every commit in a log written before this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<ModelRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Merge,
}

/// A point where the run waits for an answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Approval {
    Merge(MergeTask),
}

impl Approval {
    pub fn kind(&self) -> ApprovalKind {
        match self {
            Self::Merge(_) => ApprovalKind::Merge,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeTask {
    pub branch: String,
    /// The branch an approval merges into, if one was checked out at start.
    pub target_branch: Option<String>,
    pub stat: DiffStat,
    pub checks: Vec<CheckResult>,
    /// The agent's report, if it sent a valid one.
    pub report: Option<Report>,
    pub spend: Option<SpendStatus>,
    /// Set when Argonauts Code stopped the agent before it finished, so the work may be partial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<StopCause>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "choice", rename_all = "snake_case")]
pub enum MergeDecision {
    Approve,
    /// Keep the branch and record what should change.
    RequestChanges {
        note: Option<String>,
    },
    /// Keep the branch and suggest a pull request.
    OpenPr,
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    User,
    /// A configured setting, such as `merge.mode = "leave"`.
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub approval: ApprovalKind,
    pub decision: MergeDecision,
    pub by: DecidedBy,
    /// The task commit the answer applies to, the one the user reviewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteLevel {
    Info,
    Warn,
    Error,
    /// About where you are rather than about something that happened, such as a folder with
    /// no git: drawn with `※`, as Claude Code draws its asides.
    Aside,
    /// Worth one dim line and no mark, said once and never again, such as a newer version of
    /// a CLI being out.
    Quiet,
}

/// The note a failed run review leaves. Why it failed is in the run review phase's `Failed`
/// summary, which the main view keeps for Ctrl+O, so a raw schema error never shows there.
pub const RUN_REVIEW_FAILED: &str = "The run review failed. The run's result stands.";

/// Starts the note that names the copies removed when a run ended, such as `base, 2 tasks,
/// merge`. The main view says it on the run's last row instead of a row of its own.
pub const COPIES_REMOVED: &str = "the run's copies removed: ";

/// Why a fallback skips the retry, after ` · ` at the end of its note. The main view draws
/// these words dim and the ones before them plain.
pub const NO_SECOND_TRY: &str = "no second try, this error repeats every time";

/// How a fallback note's second line starts when `fallback.same_cli` kept the work on its own
/// CLI, as in `same_cli is on, so Codex isn't asked`. The main view shows that line under the
/// note rather than keeping it for Ctrl+O.
pub const SAME_CLI_IS_ON: &str = "same_cli is on, so ";

/// One line in plain words. Lines after the first are its detail, such as a validator's
/// errors, which the main view keeps for Ctrl+O, but for the rows right under the first
/// line, which it shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub level: NoteLevel,
    pub text: String,
}

/// Starts a row in a note's text: a line the main view shows under the first, where any
/// other waits for Ctrl+O.
const ROW: &str = "\u{23bf} ";

impl Note {
    /// The text of a note that says `line` and shows `rows` under it, such as the files a
    /// run left out.
    pub fn text_with_rows(line: &str, rows: &[String]) -> String {
        let mut text = line.trim().to_owned();
        for row in rows {
            text.push('\n');
            text.push_str(ROW);
            text.push_str(row.trim_end());
        }
        text
    }

    /// The rows the main view shows under the first line.
    pub fn rows(&self) -> impl Iterator<Item = &str> {
        self.text
            .lines()
            .skip(1)
            .map_while(|line| line.strip_prefix(ROW))
    }

    /// A note that says what happened in `line`, with the raw `detail` behind it.
    pub fn with_detail(level: NoteLevel, line: &str, detail: &str) -> Self {
        let (line, detail) = (line.trim(), detail.trim());
        let text = if detail.is_empty() {
            line.to_owned()
        } else {
            format!("{line}\n{detail}")
        };
        Self { level, text }
    }

    /// What the main view shows: the first line.
    pub fn line(&self) -> &str {
        self.text.lines().next().unwrap_or_default().trim()
    }

    /// The lines after the first, for Ctrl+O. `None` for a note of one line.
    pub fn detail(&self) -> Option<&str> {
        let mut rest = self.text.split_once('\n').map(|(_, rest)| rest)?;
        while rest.starts_with(ROW) {
            rest = rest.split_once('\n').map_or("", |(_, after)| after);
        }
        Some(rest.trim()).filter(|rest| !rest.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Merged,
    BranchLeft,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFinished {
    pub outcome: RunOutcome,
    /// One line of detail for the user.
    pub detail: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EventLogError {
    #[error("event log {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("could not serialize event: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Each append writes one complete line straight to the file, so a crash loses at most the line
/// being written.
#[derive(Debug)]
pub struct EventLog {
    file: File,
    path: PathBuf,
}

impl EventLog {
    /// New files and folders are private to your user, because the log holds prompts, the
    /// transcript, and tool output.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, EventLogError> {
        let path = path.into();
        let io_err = |source| io_error(&path, source);
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent).map_err(io_err)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .map_err(io_err)?;
        // A crash can leave a partial last line. Start a fresh line so the next event stays
        // readable.
        if ends_without_newline(&mut file).map_err(io_err)? {
            file.write_all(b"\n").map_err(io_err)?;
        }
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&mut self, event: &RunEvent) -> Result<(), EventLogError> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        self.file
            .write_all(&line)
            .map_err(|source| io_error(&self.path, source))
    }

    /// Forces everything appended so far onto disk.
    pub fn flush(&mut self) -> Result<(), EventLogError> {
        self.file
            .sync_data()
            .map_err(|source| io_error(&self.path, source))
    }

    /// Reads every event back. Lines that don't parse, including ones that aren't UTF-8, are
    /// skipped and reported, never fatal.
    pub fn read(path: &Path) -> Result<ReadBack, EventLogError> {
        let io_err = |source| io_error(path, source);
        let reader = BufReader::new(File::open(path).map_err(io_err)?);
        let mut back = ReadBack::default();
        for (index, line) in reader.split(b'\n').enumerate() {
            let line = line.map_err(io_err)?;
            if line.trim_ascii().is_empty() {
                continue;
            }
            match serde_json::from_slice::<RunEvent>(&line) {
                Ok(event) => back.events.push(event),
                Err(err) => back.skipped.push(SkippedLine {
                    line: index + 1,
                    error: err.to_string(),
                }),
            }
        }
        Ok(back)
    }
}

fn io_error(path: &Path, source: io::Error) -> EventLogError {
    EventLogError::Io {
        path: path.to_owned(),
        source,
    }
}

fn ends_without_newline(file: &mut File) -> io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

#[derive(Debug, Default)]
pub struct ReadBack {
    pub events: Vec<RunEvent>,
    pub skipped: Vec<SkippedLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLine {
    /// 1-based line number.
    pub line: usize,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_shows_its_rows_and_keeps_the_rest_for_later() {
        let rows = [
            "secrets   .env".to_owned(),
            "  packages  none here ".to_owned(),
        ];
        let text = Note::text_with_rows(" Left out: ", &rows);
        let mut note = Note {
            level: NoteLevel::Info,
            text,
        };
        assert_eq!(note.line(), "Left out:");
        assert_eq!(
            note.rows().collect::<Vec<_>>(),
            ["secrets   .env", "  packages  none here"]
        );
        assert_eq!(note.detail(), None);

        note.text.push_str("\nthe raw error\nits second line");
        assert_eq!(note.rows().count(), 2);
        assert_eq!(note.detail(), Some("the raw error\nits second line"));

        let plain = Note::with_detail(NoteLevel::Warn, "It failed", "why");
        assert_eq!(plain.rows().count(), 0);
        assert_eq!(plain.detail(), Some("why"));
    }

    use crate::meter::{LimitDimension, MeterAction, MeterLimit, Spend, WorkLimits, WorkUsed};
    use crate::task::ReportStatus;
    use serde_json::json;
    use time::macros::datetime;

    fn run() -> RunId {
        RunId::new("run-01").unwrap()
    }

    fn task() -> Option<TaskId> {
        Some(TaskId::new("task-001").unwrap())
    }

    fn event(kind: RunEventKind) -> RunEvent {
        RunEvent {
            ts: datetime!(2026-09-29 04:15:03.25 UTC),
            run_id: run(),
            task_id: task(),
            source: "argonauts-code".into(),
            kind,
        }
    }

    fn report() -> Report {
        Report {
            status: ReportStatus::Done,
            summary: "ok".into(),
            files_changed: vec![],
            tests_run: vec![],
            concerns: vec![],
        }
    }

    fn every_kind() -> Vec<RunEventKind> {
        let check = CheckResult {
            command: vec!["test".into(), "-f".into(), "hello.txt".into()],
            passed: true,
            exit_code: Some(0),
            duration_ms: 3,
            output: String::new(),
        };
        let spend = SpendStatus {
            level: MeterAction::Continue,
            limit: MeterLimit::Usd(3.0),
            spend: Spend::default(),
            fraction_used: Some(0.0),
            work: WorkUsed {
                minutes: 1.5,
                tool_calls: 4,
            },
            work_limits: WorkLimits {
                max_minutes: Some(60.0),
                max_tool_calls: None,
            },
            binding: Some(LimitDimension::Spend),
        };
        vec![
            RunEventKind::RunStarted(RunStarted {
                task: "t".into(),
                mode: "captain".into(),
                agent: "codex".into(),
                model: None,
                effort: Some("low".into()),
                repo_root: "/r".into(),
                base_branch: Some("main".into()),
                base_commit: "abc".into(),
                config_hash: "f00".into(),
                config: json!({"crew": {"main": "codex"}}),
                version: "0.0.1".into(),
            }),
            RunEventKind::Agent(AgentEvent::FileChanged {
                path: "a".into(),
                kind: "add".into(),
            }),
            RunEventKind::Phase(PhaseEvent {
                phase: Phase::Checks,
                status: PhaseStatus::Started,
                detail: None,
            }),
            RunEventKind::SessionStarted(SessionStarted {
                agent: "codex".into(),
                model: None,
                effort: None,
                role: SessionRole::Build,
                program: "codex".into(),
                args: vec!["exec".into()],
                cwd: "/w".into(),
                prompt: "p".into(),
                goal_hash: Some("9f86d081".into()),
            }),
            RunEventKind::SessionEnded(SessionEnded {
                exit_code: None,
                signal: Some(2),
                duration_ms: 10,
                stopped_by: Some(StopCause::Limit),
            }),
            RunEventKind::CheckResult(check.clone()),
            RunEventKind::Commit(CommitInfo {
                branch: "argonauts/run-01/task-001".into(),
                sha: Some("abc".into()),
                message: "m".into(),
                stat: DiffStat {
                    files_changed: 1,
                    insertions: 1,
                    deletions: 0,
                },
                by: None,
            }),
            RunEventKind::Report(report()),
            RunEventKind::Approval(Approval::Merge(MergeTask {
                branch: "b".into(),
                target_branch: Some("main".into()),
                stat: DiffStat::default(),
                checks: vec![check],
                report: Some(report()),
                spend: Some(spend),
                stopped_by: Some(StopCause::Limit),
            })),
            RunEventKind::Decision(DecisionRecord {
                approval: ApprovalKind::Merge,
                decision: MergeDecision::RequestChanges {
                    note: Some("rename it".into()),
                },
                by: DecidedBy::User,
                commit: Some("abc".into()),
            }),
            RunEventKind::Spend(spend),
            RunEventKind::Note(Note {
                level: NoteLevel::Warn,
                text: "careful".into(),
            }),
            RunEventKind::RunFinished(RunFinished {
                outcome: RunOutcome::BranchLeft,
                detail: None,
            }),
        ]
    }

    #[test]
    fn wire_format() {
        let value = serde_json::to_value(event(RunEventKind::Agent(AgentEvent::TextDelta {
            text: "hi".into(),
            parent: None,
        })))
        .unwrap();
        assert_eq!(
            value,
            json!({
                "ts": "2026-09-29T04:15:03.25Z",
                "run_id": "run-01",
                "task_id": "task-001",
                "source": "argonauts-code",
                "kind": {"type": "agent", "data": {"type": "text_delta", "text": "hi", "parent": null}},
            })
        );
        let decision = serde_json::to_value(MergeDecision::Approve).unwrap();
        assert_eq!(decision, json!({"choice": "approve"}));
        let approval = serde_json::to_value(Approval::Merge(MergeTask {
            branch: "b".into(),
            target_branch: None,
            stat: DiffStat::default(),
            checks: vec![],
            report: None,
            spend: None,
            stopped_by: None,
        }))
        .unwrap();
        assert_eq!(approval["kind"], "merge");
        assert!(approval.get("stopped_by").is_none());
    }

    #[test]
    fn every_kind_round_trips() {
        for kind in every_kind() {
            let original = event(kind);
            let text = serde_json::to_string(&original).unwrap();
            let back: RunEvent = serde_json::from_str(&text).unwrap();
            assert_eq!(back, original, "{text}");
        }
    }

    #[test]
    fn append_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/run/events.jsonl");
        let mut log = EventLog::open(&path).unwrap();
        for kind in every_kind() {
            log.append(&event(kind)).unwrap();
        }
        log.flush().unwrap();
        assert_eq!(log.path(), path);
        let back = EventLog::read(&path).unwrap();
        assert!(back.skipped.is_empty(), "{:?}", back.skipped);
        assert_eq!(back.events.len(), every_kind().len());
        assert_eq!(back.events[0], event(every_kind().remove(0)));
    }

    #[test]
    fn reopening_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let note = || {
            event(RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text: "n".into(),
            }))
        };
        EventLog::open(&path).unwrap().append(&note()).unwrap();
        EventLog::open(&path).unwrap().append(&note()).unwrap();
        assert_eq!(EventLog::read(&path).unwrap().events.len(), 2);
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_later_appends_survive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let note = event(RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: "n".into(),
        }));
        let mut log = EventLog::open(&path).unwrap();
        log.append(&note).unwrap();
        drop(log);
        let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
        raw.write_all(br#"{"ts":"2026-09-29T0"#).unwrap();
        drop(raw);

        let mut log = EventLog::open(&path).unwrap();
        log.append(&note).unwrap();
        let back = EventLog::read(&path).unwrap();
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.skipped.len(), 1);
        assert_eq!(back.skipped[0].line, 2);
    }

    #[test]
    fn a_torn_multibyte_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let note = event(RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: "é".into(),
        }));
        let mut log = EventLog::open(&path).unwrap();
        log.append(&note).unwrap();
        drop(log);
        let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
        // The first byte of a two-byte character, cut off by a crash.
        raw.write_all(b"{\"text\":\"\xc3").unwrap();
        drop(raw);
        let mut log = EventLog::open(&path).unwrap();
        log.append(&note).unwrap();

        let back = EventLog::read(&path).unwrap();
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.skipped.len(), 1);
        assert_eq!(back.skipped[0].line, 2);
    }

    #[test]
    fn new_logs_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/events.jsonl");
        EventLog::open(&path).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.path().join("run")), 0o700);
    }

    #[test]
    fn older_records_without_new_fields_still_read() {
        let started = r#"{"agent":"codex","model":null,"effort":null,"role":"build","program":"codex","args":[],"cwd":"/w","prompt":"p"}"#;
        let started: SessionStarted = serde_json::from_str(started).unwrap();
        assert_eq!(started.goal_hash, None);
        let decision = r#"{"approval":"merge","decision":{"choice":"leave"},"by":"config"}"#;
        let decision: DecisionRecord = serde_json::from_str(decision).unwrap();
        assert_eq!(decision.commit, None);
    }

    #[test]
    fn blank_lines_are_ignored_and_missing_files_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "\n\n").unwrap();
        let back = EventLog::read(&path).unwrap();
        assert!(back.events.is_empty() && back.skipped.is_empty());
        let missing = EventLog::read(&dir.path().join("nope.jsonl"));
        assert!(matches!(missing, Err(EventLogError::Io { .. })));
    }

    #[test]
    fn a_notes_detail_follows_its_line() {
        let note = Note::with_detail(
            NoteLevel::Warn,
            "The main agent's design came back incomplete.",
            "output didn't match the schema: \"name\" is a required property",
        );
        assert_eq!(note.line(), "The main agent's design came back incomplete.");
        assert_eq!(
            note.detail(),
            Some("output didn't match the schema: \"name\" is a required property")
        );
        let plain = Note::with_detail(NoteLevel::Info, "Done.", "  ");
        assert_eq!(plain.text, "Done.");
        assert_eq!(plain.detail(), None);
    }

    #[test]
    fn now_stamps_utc() {
        let e = RunEvent::now(
            run(),
            None,
            "argonauts-code",
            RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text: "x".into(),
            }),
        );
        assert!(e.ts.offset().is_utc());
        assert_eq!(e.source, "argonauts-code");
    }
}
