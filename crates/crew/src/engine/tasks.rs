//! Tasks that record beside the loop, counted so the loop can wait for them before it parks
//! or finishes.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Clone)]
pub struct Tasks {
    running: Arc<watch::Sender<usize>>,
}

impl Default for Tasks {
    fn default() -> Self {
        Self {
            running: Arc::new(watch::Sender::new(0)),
        }
    }
}

impl Tasks {
    /// Spawns `task`, counted until it returns, panics, or is aborted.
    pub fn spawn<F>(&self, task: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let counted = Counted::new(self.running.clone());
        tokio::spawn(async move {
            let _counted = counted;
            task.await
        })
    }

    pub fn len(&self) -> usize {
        *self.running.borrow()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns once no task runs.
    pub async fn wait(&self) {
        let mut running = self.running.subscribe();
        // The sender lives as long as `self`, so this only ends when the count reaches 0.
        let _ = running.wait_for(|running| *running == 0).await;
    }
}

/// Counts one task from its spawn until it's dropped.
struct Counted(Arc<watch::Sender<usize>>);

impl Counted {
    fn new(running: Arc<watch::Sender<usize>>) -> Self {
        running.send_modify(|running| *running += 1);
        Self(running)
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.0
            .send_modify(|running| *running = running.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    #[tokio::test]
    async fn waits_for_finished_and_aborted_tasks() {
        let tasks = Tasks::default();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let done = tasks.spawn(async move {
            let _ = released.await;
            7
        });
        let stuck = tasks.spawn(std::future::pending::<()>());
        assert_eq!(tasks.len(), 2);
        let _ = release.send(());
        assert_eq!(done.await.ok(), Some(7));
        stuck.abort();
        let waited = tokio::time::timeout(Duration::from_secs(5), tasks.wait()).await;
        assert!(waited.is_ok(), "an aborted task stops counting");
        assert!(tasks.is_empty());
    }
}
