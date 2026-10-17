//! When a plan usage window resets, as Claude Code's /usage says it: today's clock, a weekday this
//! week, a date after that.

use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResetStyle {
    /// `Resets 4:40pm`
    #[default]
    Clock,
    /// `Resets in 2h10m`
    Countdown,
    /// `Resets 4:40pm (in 2h10m)`
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClockStyle {
    /// `4:40pm`, and `10pm` on the hour.
    #[default]
    Twelve,
    /// `16:40`
    TwentyFour,
}

impl ClockStyle {
    /// The clock `ui.format.clock` picked.
    pub fn now() -> Self {
        if super::twenty_four_hours() {
            Self::TwentyFour
        } else {
            Self::Twelve
        }
    }
}

/// `Resets 4:40pm`, `Resets Tue 10pm`, `Resets Oct 12`, or a countdown, for a window that
/// resets `at`. Both times are in the zone they're shown in.
pub fn resets(
    at: OffsetDateTime,
    now: OffsetDateTime,
    style: ResetStyle,
    clock: ClockStyle,
) -> String {
    match style {
        ResetStyle::Clock => format!("Resets {}", reset_time(at, now, clock)),
        ResetStyle::Countdown => format!("Resets in {}", countdown(at - now)),
        ResetStyle::Both => format!(
            "Resets {} (in {})",
            reset_time(at, now, clock),
            countdown(at - now)
        ),
    }
}

/// The reset time alone: the clock today, a weekday and the clock within a week, a date later.
pub fn reset_time(at: OffsetDateTime, now: OffsetDateTime, clock: ClockStyle) -> String {
    let at = at.to_offset(now.offset());
    let days = (at.date() - now.date()).whole_days();
    let weekday = &at.weekday().to_string()[..3];
    match days {
        ..=0 => clock_time(at, clock),
        1..=6 => format!("{weekday} {}", clock_time(at, clock)),
        _ => format!("{} {}", &at.month().to_string()[..3], at.day()),
    }
}

fn clock_time(at: OffsetDateTime, clock: ClockStyle) -> String {
    let (hour, minute) = (at.hour(), at.minute());
    match clock {
        ClockStyle::TwentyFour => format!("{hour}:{minute:02}"),
        ClockStyle::Twelve => {
            let suffix = if hour < 12 { "am" } else { "pm" };
            let hour = match hour % 12 {
                0 => 12,
                h => h,
            };
            if minute == 0 {
                format!("{hour}{suffix}")
            } else {
                format!("{hour}:{minute:02}{suffix}")
            }
        }
    }
}

/// Time left, in its two largest units: `3d4h`, `2h10m`, `45m`, and `1m` under a minute.
pub fn countdown(left: Duration) -> String {
    let minutes = left.whole_minutes().max(1);
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-10-04 14:30 -7);

    #[test]
    fn a_reset_reads_by_how_far_off_it_is() {
        let twelve = ClockStyle::Twelve;
        let today = datetime!(2026-10-04 16:40 -7);
        assert_eq!(reset_time(today, NOW, twelve), "4:40pm");
        let tuesday = datetime!(2026-10-06 22:00 -7);
        assert_eq!(reset_time(tuesday, NOW, twelve), "Tue 10pm");
        let later = datetime!(2026-10-12 9:00 -7);
        assert_eq!(reset_time(later, NOW, twelve), "Oct 12");
        assert_eq!(reset_time(today, NOW, ClockStyle::TwentyFour), "16:40");
        assert_eq!(
            reset_time(tuesday, NOW, ClockStyle::TwentyFour),
            "Tue 22:00"
        );
        // A time given in another zone reads in the zone shown.
        let utc = datetime!(2026-10-04 23:40 UTC);
        assert_eq!(reset_time(utc, NOW, twelve), "4:40pm");
    }

    #[test]
    fn every_reset_style_and_clock() {
        let at = datetime!(2026-10-04 16:40 -7);
        let cases = [
            (ResetStyle::Clock, ClockStyle::Twelve, "Resets 4:40pm"),
            (ResetStyle::Clock, ClockStyle::TwentyFour, "Resets 16:40"),
            (ResetStyle::Countdown, ClockStyle::Twelve, "Resets in 2h10m"),
            (
                ResetStyle::Countdown,
                ClockStyle::TwentyFour,
                "Resets in 2h10m",
            ),
            (
                ResetStyle::Both,
                ClockStyle::Twelve,
                "Resets 4:40pm (in 2h10m)",
            ),
            (
                ResetStyle::Both,
                ClockStyle::TwentyFour,
                "Resets 16:40 (in 2h10m)",
            ),
        ];
        for (style, clock, want) in cases {
            assert_eq!(resets(at, NOW, style, clock), want, "{style:?} {clock:?}");
        }
    }

    #[test]
    fn countdowns_keep_two_units() {
        assert_eq!(countdown(Duration::seconds(20)), "1m");
        assert_eq!(countdown(Duration::minutes(45)), "45m");
        assert_eq!(countdown(Duration::minutes(130)), "2h10m");
        assert_eq!(countdown(Duration::hours(76)), "3d4h");
    }
}
