//! Codex's usage-limit message as a plan usage reading. `codex exec` has no plan usage event, so
//! "You've hit your usage limit ... try again at 3:00 PM." is the only sign in the stream.

use std::sync::LazyLock;

use argonauts_core::base::names::AgentId;
use argonauts_core::models::limits::{PlanUsageReading, PlanUsageSource, PlanUsageStatus};
use regex::Regex;
use serde_json::json;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use crate::launch::adapter::ID;
use crate::plan_usage::local_zone::LocalZone;

/// codex-cli 0.158.0 to 0.160.0 print the reset in local time: `%-I:%M %p` when it's today, else
/// `%b %-d<st|nd|rd|th>, %Y %-I:%M %p`.
static RESET_TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)try again at\s+(?:(?P<month>[a-z]{3})\s+(?P<day>\d{1,2})(?:st|nd|rd|th)?,?\s+(?P<year>\d{4})\s+)?(?P<hour>\d{1,2}):(?P<minute>\d{2})\s*(?P<half>[ap]m)",
    )
    .expect("the reset time pattern is valid")
});

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

pub fn usage_limit_reading(
    message: &str,
    now: OffsetDateTime,
    zone: &LocalZone,
) -> PlanUsageReading {
    PlanUsageReading {
        agent_id: AgentId(ID.to_owned()),
        status: PlanUsageStatus::Rejected,
        used: Some(1.0),
        resets_at: reset_time(message, now, zone),
        window: None,
        source: PlanUsageSource::ErrorMessage,
        observed_at: now,
        detail: Some(json!({ "message": message })),
        wrap_up: false,
    }
}

/// `None` when the message says "try again later" or the time doesn't parse.
pub fn reset_time(message: &str, now: OffsetDateTime, zone: &LocalZone) -> Option<OffsetDateTime> {
    let found = RESET_TIME.captures(message)?;
    let number = |name: &str| found.name(name)?.as_str().parse::<u8>().ok();
    let hour = number("hour").filter(|h| (1..=12).contains(h))?;
    let pm = found["half"].eq_ignore_ascii_case("pm");
    let time = Time::from_hms(hour % 12 + if pm { 12 } else { 0 }, number("minute")?, 0).ok()?;
    let date = match found.name("month") {
        Some(month) => {
            let month = month.as_str().to_ascii_lowercase();
            let index = MONTHS.iter().position(|m| *m == month)?;
            let month = Month::try_from(u8::try_from(index + 1).ok()?).ok()?;
            let year = found["year"].parse().ok()?;
            Date::from_calendar_date(year, month, number("day")?).ok()?
        }
        None => return Some(next_at(time, now, zone)),
    };
    Some(local_to_utc(PrimitiveDateTime::new(date, time), zone))
}

/// A bare time is the next time the clock reads it: Codex prints one only for a reset still
/// to come, so a time already gone today is tomorrow's, as when the message is read a moment
/// after midnight or on a clock a little behind Codex's.
fn next_at(time: Time, now: OffsetDateTime, zone: &LocalZone) -> OffsetDateTime {
    let today = now.to_offset(zone.offset_at(now.unix_timestamp())).date();
    let at = local_to_utc(PrimitiveDateTime::new(today, time), zone);
    if at + time::Duration::minutes(1) >= now {
        return at;
    }
    let tomorrow = today.next_day().unwrap_or(today);
    local_to_utc(PrimitiveDateTime::new(tomorrow, time), zone)
}

/// Uses the offset in force at that moment, which is different from today's across a DST change.
fn local_to_utc(local: PrimitiveDateTime, zone: &LocalZone) -> OffsetDateTime {
    let as_utc = local.assume_utc().unix_timestamp();
    let first_guess = zone.offset_at(as_utc);
    let offset = zone.offset_at(as_utc - i64::from(first_guess.whole_seconds()));
    local.assume_offset(offset).to_offset(time::UtcOffset::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const NEW_YORK: &str = "EST5EDT,M3.2.0,M11.1.0";

    fn zone(tz: &str) -> LocalZone {
        LocalZone::posix(tz).unwrap()
    }

    #[test]
    fn same_day_time_is_read_in_local_time() {
        let message = "You’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), \
                       visit https://chatgpt.com/codex/settings/usage to purchase more credits or \
                       try again at 3:05 PM.";
        // 10:10 AM EDT, so 3:05 PM EDT is 19:05 UTC.
        let resets = reset_time(message, datetime!(2026-09-30 14:10 UTC), &zone(NEW_YORK));
        assert_eq!(resets, Some(datetime!(2026-09-30 19:05 UTC)));
        // 9 PM on September 30th in UTC is already October 1st at UTC+8.
        let resets = reset_time(
            message,
            datetime!(2026-09-30 21:00 UTC),
            &LocalZone::fixed(8 * 3600),
        );
        assert_eq!(resets, Some(datetime!(2026-10-01 07:05 UTC)));
    }

    #[test]
    fn a_bare_time_already_gone_is_tomorrows() {
        let utc = LocalZone::fixed(0);
        let message = "You've hit your usage limit. Try again at 11:30 PM.";
        // Read just past midnight: 11:30 PM has gone today, so it's tonight's.
        let resets = reset_time(message, datetime!(2026-10-09 00:05 UTC), &utc);
        assert_eq!(resets, Some(datetime!(2026-10-09 23:30 UTC)));
        let early = "You've hit your usage limit. Try again at 12:15 AM.";
        assert_eq!(
            reset_time(early, datetime!(2026-10-08 20:12 UTC), &utc),
            Some(datetime!(2026-10-09 00:15 UTC))
        );
        // Within a minute of it, the time is still today's, so a reading just at the reset
        // isn't put off a day.
        let now = datetime!(2026-10-08 15:05:40 UTC);
        assert_eq!(
            reset_time("try again at 3:05 PM", now, &utc),
            Some(datetime!(2026-10-08 15:05 UTC))
        );
    }

    #[test]
    fn later_dates_carry_their_own_offset() {
        let now = datetime!(2026-10-30 12:00 UTC);
        let message = "You've hit your usage limit. Try again at Nov 3rd, 2026 12:30 AM.";
        // After the change back to EST on November 1st.
        assert_eq!(
            reset_time(message, now, &zone(NEW_YORK)),
            Some(datetime!(2026-11-03 05:30 UTC))
        );
        let message =
            "You've hit your usage limit for gpt-reserve. Try again at Oct 1st, 2026 12:00 PM.";
        assert_eq!(
            reset_time(message, now, &LocalZone::fixed(0)),
            Some(datetime!(2026-10-01 12:00 UTC))
        );
    }

    #[test]
    fn later_or_odd_times_give_no_reset() {
        let now = datetime!(2026-09-30 14:10 UTC);
        let utc = LocalZone::fixed(0);
        assert_eq!(
            reset_time("You've hit your usage limit. Try again later.", now, &utc),
            None
        );
        assert_eq!(reset_time("try again at 13:00 PM", now, &utc), None);
        assert_eq!(
            reset_time("try again at Foo 3rd, 2026 1:00 PM", now, &utc),
            None
        );
    }
}
