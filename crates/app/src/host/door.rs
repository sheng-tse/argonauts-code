//! Where the socket hands requests while engines come and go: into the host's queue while an
//! engine runs or starts, a wake-up for a host waiting out its park grace, or a refusal once the
//! host exits.

use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::{Notify, mpsc};

use super::relay::{Entry, Queue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EngineState {
    /// An engine runs, or is about to, and requests queue for it.
    Open,
    /// The engine returned `Parked` and the host waits out the grace.
    Parked,
    /// The host is exiting.
    Closed,
}

/// Why the door didn't queue a request.
pub(crate) enum Turned {
    /// The run is parked and this request doesn't wake it.
    Parked(Box<Entry>),
    Closed,
}

pub(crate) struct Door {
    inner: Mutex<Inner>,
    wake: Notify,
}

struct Inner {
    state: EngineState,
    queue: mpsc::UnboundedSender<Entry>,
}

impl Door {
    /// Open, and the queue the host reads.
    pub(crate) fn new() -> (Self, Queue) {
        let (queue, incoming) = mpsc::unbounded_channel();
        let door = Self {
            inner: Mutex::new(Inner {
                state: EngineState::Open,
                queue,
            }),
            wake: Notify::new(),
        };
        (door, Queue::new(incoming))
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> EngineState {
        self.inner().state
    }

    /// Queues `entry`. While parked, only a request that `wakes` gets in, and the first one
    /// wakes the host, so a burst of requests restarts the engine once.
    pub(crate) fn submit(&self, entry: Entry, wakes: bool) -> Result<(), Turned> {
        let mut inner = self.inner();
        match inner.state {
            EngineState::Closed => return Err(Turned::Closed),
            EngineState::Parked if !wakes => return Err(Turned::Parked(Box::new(entry))),
            EngineState::Parked | EngineState::Open => {}
        }
        // The state and the send share the lock, so the host's drain after `park` or `close`
        // sees every request that got in before it.
        inner.queue.send(entry).map_err(|_| Turned::Closed)?;
        if inner.state == EngineState::Parked {
            inner.state = EngineState::Open;
            self.wake.notify_one();
        }
        Ok(())
    }

    pub(crate) fn open(&self) {
        self.inner().state = EngineState::Open;
    }

    pub(crate) fn park(&self) {
        self.inner().state = EngineState::Parked;
    }

    pub(crate) fn close(&self) {
        self.inner().state = EngineState::Closed;
    }

    /// Parked becomes Closed. False when a request woke it first, and then the host restarts.
    pub(crate) fn close_if_parked(&self) -> bool {
        let mut inner = self.inner();
        let parked = inner.state == EngineState::Parked;
        if parked {
            inner.state = EngineState::Closed;
        }
        parked
    }

    pub(crate) async fn woken(&self) {
        self.wake.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use argonauts_core::control::ControlRequest;

    use super::*;

    fn submit(door: &Door, wakes: bool) -> Result<(), Turned> {
        door.submit(Entry::new(ControlRequest::CancelPause).0, wakes)
    }

    #[tokio::test]
    async fn only_the_first_wake_counts() {
        let (door, mut queue) = Door::new();
        door.park();
        assert!(matches!(submit(&door, false), Err(Turned::Parked(_))));
        assert!(submit(&door, true).is_ok());
        assert!(submit(&door, true).is_ok());
        tokio::time::timeout(Duration::from_secs(1), door.woken())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), door.woken())
                .await
                .is_err(),
            "a second request while starting stores no permit"
        );
        assert_eq!(queue.take_all().len(), 2);
        assert!(!door.close_if_parked());
        door.park();
        assert!(door.close_if_parked());
        assert_eq!(door.state(), EngineState::Closed);
        assert!(matches!(submit(&door, true), Err(Turned::Closed)));
    }
}
