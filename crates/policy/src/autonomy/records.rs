//! `approvals.jsonl` in the repo's state folder: one record per line, appended under a file
//! lock, never rewritten.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::layout::repo_state;
use argonauts_core::names::ApprovalId;
use argonauts_core::policy::{DecisionPoint, Verdict};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::file_lock::{self, Access};

/// Records are per decision-point kind and per learned workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// `None`, with `approval`, on a record about the whole kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalId>,
    pub kind: ApprovalKind,
    pub learned_workflow: Option<String>,
    pub event: Event,
}

impl Record {
    pub fn new(point: &DecisionPoint, at: OffsetDateTime, event: Event) -> Self {
        Self {
            at,
            run: Some(point.run.clone()),
            approval: Some(point.approval.clone()),
            kind: point.kind,
            learned_workflow: point.learned_workflow.clone(),
            event,
        }
    }

    pub fn for_kind(kind: ApprovalKind, at: OffsetDateTime, event: Event) -> Self {
        Self {
            at,
            run: None,
            approval: None,
            kind,
            learned_workflow: None,
            event,
        }
    }

    pub fn key(&self) -> (Option<&RunId>, Option<&ApprovalId>) {
        (self.run.as_ref(), self.approval.as_ref())
    }

    fn is_for(&self, run: &RunId, approval: &ApprovalId) -> bool {
        self.key() == (Some(run), Some(approval))
    }

    /// Losing one of these to a crash could raise autonomy.
    fn must_survive_a_crash(&self) -> bool {
        matches!(
            self.event,
            Event::Scored {
                outcome: Outcome::Miss,
                ..
            } | Event::SpotChecked { .. }
                | Event::Reserved { .. }
        )
    }
}

/// A fingerprint is a hash of the kind's rules, `decide_with`, and judge when it's scored.
/// `rules` is the SHA-256 of your rules document when the judge read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Answered {
        answer: Answer,
    },
    /// What auto would have decided, waiting for your answer.
    AutoPreview {
        fingerprint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rules: Option<String>,
        would: Verdict,
    },
    Scored {
        fingerprint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rules: Option<String>,
        outcome: Outcome,
    },
    /// Auto approved: the run's spend is recorded, and `merge_key` holds today's auto-merge.
    Reserved {
        fingerprint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rules: Option<String>,
        merge_key: Option<String>,
        spot_check: bool,
    },
    /// Your answer beat auto's, so the reservation was given back.
    Released,
    /// You checked an auto-approval, or the host found its merge reverted. A wrong one demotes
    /// the kind to auto preview until you reinstate it.
    SpotChecked {
        fingerprint: Option<String>,
        correct: bool,
    },
    /// Something you approved yourself turned out wrong, such as a merge reverted later. Its
    /// match counts as a miss, but nothing is demoted, since auto didn't act.
    Reverted,
    /// You lifted the kind's demotion. Written for the whole kind, never by a run.
    Reinstated,
}

/// Your answer, as far as scoring an auto preview verdict goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// Approved or merged without edits.
    Approved,
    /// Any answer that decides the approval some other way, such as edits, a send back, a
    /// pull request instead of a merge, or waiting.
    Corrected,
    /// An answer that decides nothing, which doesn't score.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Match,
    Miss,
}

pub struct Records {
    path: PathBuf,
    lock_wait: Duration,
    /// What this process has read, so each call parses only the lines added since.
    seen: Mutex<Seen>,
}

#[derive(Default)]
struct Seen {
    /// The file read, by device and inode, so one removed and made again is read afresh.
    file: Option<(u64, u64)>,
    /// Bytes read, up to and including the last newline.
    len: u64,
    /// The last line read, with its newline. It's checked before reading on, so a file
    /// replaced in place is read afresh.
    last_line: Vec<u8>,
    lines: usize,
    records: Arc<Vec<Record>>,
}

impl Records {
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(repo_state::APPROVALS),
            lock_wait: file_lock::RECORDS_WAIT,
            seen: Mutex::default(),
        }
    }

    #[cfg(test)]
    pub fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, record: &Record) -> io::Result<()> {
        self.lock()?.append(std::slice::from_ref(record))
    }

    /// A torn last line, from a write cut short, is skipped. Any other line that doesn't parse
    /// is an error, since a lost miss or demotion would raise autonomy.
    pub fn read_all(&self) -> io::Result<Arc<Vec<Record>>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return match self.path.parent() {
                    Some(dir) if dir.is_dir() => {
                        *self.seen() = Seen::default();
                        Ok(Arc::default())
                    }
                    _ => Err(self.error(err)),
                };
            }
            Err(err) => return Err(self.error(err)),
        };
        file_lock::lock_within(&file, Access::Shared, self.lock_wait)
            .map_err(|err| self.error(err))?;
        let mut seen = self.seen();
        self.catch_up(&file, &mut seen)?;
        Ok(Arc::clone(&seen.records))
    }

    /// Holds the file's exclusive lock until the result is dropped, so a check and the records
    /// that follow from it are written as one step, across threads and run hosts.
    pub fn lock(&self) -> io::Result<LockedRecords<'_>> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(|err| self.error(err))?;
        file_lock::lock_within(&file, Access::Exclusive, self.lock_wait)
            .map_err(|err| self.error(err))?;
        let mut seen = self.seen();
        let len = self.catch_up(&file, &mut seen)?;
        Ok(LockedRecords {
            records: self,
            file,
            seen,
            len,
        })
    }

    /// Taken only while the file is locked. A panic mid-update leaves it to be read afresh.
    fn seen(&self) -> MutexGuard<'_, Seen> {
        self.seen.lock().unwrap_or_else(|poisoned| {
            self.seen.clear_poison();
            let mut seen = poisoned.into_inner();
            *seen = Seen::default();
            seen
        })
    }

    /// Parses the lines added since this process last read, and returns the file's length.
    fn catch_up(&self, mut file: &File, seen: &mut Seen) -> io::Result<u64> {
        let meta = file.metadata().map_err(|err| self.error(err))?;
        let id = Some((meta.dev(), meta.ino()));
        if seen.file != id || !self.only_appended(file, seen)? {
            *seen = Seen {
                file: id,
                ..Seen::default()
            };
        }

        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(seen.len))
            .and_then(|_| file.read_to_end(&mut bytes))
            .map_err(|err| self.error(err))?;
        let complete = bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |at| at + 1);
        let text = std::str::from_utf8(&bytes[..complete]).map_err(|err| self.invalid(err))?;
        let mut lines = seen.lines;
        let mut new = Vec::new();
        for line in text.lines() {
            lines += 1;
            if line.trim().is_empty() {
                continue;
            }
            let record = serde_json::from_str(line)
                .map_err(|err| self.invalid(format!("line {lines}: {err}")))?;
            new.push(record);
        }

        if complete > 0 {
            let last_start = bytes[..complete - 1]
                .iter()
                .rposition(|&byte| byte == b'\n')
                .map_or(0, |at| at + 1);
            seen.last_line = bytes[last_start..complete].to_vec();
        }
        seen.len += complete as u64;
        seen.lines = lines;
        Arc::make_mut(&mut seen.records).extend(new);
        Ok(seen.len + (bytes.len() - complete) as u64)
    }

    /// Whether the file still holds the last line read where it was read.
    fn only_appended(&self, mut file: &File, seen: &Seen) -> io::Result<bool> {
        let Some(start) = seen.len.checked_sub(seen.last_line.len() as u64) else {
            return Ok(false);
        };
        let mut line = vec![0; seen.last_line.len()];
        let read = file
            .seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut line));
        match read {
            Ok(()) => Ok(line == seen.last_line),
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(err) => Err(self.error(err)),
        }
    }

    fn error(&self, err: io::Error) -> io::Error {
        io::Error::new(err.kind(), format!("{}: {err}", self.path.display()))
    }

    fn invalid(&self, detail: impl std::fmt::Display) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {detail}", self.path.display()),
        )
    }
}

pub struct LockedRecords<'a> {
    records: &'a Records,
    file: File,
    seen: MutexGuard<'a, Seen>,
    /// The file's length, past what was read when the last line is torn.
    len: u64,
}

impl LockedRecords<'_> {
    pub fn records(&self) -> &[Record] {
        &self.seen.records
    }

    /// Misses, spot checks, and reservations are on disk before this returns.
    pub fn append(&mut self, new: &[Record]) -> io::Result<()> {
        if new.is_empty() {
            return Ok(());
        }
        let mut lines = String::new();
        let mut last_start = 0;
        for record in new {
            last_start = lines.len();
            lines.push_str(&serde_json::to_string(record).map_err(io::Error::other)?);
            lines.push('\n');
        }
        let error = |err| self.records.error(err);
        let seen = &mut *self.seen;
        if seen.len < self.len {
            // Otherwise the next record would share a line with the torn one and be lost too.
            self.file.set_len(seen.len).map_err(error)?;
            self.len = seen.len;
        }
        self.file.write_all(lines.as_bytes()).map_err(error)?;
        self.len += lines.len() as u64;
        seen.len = self.len;
        seen.lines += new.len();
        seen.last_line = lines.as_bytes()[last_start..].to_vec();
        Arc::make_mut(&mut seen.records).extend_from_slice(new);
        if new.iter().any(Record::must_survive_a_crash) {
            self.file.sync_data().map_err(error)?;
        }
        Ok(())
    }
}

/// What the records say about one approval of one run.
#[derive(Debug, Default)]
pub struct ApprovalLog<'a> {
    pub answer: Option<Answer>,
    /// The first auto preview verdict noted, with what it was decided under.
    pub auto_preview: Option<(Basis<'a>, Verdict)>,
    pub scored: bool,
    /// Auto's reservation, unless your answer released it.
    pub reservation: Option<ReservationEntry<'a>>,
    /// A wrong spot check or a revert is already on record.
    pub wrong: bool,
}

/// What a verdict was decided under: the kind's settings fingerprint, and the SHA-256 of your
/// rules document when the judge read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Basis<'a> {
    pub fingerprint: &'a str,
    pub rules: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct ReservationEntry<'a> {
    pub fingerprint: Option<&'a str>,
    pub rules: Option<&'a str>,
    pub merge_key: Option<&'a str>,
    pub spot_check: bool,
}

pub fn approval_log<'a>(
    records: &'a [Record],
    run: &RunId,
    approval: &ApprovalId,
) -> ApprovalLog<'a> {
    let mut log = ApprovalLog::default();
    for record in records.iter().filter(|record| record.is_for(run, approval)) {
        match &record.event {
            Event::Answered { answer } => {
                log.answer.get_or_insert(*answer);
            }
            Event::AutoPreview {
                fingerprint,
                rules,
                would,
            } => {
                let basis = Basis {
                    fingerprint,
                    rules: rules.as_deref(),
                };
                log.auto_preview.get_or_insert((basis, *would));
            }
            Event::Scored { .. } => log.scored = true,
            Event::Reserved {
                fingerprint,
                rules,
                merge_key,
                spot_check,
            } => {
                log.reservation = Some(ReservationEntry {
                    fingerprint: fingerprint.as_deref(),
                    rules: rules.as_deref(),
                    merge_key: merge_key.as_deref(),
                    spot_check: *spot_check,
                });
            }
            Event::Released => log.reservation = None,
            Event::SpotChecked { correct: false, .. } | Event::Reverted => log.wrong = true,
            Event::SpotChecked { .. } | Event::Reinstated => {}
        }
    }
    log
}

/// Auto-approvals drawn for a spot check that you haven't checked, and that your own answer
/// didn't replace.
pub fn spot_checks_waiting(records: &[Record]) -> u32 {
    let mut waiting = BTreeSet::new();
    for record in records {
        let key = record.key();
        match record.event {
            Event::Reserved {
                spot_check: true, ..
            } => {
                waiting.insert(key);
            }
            Event::Released | Event::SpotChecked { .. } => {
                waiting.remove(&key);
            }
            _ => {}
        }
    }
    u32::try_from(waiting.len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use time::macros::datetime;

    use super::*;

    fn answered(n: u32) -> Record {
        let point = DecisionPoint {
            run: RunId::new("run-a").expect("run id"),
            approval: ApprovalId::numbered(n),
            kind: ApprovalKind::Understand,
            learned_workflow: None,
            authors: Vec::new(),
            mode_override: None,
            main_agent: None,
        };
        Record::new(
            &point,
            datetime!(2026-09-29 10:00 UTC),
            Event::Answered {
                answer: Answer::Approved,
            },
        )
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_replaced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let records = Records::in_state_dir(dir.path());
        assert!(records.read_all().expect("empty").is_empty());

        records.append(&answered(1)).expect("append");
        let mut text = fs::read_to_string(records.path()).expect("read");
        text.push_str("{\"at\":\"2026-");
        fs::write(records.path(), text).expect("tear");
        assert_eq!(records.read_all().expect("read").len(), 1);

        records.append(&answered(2)).expect("append after a tear");
        assert_eq!(
            *records.read_all().expect("read"),
            vec![answered(1), answered(2)]
        );
        let mode = fs::metadata(records.path())
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_bad_whole_line_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let records = Records::in_state_dir(dir.path());
        fs::write(records.path(), "not a record\n").expect("write");
        assert!(records.read_all().is_err());
        assert!(records.lock().is_err());
    }

    #[test]
    fn a_missing_state_folder_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let records = Records::in_state_dir(&dir.path().join("gone"));
        assert!(records.read_all().is_err());
        assert!(records.append(&answered(1)).is_err());
    }

    #[test]
    fn appends_from_elsewhere_are_read_on_from_where_this_process_stopped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let here = Records::in_state_dir(dir.path());
        let elsewhere = Records::in_state_dir(dir.path());
        here.append(&answered(1)).expect("append here");
        elsewhere.append(&answered(2)).expect("append elsewhere");
        assert_eq!(
            *here.read_all().expect("read"),
            vec![answered(1), answered(2)]
        );

        // Line numbers count from the top of the file, not from where reading went on.
        let mut text = fs::read_to_string(here.path()).expect("read");
        text.push_str("not a record\n");
        fs::write(here.path(), text).expect("write");
        let err = here.read_all().expect_err("a bad line");
        assert!(err.to_string().contains("line 3"), "{err}");
    }

    #[test]
    fn a_file_replaced_or_removed_is_read_afresh() {
        let dir = tempfile::tempdir().expect("temp dir");
        let records = Records::in_state_dir(dir.path());
        records.append(&answered(1)).expect("append");
        records.append(&answered(2)).expect("append");
        assert_eq!(records.read_all().expect("read").len(), 2);

        // Rewritten in place, as `cp` does, and longer than what was read.
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let other = Records::in_state_dir(elsewhere.path());
        for n in [7, 8, 9] {
            other.append(&answered(n)).expect("append");
        }
        fs::write(records.path(), fs::read(other.path()).expect("read other")).expect("replace");
        assert_eq!(
            *records.read_all().expect("read"),
            vec![answered(7), answered(8), answered(9)]
        );

        fs::remove_file(records.path()).expect("remove");
        assert!(records.read_all().expect("read").is_empty());
        records.append(&answered(4)).expect("append");
        assert_eq!(*records.read_all().expect("read"), vec![answered(4)]);
    }

    #[test]
    fn a_lock_held_elsewhere_fails_instead_of_waiting() {
        let dir = tempfile::tempdir().expect("temp dir");
        let records = Records::in_state_dir(dir.path()).with_lock_wait(Duration::from_millis(50));
        records.append(&answered(1)).expect("append");
        let holder = File::open(records.path()).expect("open");
        holder.lock().expect("hold");

        let read = records.read_all().expect_err("read");
        assert_eq!(read.kind(), io::ErrorKind::TimedOut);
        assert!(read.to_string().contains("approvals.jsonl"), "{read}");
        assert!(records.append(&answered(2)).is_err());

        holder.unlock().expect("unlock");
        assert_eq!(records.read_all().expect("read").len(), 1);
    }
}
