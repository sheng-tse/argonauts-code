//! File diffs on a task of their own, so a slow `git diff` never holds up the session, which
//! keeps reading controls, stepping its stop plan, and ticking its watchdogs meanwhile. The
//! same task reads the whole worktree when asked, so a change any call made shows, whatever the
//! call was named.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use argonauts_core::runs::events::session::SessionEvent;
use tokio::sync::{mpsc, oneshot};

use super::outbox::Outbox;
use crate::session::filediff::FileDiffs;

/// How long the end waits for the diffs still owed.
const FLUSH_WAIT: Duration = Duration::from_secs(15);

pub struct Diffs {
    jobs: mpsc::UnboundedSender<Job>,
    reads: Arc<Mutex<WorktreeReads>>,
    /// The reads' change count when last asked.
    changes_seen: u32,
}

/// What reading the worktree found so far.
#[derive(Debug, Default)]
struct WorktreeReads {
    latest: Option<u64>,
    /// Reads that found it different from the read before.
    changes: u32,
}

enum Job {
    Changed(String),
    Flush(oneshot::Sender<()>),
    ReadWorktree,
}

impl Diffs {
    pub fn start(file_diffs: FileDiffs, out: Outbox) -> Self {
        let (jobs, queue) = mpsc::unbounded_channel();
        let reads = Arc::new(Mutex::new(WorktreeReads::default()));
        tokio::spawn(run(file_diffs, out, queue, Arc::clone(&reads)));
        Self {
            jobs,
            reads,
            changes_seen: 0,
        }
    }

    pub fn changed(&self, path: String) {
        // Fails only once the task is gone, and it lives as long as this sender.
        let _ = self.jobs.send(Job::Changed(path));
    }

    /// Reads the whole worktree in the background. The first read is where changes count from.
    pub fn read_worktree(&self) {
        let _ = self.jobs.send(Job::ReadWorktree);
    }

    /// Whether a read found the worktree changed since this was last asked.
    pub fn worktree_changed(&mut self) -> bool {
        let changes = self
            .reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .changes;
        std::mem::replace(&mut self.changes_seen, changes) != changes
    }

    /// Sends the diffs still owed, so the last change of every path goes out before the end.
    pub async fn flush(&self, out: &Outbox) {
        let (done, flushed) = oneshot::channel();
        if self.jobs.send(Job::Flush(done)).is_err() {
            return;
        }
        if tokio::time::timeout(FLUSH_WAIT, flushed).await.is_err() {
            out.notice(format!(
                "the last file diffs took more than {} seconds, so they were left out",
                FLUSH_WAIT.as_secs()
            ));
        }
    }
}

async fn run(
    mut file_diffs: FileDiffs,
    out: Outbox,
    mut queue: mpsc::UnboundedReceiver<Job>,
    reads: Arc<Mutex<WorktreeReads>>,
) {
    let mut failed = false;
    let mut report = |err: std::io::Error| {
        // One failure says it all, since the next diff runs the same git in the same place.
        if !failed {
            failed = true;
            out.notice(format!("could not diff the files the agent changed: {err}"));
        }
    };
    while let Some(job) = queue.recv().await {
        match job {
            Job::Changed(path) => match file_diffs.changed(&path).await {
                Ok(Some(diff)) => out.session(SessionEvent::FileDiff(diff)),
                Ok(None) => {}
                Err(err) => report(err),
            },
            Job::Flush(done) => {
                match file_diffs.flush().await {
                    Ok(diffs) => {
                        for diff in diffs {
                            out.session(SessionEvent::FileDiff(diff));
                        }
                    }
                    Err(err) => report(err),
                }
                let _ = done.send(());
            }
            Job::ReadWorktree => match file_diffs.fingerprint().await {
                Ok(now) => {
                    let mut reads = reads.lock().unwrap_or_else(PoisonError::into_inner);
                    if reads.latest.is_some_and(|before| before != now) {
                        reads.changes += 1;
                    }
                    reads.latest = Some(now);
                }
                Err(err) => report(err),
            },
        }
    }
}
