//! File diffs on a task of their own, so a slow `git diff` never holds up the session, which
//! keeps reading controls, stepping its stop plan, and ticking its watchdogs meanwhile.

use std::time::Duration;

use argonauts_core::events::session::SessionEvent;
use tokio::sync::{mpsc, oneshot};

use super::outbox::Outbox;
use crate::filediff::FileDiffs;

/// How long the end waits for the diffs still owed.
const FLUSH_WAIT: Duration = Duration::from_secs(15);

pub struct Diffs {
    jobs: mpsc::UnboundedSender<Job>,
}

enum Job {
    Changed(String),
    Flush(oneshot::Sender<()>),
}

impl Diffs {
    pub fn start(file_diffs: FileDiffs, out: Outbox) -> Self {
        let (jobs, queue) = mpsc::unbounded_channel();
        tokio::spawn(run(file_diffs, out, queue));
        Self { jobs }
    }

    pub fn changed(&self, path: String) {
        // Fails only once the task is gone, and it lives as long as this sender.
        let _ = self.jobs.send(Job::Changed(path));
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

async fn run(mut file_diffs: FileDiffs, out: Outbox, mut queue: mpsc::UnboundedReceiver<Job>) {
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
        }
    }
}
