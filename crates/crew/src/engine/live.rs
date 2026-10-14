//! The running sessions' control channels, and the pump that records a session's worker events
//! as run events until it ends.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use argonauts_core::TaskId;
use argonauts_core::events::session::SessionEnd;
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::session_runner::{RunnerEvent, SessionControl};

use crate::engine::host::{Host, crashed_end};
use crate::error::{EngineError, EngineResult};

pub struct LiveSessions {
    by_key: Mutex<BTreeMap<SessionKey, LiveSession>>,
    count: watch::Sender<usize>,
}

impl Default for LiveSessions {
    fn default() -> Self {
        Self {
            by_key: Mutex::new(BTreeMap::new()),
            count: watch::channel(0).0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveSession {
    pub role: RoleName,
    pub task: Option<TaskId>,
    /// Drafting agents, judges, and blind reviewers: a mention delivered to them carries only your
    /// text.
    pub blind: bool,
    pub control: mpsc::Sender<SessionControl>,
    /// Cancels a step session (reading, drafting, judging) through its `StepRequest.cancel`.
    pub cancel: CancellationToken,
}

impl LiveSessions {
    pub fn insert(&self, key: SessionKey, session: LiveSession) {
        let mut sessions = self.lock();
        sessions.insert(key, session);
        self.count.send_replace(sessions.len());
    }

    /// Dropping the last control sender stops a session that is still running.
    pub fn remove(&self, key: &SessionKey) -> Option<LiveSession> {
        let mut sessions = self.lock();
        let removed = sessions.remove(key);
        self.count.send_replace(sessions.len());
        removed
    }

    pub fn get(&self, key: &SessionKey) -> Option<LiveSession> {
        self.lock().get(key).cloned()
    }

    pub fn contains(&self, key: &SessionKey) -> bool {
        self.lock().contains_key(key)
    }

    pub fn keys(&self) -> Vec<SessionKey> {
        self.lock().keys().cloned().collect()
    }

    pub fn all(&self) -> Vec<(SessionKey, LiveSession)> {
        self.lock()
            .iter()
            .map(|(key, session)| (key.clone(), session.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns once no session runs.
    pub async fn wait_empty(&self) {
        let mut count = self.count.subscribe();
        // The sender lives as long as `self`, so this only ends when the count reaches 0.
        let _ = count.wait_for(|count| *count == 0).await;
    }

    /// Returns once `key` no longer runs.
    pub async fn wait_gone(&self, key: &SessionKey) {
        let mut count = self.count.subscribe();
        // Every insert and remove notifies, so each change is a chance to look again.
        while self.contains(key) {
            if count.changed().await.is_err() {
                return;
            }
        }
    }

    /// Tells every session to stop now and cancels its step, leaving it live until it ends.
    pub fn stop_all(&self) {
        for (key, session) in self.all() {
            session.cancel.cancel();
            if let Err(err) = session.control.try_send(SessionControl::Stop) {
                tracing::debug!(session = %key, %err, "stop not sent");
            }
        }
    }

    /// Drops every control sender, which stops whatever still runs.
    pub fn clear(&self) {
        self.lock().clear();
        self.count.send_replace(0);
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<SessionKey, LiveSession>> {
        self.by_key.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Records `events` (text and reasoning deltas merged by the journal buffer) with the session's
/// task, until `ended` fires; then flushes, records `Session::Ended`, and returns the end. A
/// session the run paused ends `Paused` and returns `Interrupted`, so its step resumes it.
pub async fn pump(
    host: &Host,
    key: SessionKey,
    task: Option<TaskId>,
    mut events: mpsc::Receiver<RunnerEvent>,
    ended: oneshot::Receiver<SessionEnd>,
) -> EngineResult<SessionEnd> {
    let source = host.session_source(&key);
    let mut failed = None;
    while let Some(event) = events.recv().await {
        // Keep draining after a failed write, so the worker never blocks on a full channel.
        if failed.is_none()
            && let Err(err) = host.record_worker_event(&source, &key, task.as_ref(), event)
        {
            failed = Some(err);
        }
    }
    let end = ended
        .await
        .unwrap_or_else(|_| crashed_end("the worker ended the session without saying how"));
    let end = host.finish_session(&key, task.as_ref(), end)?;
    if let Some(err) = failed {
        return Err(err);
    }
    if host.cut_short(&end) {
        return Err(EngineError::Interrupted);
    }
    Ok(end)
}
