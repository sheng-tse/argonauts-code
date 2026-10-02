use std::time::{Duration, Instant};

use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub struct RenderCtx<'a> {
    /// Columns to fill. Rendered lines are never wider than this.
    pub width: u16,
    pub theme: &'a Theme,
    /// Draw collapsed groups and long outputs in full, as the transcript overlay does.
    pub expanded: bool,
    /// Drawing a live block, which redraws in place: code rows are cut with `…` rather than
    /// wrapped, so a preview stays short and readable.
    pub live: bool,
    /// Vim keys are on, so letters they take aren't offered as a card's keys.
    pub vim: bool,
    pub clock: Clock,
}

impl<'a> RenderCtx<'a> {
    pub fn new(width: u16, theme: &'a Theme, clock: Clock) -> Self {
        Self {
            width,
            theme,
            expanded: false,
            live: false,
            vim: false,
            clock,
        }
    }

    pub fn with_expanded(self, expanded: bool) -> Self {
        Self { expanded, ..self }
    }

    pub fn with_live(self, live: bool) -> Self {
        Self { live, ..self }
    }

    pub fn with_width(self, width: u16) -> Self {
        Self { width, ..self }
    }

    pub fn with_vim(self, vim: bool) -> Self {
        Self { vim, ..self }
    }
}

/// `start` is when animations count from. History cells print with a still clock, so their
/// printed form never depends on when a frame happened to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub start: Instant,
    pub now: Instant,
}

impl Clock {
    pub fn new(start: Instant, now: Instant) -> Self {
        Self { start, now }
    }

    pub fn still(at: Instant) -> Self {
        Self { start: at, now: at }
    }

    /// Which of `frames` animation frames shows now, for an animation that advances every `period`.
    pub fn frame(&self, period: Duration, frames: usize) -> usize {
        if frames == 0 || period.is_zero() {
            return 0;
        }
        let ticks = self.since(self.start).as_nanos() / period.as_nanos();
        (ticks % frames as u128) as usize
    }

    pub fn next_frame_at(&self, period: Duration) -> Instant {
        if period.is_zero() {
            return self.now;
        }
        let elapsed = self.since(self.start).as_nanos();
        let step = period.as_nanos();
        let next = (elapsed / step + 1) * step;
        self.start + Duration::from_nanos(u64::try_from(next).unwrap_or(u64::MAX))
    }

    /// How long ago `then` was, or zero if it is still ahead.
    pub fn since(&self, then: Instant) -> Duration {
        self.now.saturating_duration_since(then)
    }
}
