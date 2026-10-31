//! The time, behind a trait, so timeouts and reminders can be tested without waiting.
//!
//! `ARGONAUTS_CLOCK=<RFC 3339 instant>` starts the app's time at that instant, read once as the
//! process starts, and it runs on from there. The approved drawings' runner sets it, so a screen
//! that writes a time or a day draws the same one on every run. Each process that inherits the
//! variable reads it at its own start, a second or so apart, which no drawn minute shows. Times
//! kept as `SystemTime`, which files' times are compared with, stay the system's.

use std::sync::OnceLock;

use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

pub const CLOCK_ENV: &str = "ARGONAUTS_CLOCK";

pub trait Clock: Send + Sync {
    /// In UTC.
    fn now(&self) -> OffsetDateTime;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        now_utc()
    }
}

/// How far `ARGONAUTS_CLOCK` moved the app's time from the system's.
static SHIFT: OnceLock<Duration> = OnceLock::new();

fn shift() -> Duration {
    SHIFT.get().copied().unwrap_or(Duration::ZERO)
}

/// The app's time now, in UTC.
pub fn now_utc() -> OffsetDateTime {
    OffsetDateTime::now_utc() + shift()
}

/// The system's time at `at`, an instant of the app's time. The daily limits count real days
/// with it, so a clock set for the drawings can't move a limit.
pub fn real(at: OffsetDateTime) -> OffsetDateTime {
    at - shift()
}

#[derive(Debug, thiserror::Error)]
#[error("{CLOCK_ENV}={value} isn't an RFC 3339 instant such as 2026-10-20T08:40:00-04:00")]
pub struct BadClock {
    value: String,
}

/// Starts the app's time at the instant `ARGONAUTS_CLOCK` names, when it names one. Called once,
/// as the process starts and before any time is read; a later call changes nothing.
pub fn start_from_env() -> Result<(), BadClock> {
    let Some(value) = std::env::var(CLOCK_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let start = OffsetDateTime::parse(value.trim(), &Rfc3339).map_err(|_| BadClock { value })?;
    let _ = SHIFT.set(start - OffsetDateTime::now_utc());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_clock_set_the_time_is_the_systems() {
        let before = OffsetDateTime::now_utc();
        let now = now_utc();
        assert!(now >= before && now - before < Duration::seconds(5));
        assert_eq!(real(now), now);
    }
}
