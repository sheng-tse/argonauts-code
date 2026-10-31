//! The stop request and time limits a caller holds over a run.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

/// Asks a running pipeline to stop. Clones share one flag. A stop interrupts the agent, kills a
/// running check, and ends the run as cancelled with the work so far committed.
#[derive(Debug, Clone)]
pub struct StopHandle {
    flag: Arc<watch::Sender<bool>>,
}

impl Default for StopHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl StopHandle {
    pub fn new() -> Self {
        let (flag, _) = watch::channel(false);
        Self {
            flag: Arc::new(flag),
        }
    }

    pub fn stop(&self) {
        self.flag.send_replace(true);
    }

    pub fn is_stopped(&self) -> bool {
        *self.flag.borrow()
    }

    /// Resolves once a stop is requested. Cancel safe.
    pub async fn stopped(&self) {
        let mut rx = self.flag.subscribe();
        if rx.wait_for(|stopped| *stopped).await.is_err() {
            // Unreachable while `self` holds the sender, but never report a stop that didn't
            // happen.
            std::future::pending::<()>().await;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConductorSettings {
    /// `None` means no limit.
    pub session_timeout: Option<Duration>,
    pub check_timeout: Duration,
    /// How long to keep reading after the agent exits, in case something it started still holds
    /// its output open. Whatever remains in its process group is then killed.
    pub exit_grace: Duration,
    pub git_env: Vec<(String, String)>,
}

impl Default for ConductorSettings {
    fn default() -> Self {
        Self {
            session_timeout: None,
            check_timeout: Duration::from_secs(30 * 60),
            exit_grace: Duration::from_secs(2),
            git_env: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_is_shared_and_sticky() {
        let handle = StopHandle::new();
        let clone = handle.clone();
        assert!(!clone.is_stopped());
        let waiter = tokio::spawn({
            let handle = handle.clone();
            async move { handle.stopped().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        clone.stop();
        waiter.await.unwrap();
        assert!(handle.is_stopped());
        tokio::time::timeout(Duration::from_millis(100), handle.stopped())
            .await
            .unwrap();
    }

    #[test]
    fn default_settings_are_generous() {
        let settings = ConductorSettings::default();
        assert_eq!(settings.session_timeout, None);
        assert_eq!(settings.check_timeout, Duration::from_secs(1800));
        assert!(settings.git_env.is_empty());
    }
}
