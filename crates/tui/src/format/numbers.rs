//! One wording for each kind of number, so every widget, row, and card reads the same.

use std::time::Duration;

use super::resets::{ClockStyle, ResetStyle};

/// What a number shows when it isn't known. Never `?`.
pub const UNKNOWN: &str = "–";

/// How millions are written. `M` never reads as minutes beside `51m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TokenUnit {
    #[default]
    Upper,
    /// Claude Code's `1.2m`.
    Lower,
}

/// Whether quota says how much is used or how much is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuotaWords {
    #[default]
    Used,
    Left,
}

/// `ui.format`: the choices a person can make about how numbers read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Formats {
    pub tokens: TokenUnit,
    pub quota: QuotaWords,
    pub reset: ResetStyle,
    pub clock: ClockStyle,
}

/// Token counts: `950`, `3.4k`, `48.2k`, `212k`, `1.2M`, `1M`.
pub fn tokens(count: u64) -> String {
    tokens_as(count, TokenUnit::Upper)
}

/// Token counts with one decimal under 100k and under 10M, and `.0` dropped.
pub fn tokens_as(count: u64, unit: TokenUnit) -> String {
    if count < 1_000 {
        return count.to_string();
    }
    let million = match unit {
        TokenUnit::Upper => "M",
        TokenUnit::Lower => "m",
    };
    // From 999.5k on, the thousands would round to "1000k", so it reads as a million.
    let (value, unit, decimal_below) = if count < 999_500 {
        (count as f64 / 1_000.0, "k", 100.0)
    } else {
        (count as f64 / 1_000_000.0, million, 10.0)
    };
    let digits = if value < decimal_below {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    let trimmed = digits.strip_suffix(".0").unwrap_or(&digits);
    format!("{trimmed}{unit}")
}

/// A share of a whole: whole percents, one decimal under 1%, and `<0.1%` below that. Over a
/// limit it keeps counting, as `105%`.
pub fn percent(ratio: f64) -> String {
    let p = ratio.max(0.0) * 100.0;
    if p == 0.0 {
        String::from("0%")
    } else if p < 0.1 {
        String::from("<0.1%")
    } else if p < 0.95 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// Quota after its vendor and window, as `71% used`, or `29% left` with
/// `ui.format.quota = "left"`. A stale reading is marked `~71% used`.
pub fn quota(used: f64, words: QuotaWords, stale: bool) -> String {
    let used = used.max(0.0) * 100.0;
    let mark = if stale { "~" } else { "" };
    match words {
        QuotaWords::Used => format!("{mark}{used:.0}% used"),
        QuotaWords::Left => format!("{mark}{:.0}% left", (100.0 - used).max(0.0)),
    }
}

/// A multiplier with two decimals: `0.90×`, `1.20×`.
pub fn multiplier(x: f64) -> String {
    format!("{x:.2}×")
}

/// A reading that's out of date: `~` before it.
pub fn stale(text: &str) -> String {
    format!("~{text}")
}

/// A duration in a fixed slot, zero-padded so the slot keeps its width as it counts:
/// `45s`, `4m12s`, `14m02s`, `2h44m`, `2h04m`.
pub fn duration_slot(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_read_one_way_everywhere() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1_000), "1k");
        assert_eq!(tokens(3_400), "3.4k");
        assert_eq!(tokens(48_200), "48.2k");
        assert_eq!(tokens(99_960), "100k");
        assert_eq!(tokens(212_200), "212k");
        assert_eq!(tokens(999_499), "999k");
        assert_eq!(tokens(999_500), "1M");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_200_000), "1.2M");
        assert_eq!(tokens(9_960_000), "10M");
        assert_eq!(tokens(12_400_000), "12M");
        assert_eq!(tokens_as(1_200_000, TokenUnit::Lower), "1.2m");
        assert_eq!(tokens_as(48_200, TokenUnit::Lower), "48.2k");
    }

    #[test]
    fn shares_keep_small_parts_visible() {
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.212), "21%");
        assert_eq!(percent(0.0034), "0.3%");
        assert_eq!(percent(0.0096), "1%");
        assert_eq!(percent(0.0004), "<0.1%");
        assert_eq!(percent(1.05), "105%");
    }

    #[test]
    fn quota_says_used_or_left() {
        assert_eq!(quota(0.71, QuotaWords::Used, false), "71% used");
        assert_eq!(quota(0.71, QuotaWords::Left, false), "29% left");
        assert_eq!(quota(0.71, QuotaWords::Used, true), "~71% used");
        assert_eq!(quota(1.05, QuotaWords::Left, false), "0% left");
    }

    #[test]
    fn small_numbers_read_as_the_wording_says() {
        assert_eq!(multiplier(0.9), "0.90×");
        assert_eq!(multiplier(1.2), "1.20×");
        assert_eq!(stale("71%"), "~71%");
        assert_eq!(duration_slot(Duration::from_secs(45)), "45s");
        assert_eq!(duration_slot(Duration::from_secs(252)), "4m12s");
        assert_eq!(duration_slot(Duration::from_secs(842)), "14m02s");
        assert_eq!(
            duration_slot(Duration::from_secs(2 * 3600 + 44 * 60)),
            "2h44m"
        );
        assert_eq!(
            duration_slot(Duration::from_secs(2 * 3600 + 4 * 60)),
            "2h04m"
        );
    }
}
