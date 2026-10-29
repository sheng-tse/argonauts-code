use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use argonauts_core::Spend;
use argonauts_core::app::ModelChoice;
use argonauts_core::display_names::DisplayNames;
use time::{OffsetDateTime, UtcOffset};

mod clock;
pub mod figures;
mod numbers;
mod resets;

pub use clock::{on_24_hours, set_clock, twenty_four_hours};
pub use figures::Figures;
pub use numbers::{
    Formats, PlanUsageWords, TokenUnit, UNKNOWN, duration_slot, multiplier, percent, plan_usage,
    stale, tokens, tokens_as, tokens_as_claude_code, tokens_beside, tokens_in_tenths,
    tokens_of_window, window,
};
pub use resets::{ClockStyle, ResetStyle, countdown, reset_time, resets};

/// Elapsed time with spaces: `3s`, `9m 50s`, `2h 53m 6s`.
pub fn duration(elapsed: Duration) -> String {
    let (hours, minutes, seconds) = split(elapsed);
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Elapsed time without spaces, for the agent panel: `45s`, `2m29s`, `1h4m`.
pub fn duration_compact(elapsed: Duration) -> String {
    let (hours, minutes, seconds) = split(elapsed);
    if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// A cost in US dollars, its thousands marked: `$0.21`, `$12.40`, `$12,356.88`.
pub fn usd(cost: f64) -> String {
    let plain = format!("{cost:.2}");
    let dollars = plain.split_once('.').and_then(|(whole, cents)| {
        let whole: u64 = whole.parse().ok()?;
        Some(format!("{}.{cents}", thousands(whole)))
    });
    format!("${}", dollars.unwrap_or(plain))
}

/// A count with its thousands marked: `1,086`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// What was spent, marked the same on every screen: `~$0.24` when estimated from token counts,
/// `$0.24+` when some usage had no price, and `<$0.01` for less than would show as a cent, which
/// says on its own that it isn't exact.
pub fn spend(spent: &Spend) -> String {
    let partial = if spent.cost_partial { "+" } else { "" };
    if spent.cost_usd > 0.0 && spent.cost_usd < UNDER_A_CENT {
        return format!("<$0.01{partial}");
    }
    // A free model, such as one on your own server, is priced at exactly nothing.
    let free = spent.cost_usd == 0.0 && !spent.cost_partial;
    let estimated = if spent.cost_estimated && !free {
        "~"
    } else {
        ""
    };
    format!("{estimated}{}{partial}", usd(spent.cost_usd))
}

/// Token counts as Claude Code's `/context` writes them: the count itself under a thousand,
/// then one decimal with `.0` dropped, `4.4k`, `182.5k`, `33k`, and `1m` from 999,950 on.
pub fn context_tokens(count: u64) -> String {
    if count < 1_000 {
        return count.to_string();
    }
    let (value, unit) = if count < 999_950 {
        (count as f64 / 1_000.0, "k")
    } else {
        (count as f64 / 1_000_000.0, "m")
    };
    let digits = format!("{value:.1}");
    let trimmed = digits.strip_suffix(".0").unwrap_or(&digits);
    format!("{trimmed}{unit}")
}

/// What rounds to `$0.00`, though something was spent.
const UNDER_A_CENT: f64 = 0.005;

fn split(elapsed: Duration) -> (u64, u64, u64) {
    let secs = elapsed.as_secs();
    (secs / 3600, secs / 60 % 60, secs % 60)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClockTime {
    hour: u8,
    minute: u8,
}

impl ClockTime {
    /// A time with `hour` in `0..24` and `minute` in `0..60`.
    pub fn new(hour: u8, minute: u8) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self { hour, minute })
    }

    pub fn hour(&self) -> u8 {
        self.hour
    }

    pub fn minute(&self) -> u8 {
        self.minute
    }
}

impl fmt::Display for ClockTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(time) = on_24_hours(self.hour, self.minute) {
            return f.write_str(&time);
        }
        let suffix = if self.hour < 12 { "AM" } else { "PM" };
        let hour = match self.hour % 12 {
            0 => 12,
            h => h,
        };
        write!(f, "{hour}:{:02} {suffix}", self.minute)
    }
}

impl From<OffsetDateTime> for ClockTime {
    fn from(at: OffsetDateTime) -> Self {
        Self {
            hour: at.hour(),
            minute: at.minute(),
        }
    }
}

/// Wall-clock times on screen, such as `done 10:58 AM`, in the local zone. Tests pin the zone
/// and the time so snapshots don't depend on the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallClock {
    pinned: Option<OffsetDateTime>,
}

impl WallClock {
    pub fn local() -> Self {
        Self { pinned: None }
    }

    /// Always `now`, and times shown in `now`'s offset.
    pub fn pinned(now: OffsetDateTime) -> Self {
        Self { pinned: Some(now) }
    }

    pub fn now(&self) -> OffsetDateTime {
        match self.pinned {
            Some(now) => now,
            None => self.to_local(OffsetDateTime::now_utc()),
        }
    }

    /// `at` in the zone this clock shows.
    pub fn to_local(&self, at: OffsetDateTime) -> OffsetDateTime {
        let offset = match self.pinned {
            Some(now) => now.offset(),
            None => local_offset_at(at).unwrap_or(UtcOffset::UTC),
        };
        at.to_offset(offset)
    }

    pub fn clock(&self, at: OffsetDateTime) -> ClockTime {
        ClockTime::from(self.to_local(at))
    }

    /// `at` to the minute as a promise reads it, rounded up so it never says a try comes
    /// earlier than it will.
    pub fn clock_rounded_up(&self, at: OffsetDateTime) -> ClockTime {
        let into_minute = std::time::Duration::from_secs(u64::from(at.second()))
            + std::time::Duration::from_nanos(u64::from(at.nanosecond()));
        let at = if into_minute.is_zero() {
            at
        } else {
            at + (std::time::Duration::from_secs(60) - into_minute)
        };
        self.clock(at)
    }

    pub fn clock_now(&self) -> ClockTime {
        ClockTime::from(self.now())
    }
}

impl Default for WallClock {
    fn default() -> Self {
        Self::local()
    }
}

/// The `time` crate won't read the zone while other threads may run, so this asks libc, whose
/// `localtime_r` is reentrant. The zone can change at a daylight saving switch, so it's read
/// for each time shown.
fn local_offset_at(at: OffsetDateTime) -> Option<UtcOffset> {
    use nix::libc;

    // libc deprecates musl's `time_t` because its width will change on 32-bit targets, and
    // `try_from` checks the width whatever it is.
    #[allow(deprecated)]
    let seconds = libc::time_t::try_from(at.unix_timestamp()).ok()?;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `localtime_r` only reads `seconds` and writes the `tm` it is given, which lives
    // for the call. It returns null on failure, and `tm` is read only after it succeeded.
    let tm = unsafe {
        if libc::localtime_r(&seconds, tm.as_mut_ptr()).is_null() {
            return None;
        }
        tm.assume_init()
    };
    let offset = i32::try_from(tm.tm_gmtoff).ok()?;
    UtcOffset::from_whole_seconds(offset).ok()
}

/// How an agent is named in sentences, such as `Claude Code is back`, where the app's names
/// aren't at hand. Unknown ids stay as they are.
pub fn agent_label(id: &str) -> String {
    DisplayNames::default().cli(id)
}

/// How a model list names `choice`: a CLI's default with the model it runs, and an alias
/// with its model when that reads differently.
pub fn choice_words(choice: &ModelChoice) -> String {
    let runs = choice
        .runs
        .as_deref()
        .map(str::trim)
        .filter(|runs| !runs.is_empty());
    if choice.worker.model.is_none() {
        let default = format!("{}'s default", agent_label(&choice.worker.agent));
        return match runs {
            Some(runs) => format!("{default} ({runs})"),
            None => default,
        };
    }
    let name = choice.display_name();
    match runs {
        Some(runs) if choice.alias && runs != name => format!("{name} ({runs})"),
        _ => name,
    }
}

/// A setting's TOML text as it reads in a sentence: a plain string loses its quotes, and
/// anything else stays as written.
pub fn setting_value(toml: &str) -> String {
    let trimmed = toml.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        });
    match unquoted {
        Some(inner) if !inner.contains(['"', '\\', '\'']) => inner.to_owned(),
        _ => trimmed.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockTimeError(String);

impl fmt::Display for ClockTimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a clock time: {:?}", self.0)
    }
}

impl std::error::Error for ClockTimeError {}

impl FromStr for ClockTime {
    type Err = ClockTimeError;

    /// Reads `10:42 AM`, `6:41 pm`, or 24 hour `18:05`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let error = || ClockTimeError(text.to_owned());
        let trimmed = text.trim();
        let (clock, suffix) = match trimmed.rsplit_once(' ') {
            Some((clock, suffix)) => (clock.trim(), Some(suffix.to_ascii_uppercase())),
            None => (trimmed, None),
        };
        let (hour, minute) = clock.split_once(':').ok_or_else(error)?;
        let hour: u8 = hour.parse().map_err(|_| error())?;
        let minute: u8 = minute.parse().map_err(|_| error())?;
        let hour = match suffix.as_deref() {
            None => hour,
            Some("AM") if (1..=12).contains(&hour) => hour % 12,
            Some("PM") if (1..=12).contains(&hour) => hour % 12 + 12,
            Some(_) => return Err(error()),
        };
        Self::new(hour, minute).ok_or_else(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_marks_an_estimate_and_a_floor_but_not_a_free_model() {
        let estimated = Spend {
            cost_usd: 0.24,
            cost_estimated: true,
            ..Spend::default()
        };
        assert_eq!(spend(&estimated), "~$0.24");
        let floor = Spend {
            cost_usd: 1.5,
            cost_partial: true,
            ..Spend::default()
        };
        assert_eq!(spend(&floor), "$1.50+");
        let free = Spend {
            cost_estimated: true,
            ..Spend::default()
        };
        assert_eq!(spend(&free), "$0.00");
    }

    #[test]
    fn context_counts_read_as_claude_codes_context_writes_them() {
        for (count, words) in [
            (960, "960"),
            (4_400, "4.4k"),
            (6_000, "6k"),
            (182_460, "182.5k"),
            (784_540, "784.5k"),
            (872_000, "872k"),
            (999_949, "999.9k"),
            (999_950, "1m"),
            (1_000_000, "1m"),
            (1_260_000, "1.3m"),
        ] {
            assert_eq!(context_tokens(count), words, "{count}");
        }
    }

    #[test]
    fn spend_under_a_cent_is_not_written_as_nothing() {
        let spent = |cost_usd, cost_estimated| Spend {
            cost_usd,
            cost_estimated,
            ..Spend::default()
        };
        assert_eq!(spend(&spent(0.004_531_5, false)), "<$0.01");
        assert_eq!(spend(&spent(0.000_8, true)), "<$0.01");
        assert_eq!(spend(&spent(0.006, false)), "$0.01");
        assert_eq!(spend(&spent(0.0, false)), "$0.00");
    }

    #[test]
    fn durations_have_both_shapes() {
        assert_eq!(duration(Duration::from_secs(3)), "3s");
        assert_eq!(duration(Duration::from_secs(590)), "9m 50s");
        assert_eq!(
            duration(Duration::from_secs(2 * 3600 + 53 * 60 + 6)),
            "2h 53m 6s"
        );
        assert_eq!(duration_compact(Duration::from_secs(149)), "2m29s");
        assert_eq!(duration_compact(Duration::from_secs(45)), "45s");
        assert_eq!(duration_compact(Duration::from_secs(3840)), "1h4m");
    }

    #[test]
    fn clock_times_print_and_parse() {
        assert_eq!(ClockTime::new(6, 41).unwrap().to_string(), "6:41 AM");
        assert_eq!(ClockTime::new(0, 5).unwrap().to_string(), "12:05 AM");
        assert_eq!(ClockTime::new(12, 0).unwrap().to_string(), "12:00 PM");
        assert_eq!(ClockTime::new(22, 58).unwrap().to_string(), "10:58 PM");
        assert_eq!("10:42 AM".parse(), Ok(ClockTime::new(10, 42).unwrap()));
        assert_eq!("6:41 pm".parse(), Ok(ClockTime::new(18, 41).unwrap()));
        assert_eq!("18:05".parse(), Ok(ClockTime::new(18, 5).unwrap()));
        assert!("13:00 PM".parse::<ClockTime>().is_err());
        assert!("noon".parse::<ClockTime>().is_err());
        assert_eq!(usd(0.21), "$0.21");
    }

    #[test]
    fn money_and_counts_mark_their_thousands() {
        assert_eq!(usd(12.4), "$12.40");
        assert_eq!(usd(999.994), "$999.99");
        assert_eq!(usd(999.995), "$1,000.00");
        assert_eq!(usd(12_356.88), "$12,356.88");
        assert_eq!(usd(1_234_567.0), "$1,234,567.00");
        assert_eq!(thousands(812), "812");
        assert_eq!(thousands(1_086), "1,086");
        assert_eq!(thousands(1_000_000), "1,000,000");
        let estimated = Spend {
            cost_usd: 12_356.88,
            cost_estimated: true,
            ..Spend::default()
        };
        assert_eq!(spend(&estimated), "~$12,356.88");
    }

    #[test]
    fn a_pinned_clock_shows_times_in_its_own_zone() {
        let now = time::macros::datetime!(2026-09-30 10:58 -7);
        let clock = WallClock::pinned(now);
        assert_eq!(clock.clock_now().to_string(), "10:58 AM");
        let later = time::macros::datetime!(2026-09-30 22:45 UTC);
        assert_eq!(clock.clock(later).to_string(), "3:45 PM");
    }

    #[test]
    fn the_local_clock_reads_the_zone() {
        let now = OffsetDateTime::now_utc();
        let local = WallClock::local().to_local(now);
        assert_eq!(local, now);
        assert!(local.offset().whole_hours().abs() <= 14);
    }

    #[test]
    fn agents_and_values_read_as_words() {
        assert_eq!(agent_label("claude-code"), "Claude Code");
        assert_eq!(agent_label("antigravity-cli"), "agy");
        assert_eq!(agent_label("agy"), "agy");
        assert_eq!(setting_value("\"auto\""), "auto");
        assert_eq!(setting_value("'dark'"), "dark");
        assert_eq!(setting_value("3"), "3");
        assert_eq!(setting_value("[\"a\", \"b\"]"), "[\"a\", \"b\"]");
        assert_eq!(setting_value("\"say \\\"hi\\\"\""), "\"say \\\"hi\\\"\"");
    }
}
