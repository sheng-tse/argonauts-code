//! Losing autonomy on a revert: an auto-merge of the last 7 days whose merge was reverted on
//! the target branch counts as a wrong auto-approval.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use anyhow::Context as _;
use argonauts_core::RunId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointEvent, CheckpointKind};
use argonauts_core::events::run::Party;
use argonauts_core::journal::read_all;
use argonauts_core::layout::run_files;
use argonauts_core::names::CheckpointId;
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::policy::{ApprovalPolicy, DecisionPoint};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::git::git;

/// How far back an auto-merge still counts. After that its shadow match stands.
const WINDOW: Duration = Duration::days(7);

/// The reverts already counted, in the repo's state directory, so each counts once however many
/// supervisors start in the window.
const COUNTED: &str = "reverts-counted.json";

/// Calls `policy.spot_checked(point, false)` once for each reverted auto-merge, and returns one
/// notice line per run it counted this time.
pub(crate) fn check_reverts(
    repo_root: &Path,
    state_dir: &Path,
    policy: &dyn ApprovalPolicy,
    now: OffsetDateTime,
) -> anyhow::Result<Vec<String>> {
    let runs_dir = state_dir.join(argonauts_core::layout::repo_state::RUNS);
    let entries = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", runs_dir.display())),
    };
    // Held until the check is done, so two supervisors starting at once count a revert once.
    let mut ledger = Ledger::open(state_dir)?;
    ledger
        .counted
        .retain(|counted| now.unix_timestamp() - counted.landed <= WINDOW.whole_seconds());
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let journal = entry.path().join(run_files::EVENTS);
        // A landing is written to the journal, so one last written before the window has none
        // inside it, and isn't worth reading.
        let written = std::fs::metadata(&journal).and_then(|meta| meta.modified());
        match written {
            Ok(written) if now - OffsetDateTime::from(written) <= WINDOW => {}
            Ok(_) => continue,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                tracing::warn!("checking {} for reverts: {err}", journal.display());
                continue;
            }
        }
        let events = match read_all(&journal) {
            Ok(back) => back.events,
            Err(err) => {
                tracing::warn!("checking {} for reverts: {err}", journal.display());
                continue;
            }
        };
        let Some(merge) = auto_merge(&events, now) else {
            continue;
        };
        if ledger.has(&merge) {
            continue;
        }
        let pattern = format!("This reverts commit {}", merge.sha);
        let searched = git(
            repo_root,
            &[
                "log",
                &merge.target,
                "--fixed-strings",
                "--grep",
                &pattern,
                "--format=%H",
                "-n",
                "1",
            ],
        );
        let revert = match searched {
            Ok(revert) => revert,
            Err(err) => {
                tracing::warn!("checking run {} for a revert: {err:#}", merge.run);
                continue;
            }
        };
        if revert.is_empty() {
            continue;
        }
        found.push((merge, revert));
    }
    if found.is_empty() {
        return Ok(Vec::new());
    }
    // Written before the policy hears of them, so a failed write can't count one twice.
    ledger
        .counted
        .extend(found.iter().map(|(merge, _)| Counted {
            run: merge.run.clone(),
            sha: merge.sha.clone(),
            landed: merge.landed.unix_timestamp(),
        }));
    ledger.save()?;
    let mut notices = Vec::new();
    for (merge, revert) in found {
        policy.spot_checked(
            &DecisionPoint {
                run: merge.run.clone(),
                checkpoint: merge.checkpoint,
                kind: CheckpointKind::Merge,
                recipe: None,
                authors: Vec::new(),
                mode_override: None,
                lead: None,
            },
            false,
        );
        notices.push(format!(
            "The auto-merge of {} was reverted on {} ({}), so it counts as a wrong auto-approval. \
             Merges are back in shadow mode: they wait for you until you turn auto on again in \
             /approvals.",
            merge.run,
            merge.target,
            short(&revert),
        ));
    }
    Ok(notices)
}

struct AutoMerge {
    run: RunId,
    checkpoint: CheckpointId,
    target: String,
    sha: String,
    landed: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Counted {
    run: RunId,
    sha: String,
    /// Unix seconds, so an entry can go once it falls out of the window.
    landed: i64,
}

/// `reverts-counted.json`, open and locked.
struct Ledger {
    file: File,
    path: std::path::PathBuf,
    counted: Vec<Counted>,
}

impl Ledger {
    fn open(state_dir: &Path) -> anyhow::Result<Self> {
        create_private_dir_all(state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        let path = state_dir.join(COUNTED);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        file.lock()
            .with_context(|| format!("locking {}", path.display()))?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .with_context(|| format!("reading {}", path.display()))?;
        // A torn file reads as empty. Its reverts may then count once more, never less.
        let counted = serde_json::from_str(&text).unwrap_or_default();
        Ok(Self {
            file,
            path,
            counted,
        })
    }

    fn has(&self, merge: &AutoMerge) -> bool {
        self.counted
            .iter()
            .any(|counted| counted.run == merge.run && counted.sha == merge.sha)
    }

    fn save(&mut self) -> anyhow::Result<()> {
        let mut text = serde_json::to_vec_pretty(&self.counted)?;
        text.push(b'\n');
        self.file
            .set_len(0)
            .and_then(|()| self.file.seek(SeekFrom::Start(0)))
            .and_then(|_| self.file.write_all(&text))
            .with_context(|| format!("writing {}", self.path.display()))
    }
}

/// The run's landing within the window, when your rules or an approver model approved its merge.
fn auto_merge(events: &[RunEvent], now: OffsetDateTime) -> Option<AutoMerge> {
    let mut kinds: HashMap<&CheckpointId, CheckpointKind> = HashMap::new();
    let mut approved: Option<&CheckpointId> = None;
    let mut landed = None;
    for event in events {
        match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                kinds.insert(&opened.id, opened.kind);
            }
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                let merge = kinds.get(&decided.id) == Some(&CheckpointKind::Merge)
                    || decided.action == AnswerAction::Merge;
                if merge {
                    let automatic = matches!(decided.by, Party::Rules | Party::Approver { .. });
                    approved = automatic.then_some(&decided.id);
                }
            }
            RunEventKind::Integration(IntegrationEvent::Landed { target, sha, .. }) => {
                landed = Some((event, target, sha));
            }
            _ => {}
        }
    }
    let (event, target, sha) = landed?;
    let checkpoint = approved?;
    if now - event.ts > WINDOW {
        return None;
    }
    Some(AutoMerge {
        run: event.run_id.clone(),
        checkpoint: checkpoint.clone(),
        target: target.clone(),
        sha: sha.clone(),
        landed: event.ts,
    })
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::checkpoint::CheckpointDecided;
    use argonauts_core::names::WorkerRef;
    use argonauts_testkit::TempRepo;
    use argonauts_testkit::services::AskingPolicy;

    use super::*;

    fn event(run: &RunId, seq: u64, ts: OffsetDateTime, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts,
            run_id: run.clone(),
            card_id: None,
            session: None,
            source: "conductor".to_owned(),
            kind,
        }
    }

    fn write_run(
        runs_dir: &Path,
        run: &str,
        by: Party,
        sha: &str,
        landed: OffsetDateTime,
    ) -> RunId {
        let run = RunId::new(run).unwrap();
        let decided = RunEventKind::Checkpoint(CheckpointEvent::Decided(CheckpointDecided {
            id: CheckpointId::numbered(4),
            action: AnswerAction::Merge,
            text: None,
            by,
            evidence_sha256: "e".repeat(64),
            shadow: None,
            cards: Vec::new(),
        }));
        let landing = RunEventKind::Integration(IntegrationEvent::Landed {
            target: "main".to_owned(),
            sha: sha.to_owned(),
            fast_forward: true,
        });
        let dir = runs_dir.join(run.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = String::new();
        for (seq, kind) in [decided, landing].into_iter().enumerate() {
            let line = serde_json::to_string(&event(&run, seq as u64, landed, kind)).unwrap();
            lines.push_str(&line);
            lines.push('\n');
        }
        std::fs::write(dir.join(run_files::EVENTS), lines).unwrap();
        run
    }

    #[test]
    fn a_reverted_auto_merge_is_spot_checked_as_wrong() {
        let repo = TempRepo::new().unwrap();
        repo.write("a.txt", "one\n").unwrap();
        let reverted = repo.commit_all("add a").unwrap();
        repo.write("b.txt", "two\n").unwrap();
        let kept = repo.commit_all("add b").unwrap();
        repo.git(&["revert", "--no-edit", &reverted]).unwrap();

        let state = tempfile::tempdir().unwrap();
        let runs = state.path().join(argonauts_core::layout::repo_state::RUNS);
        let now = OffsetDateTime::now_utc();
        let hit = write_run(
            &runs,
            "run-20260930-010000-0001",
            Party::Rules,
            &reverted,
            now,
        );
        write_run(&runs, "run-20260930-010000-0002", Party::Rules, &kept, now);
        write_run(
            &runs,
            "run-20260930-010000-0003",
            Party::User,
            &reverted,
            now,
        );
        write_run(
            &runs,
            "run-20260930-010000-0004",
            Party::Approver {
                model: WorkerRef::new("codex", None),
            },
            &reverted,
            now - Duration::days(8),
        );

        let policy = AskingPolicy::default();
        let notices = check_reverts(repo.root(), state.path(), &policy, now).unwrap();
        let checks = policy.spot_checks();
        assert_eq!(checks.len(), 1, "{checks:?}");
        let (point, correct) = &checks[0];
        assert!(!correct);
        assert_eq!(point.run, hit);
        assert_eq!(point.checkpoint, CheckpointId::numbered(4));
        assert_eq!(point.kind, CheckpointKind::Merge);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains(hit.as_str()), "{}", notices[0]);

        let again = check_reverts(repo.root(), state.path(), &policy, now).unwrap();
        assert!(again.is_empty(), "a revert counts once: {again:?}");
        assert_eq!(policy.spot_checks().len(), 1);
    }

    #[test]
    fn a_journal_last_written_before_the_window_is_skipped() {
        let repo = TempRepo::new().unwrap();
        repo.write("a.txt", "one\n").unwrap();
        let reverted = repo.commit_all("add a").unwrap();
        repo.git(&["revert", "--no-edit", &reverted]).unwrap();
        let state = tempfile::tempdir().unwrap();
        let runs = state.path().join(argonauts_core::layout::repo_state::RUNS);
        let now = OffsetDateTime::now_utc();
        let run = write_run(
            &runs,
            "run-20260930-010000-0001",
            Party::Rules,
            &reverted,
            now,
        );
        let journal = runs.join(run.as_str()).join(run_files::EVENTS);
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&journal)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        let policy = AskingPolicy::default();
        let notices = check_reverts(repo.root(), state.path(), &policy, now).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        assert!(policy.spot_checks().is_empty());
    }

    #[test]
    fn no_runs_means_no_checks() {
        let repo = TempRepo::new().unwrap();
        let policy = AskingPolicy::default();
        let missing = repo.scratch().join("no-runs");
        assert!(
            check_reverts(repo.root(), &missing, &policy, OffsetDateTime::now_utc())
                .unwrap()
                .is_empty()
        );
    }
}
