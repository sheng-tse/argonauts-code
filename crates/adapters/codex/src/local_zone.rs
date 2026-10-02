//! The machine's UTC offset at any moment, read from the same TZif file the C library uses.
//! Codex prints reset times in local time, and the `time` crate can't tell the local offset
//! once a process has threads.

use std::path::{Path, PathBuf};

use time::{Date, Month, UtcOffset, Weekday};

const ZONEINFO_DIRS: &[&str] = &["/usr/share/zoneinfo", "/var/db/timezone/zoneinfo"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalZone {
    /// (unix seconds, offset in seconds from then on), sorted by time.
    transitions: Vec<(i64, i32)>,
    /// Offset in seconds before the first transition.
    initial: i32,
    /// After the last transition, from the file's POSIX TZ footer.
    rule: Option<PosixRule>,
}

impl LocalZone {
    pub fn fixed(offset_seconds: i32) -> Self {
        Self {
            transitions: Vec::new(),
            initial: offset_seconds,
            rule: None,
        }
    }

    /// `TZ` when set, else `/etc/localtime`, else UTC.
    pub fn system() -> Self {
        let from_tz = std::env::var("TZ")
            .ok()
            .and_then(|tz| Self::from_tz_value(&tz));
        from_tz
            .or_else(|| Self::from_file(Path::new("/etc/localtime")))
            .unwrap_or_else(|| Self::fixed(0))
    }

    fn from_tz_value(tz: &str) -> Option<Self> {
        let name = tz.strip_prefix(':').unwrap_or(tz).trim();
        if name.is_empty() {
            return Some(Self::fixed(0));
        }
        let candidates: Vec<PathBuf> = if name.starts_with('/') {
            vec![PathBuf::from(name)]
        } else {
            ZONEINFO_DIRS
                .iter()
                .map(|dir| Path::new(dir).join(name))
                .collect()
        };
        candidates
            .iter()
            .find_map(|path| Self::from_file(path))
            .or_else(|| Self::posix(name))
            .or_else(|| matches!(name, "UTC" | "GMT").then(|| Self::fixed(0)))
    }

    /// A POSIX TZ string such as `EST5EDT,M3.2.0,M11.1.0`.
    pub fn posix(text: &str) -> Option<Self> {
        PosixRule::parse(text).map(|rule| Self {
            transitions: Vec::new(),
            initial: rule.std_offset,
            rule: Some(rule),
        })
    }

    fn from_file(path: &Path) -> Option<Self> {
        Self::from_tzif(&std::fs::read(path).ok()?)
    }

    /// Parses a TZif file (RFC 8536), using the 64-bit block when there is one.
    pub fn from_tzif(bytes: &[u8]) -> Option<Self> {
        let first = TzifHeader::read(bytes)?;
        let (header, data) = if first.version >= 2 {
            let second = bytes.get(44 + first.data_len(4)..)?;
            (TzifHeader::read(second)?, second.get(44..)?)
        } else {
            (first, bytes.get(44..)?)
        };
        let time_size = if header.version >= 2 { 8 } else { 4 };
        let mut at = 0;
        let times: Vec<i64> = (0..header.time_count)
            .map(|_| {
                let value = read_int(data, at, time_size);
                at += time_size;
                value
            })
            .collect::<Option<_>>()?;
        let indexes = data.get(at..at + header.time_count)?;
        at += header.time_count;
        let offsets: Vec<i32> = (0..header.type_count)
            .map(|i| read_int(data, at + i * 6, 4).map(|v| v as i32))
            .collect::<Option<_>>()?;
        let transitions = times
            .into_iter()
            .zip(indexes)
            .map(|(time, &index)| offsets.get(usize::from(index)).map(|o| (time, *o)))
            .collect::<Option<_>>()?;
        let footer = data.get(header.data_len(time_size)..).and_then(|rest| {
            let text = std::str::from_utf8(rest).ok()?;
            PosixRule::parse(text.trim_matches('\n'))
        });
        Some(Self {
            transitions,
            initial: *offsets.first()?,
            rule: footer.filter(|_| header.version >= 2),
        })
    }

    pub fn offset_at(&self, unix: i64) -> UtcOffset {
        let passed = self.transitions.partition_point(|(time, _)| *time <= unix);
        let seconds = match (&self.rule, passed.checked_sub(1)) {
            (Some(rule), _) if passed == self.transitions.len() => rule.offset_at(unix),
            (_, Some(last)) => self.transitions[last].1,
            (_, None) => self.initial,
        };
        UtcOffset::from_whole_seconds(seconds).unwrap_or(UtcOffset::UTC)
    }
}

struct TzifHeader {
    version: u8,
    utc_count: usize,
    std_count: usize,
    leap_count: usize,
    time_count: usize,
    type_count: usize,
    char_count: usize,
}

impl TzifHeader {
    fn read(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..4)? != b"TZif" {
            return None;
        }
        let version = match *bytes.get(4)? {
            0 => 1,
            digit => digit.checked_sub(b'0')?,
        };
        let count = |i: usize| read_int(bytes, 20 + i * 4, 4).and_then(|v| usize::try_from(v).ok());
        Some(Self {
            version,
            utc_count: count(0)?,
            std_count: count(1)?,
            leap_count: count(2)?,
            time_count: count(3)?,
            type_count: count(4)?,
            char_count: count(5)?,
        })
    }

    fn data_len(&self, time_size: usize) -> usize {
        self.time_count * (time_size + 1)
            + self.type_count * 6
            + self.char_count
            + self.leap_count * (time_size + 4)
            + self.std_count
            + self.utc_count
    }
}

fn read_int(bytes: &[u8], at: usize, size: usize) -> Option<i64> {
    let slice = bytes.get(at..at + size)?;
    Some(match size {
        4 => i64::from(i32::from_be_bytes(slice.try_into().ok()?)),
        _ => i64::from_be_bytes(slice.try_into().ok()?),
    })
}

/// A POSIX TZ string such as `EST5EDT,M3.2.0,M11.1.0`. Offsets are stored east of UTC, the
/// opposite sign of the string.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PosixRule {
    std_offset: i32,
    dst: Option<DstRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DstRule {
    offset: i32,
    start: (DayRule, i32),
    end: (DayRule, i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DayRule {
    /// `Jn`: day 1 to 365, never counting February 29.
    Julian(u16),
    /// `n`: day 0 to 365, counting February 29.
    Zero(u16),
    /// `Mm.w.d`: weekday `d` of week `w` (5 means last) of month `m`.
    Month(u8, u8, u8),
}

impl PosixRule {
    fn parse(text: &str) -> Option<Self> {
        let mut rest = text;
        skip_name(&mut rest)?;
        let std_offset = -parse_hms(&mut rest)?;
        if rest.is_empty() {
            return Some(Self {
                std_offset,
                dst: None,
            });
        }
        skip_name(&mut rest)?;
        let offset = if rest.starts_with(',') || rest.is_empty() {
            std_offset + 3600
        } else {
            -parse_hms(&mut rest)?
        };
        // Without rules, POSIX leaves the dates to the system; the US rules are the usual default.
        let rules = if rest.is_empty() {
            "M3.2.0,M11.1.0"
        } else {
            rest.strip_prefix(',')?
        };
        let (start, end) = rules.split_once(',')?;
        Some(Self {
            std_offset,
            dst: Some(DstRule {
                offset,
                start: parse_transition(start)?,
                end: parse_transition(end)?,
            }),
        })
    }

    fn offset_at(&self, unix: i64) -> i32 {
        let Some(dst) = &self.dst else {
            return self.std_offset;
        };
        let local_year =
            time::OffsetDateTime::from_unix_timestamp(unix + i64::from(self.std_offset))
                .map(|t| t.year())
                .unwrap_or(1970);
        let start = transition_unix(local_year, dst.start, self.std_offset);
        let end = transition_unix(local_year, dst.end, dst.offset);
        let in_dst = match (start, end) {
            (Some(start), Some(end)) if start < end => start <= unix && unix < end,
            // Southern hemisphere: summer time spans the new year.
            (Some(start), Some(end)) => unix < end || start <= unix,
            _ => false,
        };
        if in_dst { dst.offset } else { self.std_offset }
    }
}

/// The instant a rule's local wall time happens, given the offset in force just before it.
fn transition_unix(year: i32, (day, seconds): (DayRule, i32), offset: i32) -> Option<i64> {
    let date = day.date_in(year)?;
    let midnight = date.midnight().assume_utc().unix_timestamp();
    Some(midnight + i64::from(seconds) - i64::from(offset))
}

impl DayRule {
    fn date_in(self, year: i32) -> Option<Date> {
        match self {
            Self::Julian(n) => {
                let leap_shift = u16::from(time::util::is_leap_year(year) && n >= 60);
                Date::from_ordinal_date(year, n + leap_shift).ok()
            }
            Self::Zero(n) => Date::from_ordinal_date(year, n + 1).ok(),
            Self::Month(month, week, weekday) => {
                let month = Month::try_from(month).ok()?;
                let first = Date::from_calendar_date(year, month, 1).ok()?;
                let weekday = Weekday::Sunday.nth_next(weekday);
                let lead = (weekday.number_days_from_sunday() + 7
                    - first.weekday().number_days_from_sunday())
                    % 7;
                let mut day = 1 + lead + 7 * (week - 1);
                let last = time::util::days_in_month(month, year);
                while day > last {
                    day -= 7;
                }
                Date::from_calendar_date(year, month, day).ok()
            }
        }
    }
}

fn skip_name(rest: &mut &str) -> Option<()> {
    let len = if let Some(quoted) = rest.strip_prefix('<') {
        quoted.find('>')? + 2
    } else {
        rest.find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len())
    };
    (len >= 3).then(|| *rest = &rest[len..])
}

/// `[+-]hh[:mm[:ss]]` in seconds.
fn parse_hms(rest: &mut &str) -> Option<i32> {
    let sign = match rest.chars().next()? {
        '-' => -1,
        _ => 1,
    };
    let body = rest.trim_start_matches(['+', '-']);
    let len = body
        .find(|c: char| !(c.is_ascii_digit() || c == ':'))
        .unwrap_or(body.len());
    let mut seconds = 0;
    for (part, scale) in body[..len].split(':').zip([3600, 60, 1]) {
        seconds += part.parse::<i32>().ok()? * scale;
    }
    *rest = &body[len..];
    Some(sign * seconds)
}

fn parse_transition(text: &str) -> Option<(DayRule, i32)> {
    let (day, time) = match text.split_once('/') {
        Some((day, time)) => (day, parse_hms(&mut &*time)?),
        None => (text, 2 * 3600),
    };
    let rule = if let Some(n) = day.strip_prefix('J') {
        DayRule::Julian(n.parse().ok().filter(|n| (1..=365).contains(n))?)
    } else if let Some(m) = day.strip_prefix('M') {
        let mut parts = m.split('.').map(str::parse::<u8>);
        let (month, week, weekday) = (
            parts.next()?.ok()?,
            parts.next()?.ok()?,
            parts.next()?.ok()?,
        );
        let valid = (1..=12).contains(&month) && (1..=5).contains(&week) && weekday <= 6;
        valid.then_some(DayRule::Month(month, week, weekday))?
    } else {
        DayRule::Zero(day.parse().ok().filter(|n| *n <= 365)?)
    };
    Some((rule, time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn offset_hours(zone: &LocalZone, at: time::OffsetDateTime) -> f64 {
        f64::from(zone.offset_at(at.unix_timestamp()).whole_seconds()) / 3600.0
    }

    #[test]
    fn posix_rules_follow_both_hemispheres() {
        let new_york = LocalZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(
            offset_hours(&new_york, datetime!(2026-01-15 12:00 UTC)),
            -5.0
        );
        assert_eq!(
            offset_hours(&new_york, datetime!(2026-09-30 12:00 UTC)),
            -4.0
        );
        // DST starts 2026-03-08 at 02:00 EST, which is 07:00 UTC.
        assert_eq!(
            offset_hours(&new_york, datetime!(2026-03-08 06:59 UTC)),
            -5.0
        );
        assert_eq!(
            offset_hours(&new_york, datetime!(2026-03-08 07:00 UTC)),
            -4.0
        );
        // It ends 2026-11-01 at 02:00 EDT, which is 06:00 UTC.
        assert_eq!(
            offset_hours(&new_york, datetime!(2026-11-01 05:59 UTC)),
            -4.0
        );
        assert_eq!(
            offset_hours(&new_york, datetime!(2026-11-01 06:00 UTC)),
            -5.0
        );

        let sydney = LocalZone::posix("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(offset_hours(&sydney, datetime!(2026-01-15 00:00 UTC)), 11.0);
        assert_eq!(offset_hours(&sydney, datetime!(2026-06-15 00:00 UTC)), 10.0);

        let india = LocalZone::posix("<+0530>-5:30").unwrap();
        assert_eq!(offset_hours(&india, datetime!(2026-06-15 00:00 UTC)), 5.5);
        assert_eq!(LocalZone::from_tz_value(""), Some(LocalZone::fixed(0)));
    }

    #[test]
    fn tzif_transitions_then_footer() {
        // A version 2 file with one transition in 2026 to UTC-4 and a New York footer.
        let mut file = tzif_header(b'2', 0, 0);
        file.extend(tzif_header(b'2', 1, 2));
        file.extend(
            datetime!(2026-03-08 07:00 UTC)
                .unix_timestamp()
                .to_be_bytes(),
        );
        file.push(1);
        for (offset, dst) in [(-5 * 3600, 0u8), (-4 * 3600, 1)] {
            file.extend(i32::to_be_bytes(offset));
            file.extend([dst, 0]);
        }
        file.extend(b"\nEST5EDT,M3.2.0,M11.1.0\n");
        let zone = LocalZone::from_tzif(&file).unwrap();
        assert_eq!(offset_hours(&zone, datetime!(2026-01-15 12:00 UTC)), -5.0);
        assert_eq!(offset_hours(&zone, datetime!(2026-09-30 12:00 UTC)), -4.0);
        assert_eq!(offset_hours(&zone, datetime!(2026-12-15 12:00 UTC)), -5.0);
        assert!(LocalZone::from_tzif(b"not a tzif file").is_none());
    }

    #[test]
    fn system_zone_matches_the_c_library() {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let offset = LocalZone::system().offset_at(now);
        // `date +%z` prints the C library's offset, such as -0400.
        let Ok(output) = std::process::Command::new("date").arg("+%z").output() else {
            return;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let text = text.trim();
        let hours: i32 = text[..3].parse().unwrap();
        let minutes: i32 = text[3..].parse().unwrap();
        let expected = hours * 3600 + hours.signum() * minutes * 60;
        assert_eq!(offset.whole_seconds(), expected, "date +%z says {text}");
    }

    fn tzif_header(version: u8, time_count: u32, type_count: u32) -> Vec<u8> {
        let mut header = b"TZif".to_vec();
        header.push(version);
        header.extend([0; 15]);
        for count in [0, 0, 0, time_count, type_count, 0] {
            header.extend(u32::to_be_bytes(count));
        }
        header
    }
}
