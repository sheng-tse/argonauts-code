//! File locks that are waited for only so long. A stopped supervisor or a hung network home can
//! hold one forever, and the engine must not wait with it.

use std::fs::{File, TryLockError};
use std::io;
use std::thread;
use std::time::{Duration, Instant};

/// Long enough for another supervisor's read and write, however many are running.
pub const RECORDS_WAIT: Duration = Duration::from_secs(2);
/// Shorter than [`RECORDS_WAIT`], since a reservation waits for the ledger while it holds the
/// records.
pub const LEDGER_WAIT: Duration = Duration::from_secs(1);

const LONGEST_PAUSE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Shared,
    Exclusive,
}

/// Fails with `TimedOut` once `wait` runs out, so the caller fails closed.
pub fn lock_within(file: &File, access: Access, wait: Duration) -> io::Result<()> {
    let deadline = Instant::now() + wait;
    let mut pause = Duration::from_millis(1);
    loop {
        let tried = match access {
            Access::Shared => file.try_lock_shared(),
            Access::Exclusive => file.try_lock(),
        };
        match tried {
            Ok(()) => return Ok(()),
            Err(TryLockError::Error(err)) => return Err(err),
            Err(TryLockError::WouldBlock) => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("another process kept it locked for over {wait:?}"),
            ));
        }
        thread::sleep(pause.min(left));
        pause = (pause * 2).min(LONGEST_PAUSE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_times_out_and_a_free_one_is_taken() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("locked");
        let holder = File::create(&path).expect("create");
        holder.lock().expect("hold");

        let waiter = File::open(&path).expect("open");
        let started = Instant::now();
        let err = lock_within(&waiter, Access::Shared, Duration::from_millis(50))
            .expect_err("held elsewhere");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));

        holder.unlock().expect("unlock");
        lock_within(&waiter, Access::Exclusive, Duration::from_millis(50)).expect("free");
    }
}
