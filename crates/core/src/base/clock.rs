//! The time, behind a trait, so timeouts and reminders can be tested without waiting.

use time::OffsetDateTime;

pub trait Clock: Send + Sync {
    /// In UTC.
    fn now(&self) -> OffsetDateTime;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}
