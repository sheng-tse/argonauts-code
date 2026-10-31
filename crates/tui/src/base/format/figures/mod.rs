//! Every figure a screen writes, by one of three named styles, so one number reads one way
//! wherever the same kind of place shows it: a table's column, a sentence, or a copy of one of
//! Claude Code's own screens.

use std::time::Duration;

use argonauts_core::Spend;

use super::numbers::{percent, tokens, tokens_as_claude_code};

/// How figures read where they stand. Each style has one function per kind of figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Figures {
    /// A table's columns and the totals over them: tokens in millions with both decimals,
    /// whole-percent shares with `<1%` for a sliver, time to the minute, so a column lines up
    /// and its parts add up to the total: `6.50M`, `51%`, `~$0.38`, `1h 39m`, `3d ago`.
    Table,
    /// Sentences and lines of words, where nothing lines up: `1.2M`, `0.7%`, `2h 5m`,
    /// `8 days ago`.
    Prose,
    /// The places that copy Claude Code's own rows, as it writes them: `30.3k`, `0.4%`,
    /// `1m 2s`, `37m ago`.
    ClaudeCode,
    /// The council's working line, as its drawings write it: its tokens as a sentence writes
    /// them, `1.2M`, and its time with the parts after the first in two digits, `11m 02s`, so
    /// it keeps its width as it runs.
    Council,
}

impl Figures {
    /// The style for token counts out of `total` tokens in all, as Limits' spend and the row
    /// under the tabs write them: from a million in all, a table's millions, so every count is
    /// on one scale; below that, a sentence's `840k`.
    pub fn for_tokens_of(total: u64) -> Figures {
        if total < 1_000_000 {
            Self::Prose
        } else {
            Self::Table
        }
    }

    pub fn tokens(self, count: u64) -> String {
        match self {
            Self::Table => millions(count),
            Self::Prose | Self::Council => tokens(count),
            Self::ClaudeCode => tokens_as_claude_code(count),
        }
    }

    /// `part`'s share of `whole`, both in one unit (tokens, seconds, fractions of a cent). A
    /// part that is something never reads as nothing, and nothing of nothing reads `0%`.
    pub fn share(self, part: u64, whole: u64) -> String {
        match self {
            Self::Table => whole_percent(part, whole),
            Self::Prose if whole == 0 => "0%".to_owned(),
            Self::Prose => percent(part as f64 / whole as f64),
            Self::ClaudeCode | Self::Council => tenths_of_a_percent(part, whole),
        }
    }

    /// What was spent. Spend reads one way in every style, so a total and its parts never
    /// disagree: `~$0.38` estimated, `$1.50+` with some usage unpriced, `<$0.01` for a sliver,
    /// `$0.00` for nothing.
    pub fn spend(self, spent: &Spend) -> String {
        super::spend(spent)
    }

    pub fn duration(self, took: Duration) -> String {
        let secs = took.as_secs();
        match self {
            Self::Table => to_the_minute(secs),
            Self::Prose => two_largest_parts(secs),
            Self::ClaudeCode => super::duration(took),
            Self::Council => two_digits_after_the_first(secs),
        }
    }

    /// How long ago something happened, given the time since it did.
    pub fn ago(self, since: Duration) -> String {
        let secs = since.as_secs();
        match self {
            Self::Table => short_ago(secs),
            Self::Prose => ago_in_words(secs),
            Self::ClaudeCode | Self::Council => claude_codes_ago(secs),
        }
    }
}

/// `14s`, `11m 02s`, `1h 05m 09s`.
fn two_digits_after_the_first(secs: u64) -> String {
    let (hours, minutes, seconds) = (secs / HOUR, secs / MINUTE % 60, secs % MINUTE);
    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;
const MONTH: u64 = 30 * DAY;
const YEAR: u64 = 365 * DAY;

/// Millions with both decimals on one scale, so a column of them lines up: `6.50M`, `0.07M`,
/// `<0.01M` for a sliver, and billions from a thousand million, `1.20b`, the `b` lower case
/// as everywhere billions are written. Rounded half up in whole numbers, so a boundary such
/// as 9,995,000 doesn't land on a float's side of it.
fn millions(count: u64) -> String {
    let of_a_million = rounded(u128::from(count), 10_000);
    let (value, unit) = if of_a_million < 100_000 {
        (of_a_million, "M")
    } else {
        (rounded(u128::from(count), 10_000_000), "b")
    };
    if count > 0 && value == 0 {
        return "<0.01M".to_owned();
    }
    format!("{}.{:02}{unit}", value / 100, value % 100)
}

/// `26%`, `<1%` for a part too small to round to one, rounded half up.
fn whole_percent(part: u64, whole: u64) -> String {
    if part == 0 || whole == 0 {
        return "0%".to_owned();
    }
    match rounded(u128::from(part) * 100, u128::from(whole)) {
        0 => "<1%".to_owned(),
        share => format!("{share}%"),
    }
}

/// `46.0%`, `0.4%`, and `<0.1%` where the tenths would read `0.0%` for something there.
/// Claude Code writes `0.0%` there; a part that takes room says so here.
fn tenths_of_a_percent(part: u64, whole: u64) -> String {
    if part == 0 || whole == 0 {
        return "0.0%".to_owned();
    }
    match rounded(u128::from(part) * 1_000, u128::from(whole)) {
        0 => "<0.1%".to_owned(),
        tenths => format!("{}.{}%", tenths / 10, tenths % 10),
    }
}

/// `top / bottom` rounded half up, in whole numbers.
fn rounded(top: u128, bottom: u128) -> u128 {
    (top * 2 + bottom) / (bottom * 2)
}

/// `41s` under a minute, then `21m`, `1h 39m`; hours keep counting past a day, so a column of
/// them compares at a glance.
fn to_the_minute(secs: u64) -> String {
    match secs {
        HOUR.. => format!("{}h {}m", secs / HOUR, secs % HOUR / MINUTE),
        MINUTE.. => format!("{}m", secs / MINUTE),
        _ => format!("{secs}s"),
    }
}

/// `40s`, `14m`, `2h 5m`, `57d 3h`: to the minute, and the two largest parts from an hour on.
fn two_largest_parts(secs: u64) -> String {
    match secs {
        DAY.. => format!("{}d {}h", secs / DAY, secs % DAY / HOUR),
        HOUR.. => format!("{}h {}m", secs / HOUR, secs % HOUR / MINUTE),
        MINUTE.. => format!("{}m", secs / MINUTE),
        _ => format!("{secs}s"),
    }
}

/// `just now`, `12m ago`, `3h ago`, `4d ago`, `3w ago`, `2mo ago`, `1y ago`.
fn short_ago(secs: u64) -> String {
    if secs < MINUTE {
        return "just now".to_owned();
    }
    format!("{} ago", in_largest_unit(secs))
}

/// `now`, `5s ago`, `37m ago`, `2h ago`, `3d ago`, as Claude Code's history search writes it.
fn claude_codes_ago(secs: u64) -> String {
    match secs {
        0 => "now".to_owned(),
        1..MINUTE => format!("{secs}s ago"),
        _ => format!("{} ago", in_largest_unit(secs)),
    }
}

/// The time in its largest whole unit from minutes up: `12m`, `3h`, `4d`, `3w`, `2mo`, `1y`.
fn in_largest_unit(secs: u64) -> String {
    let (unit, letter) = match secs {
        YEAR.. => (YEAR, "y"),
        MONTH.. => (MONTH, "mo"),
        WEEK.. => (WEEK, "w"),
        DAY.. => (DAY, "d"),
        HOUR.. => (HOUR, "h"),
        _ => (MINUTE, "m"),
    };
    format!("{}{letter}", secs / unit)
}

/// `just now`, `3 min ago`, `5 h ago`, then days, `1 day ago`, `8 days ago`, as far back as
/// it goes.
fn ago_in_words(secs: u64) -> String {
    match secs {
        DAY.. => match secs / DAY {
            1 => "1 day ago".to_owned(),
            days => format!("{days} days ago"),
        },
        HOUR.. => format!("{} h ago", secs / HOUR),
        MINUTE.. => format!("{} min ago", secs / MINUTE),
        _ => "just now".to_owned(),
    }
}

#[cfg(test)]
mod tests;
