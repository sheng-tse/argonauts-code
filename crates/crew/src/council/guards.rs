//! When the talk stops without agreement: messages that move nobody, its tokens, its working
//! time.
//!
//! A talk ends by itself once every seat agrees on every point. These stop one that goes round in
//! circles, whichever comes first, so a council never spends without end.

use std::time::Duration;

use argonauts_core::events::council::TalkStop;
use argonauts_core::settings::council::TalkStop as StopAfter;
use argonauts_core::window_size::tokens_by;

/// Messages in a row that move nobody after which a talk stops even with every limit off, so
/// it can't go round in circles forever.
const MOST_UNMOVED: u32 = 20;

/// What the talk may spend before it stops. Zero is no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub messages_that_move_nobody: u32,
    pub tokens: u64,
    pub working_time: Duration,
}

impl Limits {
    /// From the settings, or the limits the talk recorded when it started or you let it talk on.
    pub fn of(settings: &StopAfter, recorded: Option<(u64, u32)>) -> Self {
        let from_settings = match settings.working_time.parse() {
            Ok(Some(span)) => span,
            // "none": no limit, which zero stands for.
            Ok(None) => Duration::ZERO,
            Err(_) => Duration::from_secs(15 * 60),
        };
        let (tokens, working_time) = match recorded {
            Some((tokens, minutes)) => (tokens, Duration::from_secs(u64::from(minutes) * 60)),
            None => (settings.tokens, from_settings),
        };
        let messages_that_move_nobody = match settings.messages_that_move_nobody {
            0 if tokens == 0 && working_time.is_zero() => MOST_UNMOVED,
            messages => messages,
        };
        Self {
            messages_that_move_nobody,
            tokens,
            working_time,
        }
    }

    /// Whole minutes, as the `Limits` event and your screen count them.
    pub fn minutes(&self) -> u32 {
        u32::try_from(self.working_time.as_secs().div_ceil(60)).unwrap_or(u32::MAX)
    }
}

/// What the talk has spent so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spent {
    /// Seat messages in a row that moved nobody's stance.
    pub unmoved: u32,
    /// Every seat's tokens together.
    pub tokens: u64,
    /// The seats' working time together, never time spent waiting on you.
    pub working: Duration,
}

/// How much further you let a stopped talk go: half its limits again, from what it has spent,
/// so it can't go round in circles for as long a second time. A limit that is off stays off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TalkOn {
    pub tokens: u64,
    pub minutes: u32,
}

impl TalkOn {
    pub fn of(settings: &StopAfter) -> Self {
        let set = Limits::of(settings, None);
        Self {
            tokens: set.tokens / 2,
            minutes: set.minutes().div_ceil(2),
        }
    }

    /// The tokens and minutes to record as the talk's limits from now on.
    pub fn limits(&self, spent: &Spent) -> (u64, u32) {
        let worked = u32::try_from(spent.working.as_secs().div_ceil(60)).unwrap_or(u32::MAX);
        let tokens = if self.tokens == 0 {
            0
        } else {
            spent.tokens.saturating_add(self.tokens)
        };
        let minutes = if self.minutes == 0 {
            0
        } else {
            worked.saturating_add(self.minutes)
        };
        (tokens, minutes)
    }

    /// The option that lets the talk go on, as your question offers it.
    pub fn words(&self) -> String {
        match (self.tokens, self.minutes) {
            (0, 0) => "Talk on".to_owned(),
            (0, 1) => "Talk on, up to 1 more minute".to_owned(),
            (0, minutes) => format!("Talk on, up to {minutes} more minutes"),
            (tokens, _) => format!("Talk on, up to {} more tokens", tokens_by(tokens, 1_000)),
        }
    }
}

/// Why the talk stops now, or `None` while it may go on.
pub fn stop(spent: &Spent, limits: &Limits) -> Option<TalkStop> {
    if limits.messages_that_move_nobody > 0 && spent.unmoved >= limits.messages_that_move_nobody {
        return Some(TalkStop::MovedNobody {
            messages: spent.unmoved,
        });
    }
    if limits.tokens > 0 && spent.tokens >= limits.tokens {
        return Some(TalkStop::Tokens {
            used: spent.tokens,
            limit: limits.tokens,
        });
    }
    if !limits.working_time.is_zero() && spent.working >= limits.working_time {
        return Some(TalkStop::Time {
            minutes: u32::try_from(spent.working.as_secs() / 60).unwrap_or(u32::MAX),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::Span;

    /// Every limit off still leaves one: a talk that moves nobody for long stops.
    #[test]
    fn a_talk_with_every_limit_off_still_stops_going_round() {
        let off = StopAfter {
            messages_that_move_nobody: 0,
            tokens: 0,
            working_time: Span::new("none"),
        };
        let limits = Limits::of(&off, None);
        assert_eq!(limits.messages_that_move_nobody, MOST_UNMOVED);
        let one_left = StopAfter {
            tokens: 1_000,
            ..off
        };
        assert_eq!(Limits::of(&one_left, None).messages_that_move_nobody, 0);
    }

    use super::*;

    fn defaults() -> Limits {
        Limits::of(&StopAfter::default(), None)
    }

    #[test]
    fn the_defaults_are_six_messages_200k_tokens_and_15_minutes() {
        let limits = defaults();
        assert_eq!(limits.messages_that_move_nobody, 6);
        assert_eq!(limits.tokens, 200_000);
        assert_eq!(limits.working_time, Duration::from_secs(900));
        assert_eq!(limits.minutes(), 15);
        assert_eq!(stop(&Spent::default(), &limits), None);
    }

    #[test]
    fn each_limit_stops_the_talk_on_its_own() {
        let limits = defaults();
        let unmoved = Spent {
            unmoved: 6,
            ..Spent::default()
        };
        assert_eq!(
            stop(&unmoved, &limits),
            Some(TalkStop::MovedNobody { messages: 6 })
        );
        let five = Spent {
            unmoved: 5,
            tokens: 199_999,
            working: Duration::from_secs(899),
        };
        assert_eq!(stop(&five, &limits), None);
        let tokens = Spent {
            tokens: 200_000,
            ..Spent::default()
        };
        assert_eq!(
            stop(&tokens, &limits),
            Some(TalkStop::Tokens {
                used: 200_000,
                limit: 200_000
            })
        );
        let time = Spent {
            working: Duration::from_secs(16 * 60),
            ..Spent::default()
        };
        assert_eq!(stop(&time, &limits), Some(TalkStop::Time { minutes: 16 }));
    }

    #[test]
    fn talking_on_gives_half_the_limits_again_from_what_was_spent() {
        let on = TalkOn::of(&StopAfter::default());
        assert_eq!(on.words(), "Talk on, up to 100k more tokens");
        let spent = Spent {
            unmoved: 6,
            tokens: 118_000,
            working: Duration::from_secs(6 * 60 + 2),
        };
        assert_eq!(on.limits(&spent), (218_000, 15));

        let minutes_only = StopAfter {
            tokens: 0,
            ..StopAfter::default()
        };
        let on = TalkOn::of(&minutes_only);
        assert_eq!(on.words(), "Talk on, up to 8 more minutes");
        assert_eq!(on.limits(&spent).0, 0, "a limit that is off stays off");
        let none = StopAfter {
            tokens: 0,
            working_time: Span::new("none"),
            ..StopAfter::default()
        };
        assert_eq!(TalkOn::of(&none).words(), "Talk on");
    }

    #[test]
    fn recorded_limits_win_over_the_settings_and_zero_means_no_limit() {
        let raised = Limits::of(&StopAfter::default(), Some((400_000, 30)));
        assert_eq!(raised.tokens, 400_000);
        assert_eq!(raised.working_time, Duration::from_secs(1_800));
        let spent = Spent {
            tokens: 250_000,
            working: Duration::from_secs(20 * 60),
            ..Spent::default()
        };
        assert_eq!(stop(&spent, &raised), None);

        let unlimited = Limits::of(
            &StopAfter {
                messages_that_move_nobody: 0,
                tokens: 0,
                working_time: Span::new("none"),
            },
            None,
        );
        // Only the cap on messages that move nobody is left.
        let a_lot = Spent {
            unmoved: MOST_UNMOVED - 1,
            tokens: u64::MAX,
            working: Duration::from_secs(86_400),
        };
        assert_eq!(stop(&a_lot, &unlimited), None);
    }
}
