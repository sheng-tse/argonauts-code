//! The one clock that steps all of the world's motion, and the rules for when it may wake the
//! app: only while the world's cells were in the last frame and something in them moves, motion
//! is allowed, the window has focus, a key or the pointer moved in the last two minutes, and Low
//! Power Mode is off. On battery it steps half as often. Focus, input and power each come
//! through a small trait, so every rule can be checked with a fake.

use std::time::{Duration, Instant};

use crate::base::draw::render::Clock;

/// The world steps 8 times a second.
pub const STEP: Duration = Duration::from_millis(125);
/// With no key or pointer move for this long, the world holds still.
pub const IDLE_AFTER: Duration = Duration::from_secs(120);

/// Whether the terminal's window has focus. One that never says so counts as focused.
pub trait FocusSource: Send {
    fn focused(&self) -> bool;

    /// The terminal said the window gained or lost focus.
    fn set_focused(&mut self, focused: bool) {
        let _ = focused;
    }
}

/// When the last key or pointer move came.
pub trait IdleClock: Send {
    fn last_input(&self) -> Instant;

    /// A key or a pointer move came at `at`.
    fn input(&mut self, at: Instant) {
        let _ = at;
    }
}

/// How the computer is powered right now.
pub trait PowerSource: Send {
    fn power(&self, now: Instant) -> Power;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Power {
    /// Low Power Mode, or the system's own power saver, is on.
    pub low_power: bool,
    pub on_battery: bool,
}

/// Why the world holds still.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Still {
    /// Its cells weren't in the last frame.
    Hidden,
    /// Nothing in its scene moves.
    Unmoving,
    /// Motion is set to reduced or off.
    Reduced,
    /// The terminal takes only whole pictures, which motion would send again and again.
    WholePictures,
    Unfocused,
    /// No key or pointer move came for [`IDLE_AFTER`].
    Idle,
    LowPower,
}

/// What the screen knows of the world for its clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Showing {
    /// Its cells were in the last frame.
    pub shown: bool,
    /// Something in its last scene moves.
    pub moves: bool,
    /// Motion is allowed.
    pub motion: bool,
    /// The terminal keeps pictures, so a step sends only what changed.
    pub kept_pictures: bool,
}

/// Focus, input and power, read together.
pub struct Sources {
    pub focus: Box<dyn FocusSource>,
    pub idle: Box<dyn IdleClock>,
    pub power: Box<dyn PowerSource>,
}

impl std::fmt::Debug for Sources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sources")
            .field("focused", &self.focus.focused())
            .field("last_input", &self.idle.last_input())
            .finish_non_exhaustive()
    }
}

impl Sources {
    /// The real ones: focus as the terminal reports it, input as it arrives from `started`, and
    /// the system's power.
    pub fn new(started: Instant) -> Self {
        Self {
            focus: Box::new(ReportedFocus::default()),
            idle: Box::new(InputClock::new(started)),
            power: Box::new(super::power::SystemPower::default()),
        }
    }

    /// Why the world holds still at `now`, or `None` while it may move.
    pub fn still(&self, showing: Showing, now: Instant) -> Option<Still> {
        if !showing.shown {
            return Some(Still::Hidden);
        }
        if !showing.moves {
            return Some(Still::Unmoving);
        }
        if !showing.motion {
            return Some(Still::Reduced);
        }
        if !showing.kept_pictures {
            return Some(Still::WholePictures);
        }
        if !self.focus.focused() {
            return Some(Still::Unfocused);
        }
        if now.saturating_duration_since(self.idle.last_input()) >= IDLE_AFTER {
            return Some(Still::Idle);
        }
        if self.power.power(now).low_power {
            return Some(Still::LowPower);
        }
        None
    }

    /// How long a step lasts now: twice as long on battery.
    pub fn step(&self, now: Instant) -> Duration {
        if self.power.power(now).on_battery {
            STEP * 2
        } else {
            STEP
        }
    }
}

/// The world's steps, counted from when it first showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Steps {
    /// When step `from` began.
    origin: Instant,
    from: u128,
    /// How long a step lasted when they were last counted.
    length: Option<Duration>,
}

impl Steps {
    /// Steps counted from `origin`.
    pub fn new(origin: Instant) -> Self {
        Self {
            origin,
            from: 0,
            length: None,
        }
    }

    /// Which step `now` falls in, `step` long each. When the length changes, as when the
    /// computer goes onto battery and the steps slow to half, the count goes on from the step it
    /// had reached rather than jumping, so nothing moving skips to another place in its loop.
    pub fn index(&mut self, now: Instant, step: Duration) -> u128 {
        if let Some(length) = self.length
            && length != step
        {
            self.from = self.counted(now, length);
            self.origin = now;
        }
        self.length = Some(step);
        self.counted(now, step)
    }

    /// When the step after the one `now` falls in begins.
    pub fn next(&self, now: Instant, step: Duration) -> Instant {
        match self.length {
            // The next count starts the step it reached over at the new length.
            Some(length) if length != step => now + step,
            _ => Clock::new(self.origin, now).next_frame_at(step),
        }
    }

    fn counted(&self, now: Instant, step: Duration) -> u128 {
        let step = step.as_nanos().max(1);
        self.from + now.saturating_duration_since(self.origin).as_nanos() / step
    }
}

/// Focus as the terminal reports it, focused until it says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportedFocus {
    focused: bool,
}

impl Default for ReportedFocus {
    fn default() -> Self {
        Self { focused: true }
    }
}

impl FocusSource for ReportedFocus {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

/// Input as it arrives, counting from a start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputClock {
    last: Instant,
}

impl InputClock {
    pub fn new(started: Instant) -> Self {
        Self { last: started }
    }
}

impl IdleClock for InputClock {
    fn last_input(&self) -> Instant {
        self.last
    }

    fn input(&mut self, at: Instant) {
        self.last = self.last.max(at);
    }
}

#[cfg(test)]
mod tests;
