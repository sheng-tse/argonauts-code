//! The same idea as Codex's `frame_requester.rs` and `frame_rate_limiter.rs` (Apache-2.0),
//! without a task: the event loop asks for the next deadline and sleeps until then.

use std::time::{Duration, Instant};

/// About 120 frames a second.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_micros(8_333);

#[derive(Debug, Clone, Default)]
pub struct FrameScheduler {
    last_frame: Option<Instant>,
    wanted: Option<Instant>,
}

impl FrameScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks for a frame at `at` or later. Earlier requests win.
    pub fn request(&mut self, at: Instant) {
        self.wanted = Some(self.wanted.map_or(at, |wanted| wanted.min(at)));
    }

    pub fn deadline(&self) -> Option<Instant> {
        let wanted = self.wanted?;
        let earliest = self.last_frame.map(|last| last + MIN_FRAME_INTERVAL);
        Some(earliest.map_or(wanted, |earliest| wanted.max(earliest)))
    }

    pub fn is_due(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| deadline <= now)
    }

    /// Frames stay [`MIN_FRAME_INTERVAL`] apart whether or not one was requested.
    pub fn earliest(&self, now: Instant) -> Instant {
        self.last_frame
            .map_or(now, |last| (last + MIN_FRAME_INTERVAL).max(now))
    }

    pub fn frame_ran(&mut self, now: Instant) {
        self.last_frame = Some(now);
        self.wanted = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_coalesce_and_frames_stay_apart() {
        let t0 = Instant::now();
        let mut frames = FrameScheduler::new();
        assert_eq!(frames.deadline(), None);

        frames.request(t0 + Duration::from_millis(50));
        frames.request(t0);
        assert_eq!(frames.deadline(), Some(t0));
        assert!(frames.is_due(t0));

        frames.frame_ran(t0);
        assert_eq!(frames.deadline(), None);
        frames.request(t0 + Duration::from_millis(1));
        assert_eq!(frames.deadline(), Some(t0 + MIN_FRAME_INTERVAL));
        assert!(!frames.is_due(t0 + Duration::from_millis(5)));
        assert_eq!(frames.earliest(t0), t0 + MIN_FRAME_INTERVAL);
        assert_eq!(
            frames.earliest(t0 + Duration::from_secs(1)),
            t0 + Duration::from_secs(1)
        );
    }
}
