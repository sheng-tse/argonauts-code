//! One wording for each kind of number, so every mod, row, and card reads the same.

use std::time::Duration;

use argonauts_core::window_size;

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

/// Whether plan usage says how much is used or how much is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanUsageWords {
    #[default]
    Used,
    Left,
}

/// `ui.format`: the choices a person can make about how numbers read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Formats {
    pub tokens: TokenUnit,
    pub plan_usage: PlanUsageWords,
    pub reset: ResetStyle,
    pub clock: ClockStyle,
}

/// Token counts: `950`, `3.4k`, `48.2k`, `212k`, `1.2M`, `1M`, `1.2b`.
pub fn tokens(count: u64) -> String {
    tokens_as(count, TokenUnit::Upper)
}

/// Token counts with one decimal under 100k, under 10M and under 10b, and `.0` dropped. The
/// `b` of billions is lower case whatever the unit, as everywhere billions are written.
pub fn tokens_as(count: u64, unit: TokenUnit) -> String {
    if count < 1_000 {
        return count.to_string();
    }
    let million = match unit {
        TokenUnit::Upper => "M",
        TokenUnit::Lower => "m",
    };
    // From 999.5k on, the thousands would round to "1000k", so it reads as a million, and
    // likewise a billion from 999.5M on.
    let (value, unit, decimal_below) = match count {
        999_500_000.. => (count as f64 / 1_000_000_000.0, "b", 10.0),
        999_500.. => (count as f64 / 1_000_000.0, million, 10.0),
        _ => (count as f64 / 1_000.0, "k", 100.0),
    };
    let digits = if value < decimal_below {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    let trimmed = digits.strip_suffix(".0").unwrap_or(&digits);
    format!("{trimmed}{unit}")
}

/// Token counts in tenths of a thousand all the way to a million, as the rows of runs under the
/// input write them, where a run's count grows a little at a time: `240.6k`, `12k`. Under a
/// thousand and from a million on, as [`tokens`] writes them.
pub fn tokens_in_tenths(count: u64) -> String {
    if !(1_000..999_500).contains(&count) {
        return tokens(count);
    }
    let thousands = format!("{:.1}", count as f64 / 1_000.0);
    format!("{}k", thousands.strip_suffix(".0").unwrap_or(&thousands))
}

/// Token counts as Claude Code writes them under an agent's row, by Intl's compact rule: one
/// decimal from 1,000 on, `.0` kept, `11.0k`, `118.8k`, `1.2m`; under 1,000 the count itself.
pub fn tokens_as_claude_code(count: u64) -> String {
    const UNITS: [&str; 4] = ["k", "m", "b", "t"];
    if count < 1_000 {
        return count.to_string();
    }
    let mut value = count as f64 / 1_000.0;
    let mut unit = 0;
    // Rounded first, so 999,950 reads as the next unit up, as Intl's does.
    while (value * 10.0).round() >= 10_000.0 && unit + 1 < UNITS.len() {
        value /= 1_000.0;
        unit += 1;
    }
    format!("{:.1}{}", (value * 10.0).round() / 10.0, UNITS[unit])
}

/// A context window's size, by the one rule every screen, mod and command shares: `64k` for
/// 65,536, `200k` for 200,000, `1M` for either million.
pub fn window(size: u64) -> String {
    window_size::window_words(size)
}

/// A token count beside a window, in the window's divisor: 9,551 of 65,536 reads `9.3k/64k`,
/// and a full 64k window `64k/64k`. A count with no window beside it reads as [`tokens`].
pub fn tokens_beside(count: u64, window: u64) -> String {
    window_size::tokens_beside(count, window)
}

/// What a session holds of its window, `9.3k/64k`, or its count alone while no one has said
/// how large the window is.
pub fn tokens_of_window(used: u64, window: Option<u64>) -> String {
    match window.filter(|window| *window > 0) {
        Some(window) => format!("{}/{}", tokens_beside(used, window), self::window(window)),
        None => tokens(used),
    }
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

/// Plan usage after its CLI and window, as `71% used`, or `29% left` with
/// `ui.format.plan_usage = "left"`. A stale reading is marked `~71% used`.
pub fn plan_usage(used: f64, words: PlanUsageWords, stale: bool) -> String {
    let used = used.max(0.0) * 100.0;
    let mark = if stale { "~" } else { "" };
    match words {
        PlanUsageWords::Used => format!("{mark}{used:.0}% used"),
        PlanUsageWords::Left => format!("{mark}{:.0}% left", (100.0 - used).max(0.0)),
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

    /// Claude Code 2.1.303's `ti()`: Intl's compact notation, one decimal from 1,000 on.
    #[test]
    fn tokens_read_as_claude_code_writes_them_under_an_agent() {
        for (count, words) in [
            (950, "950"),
            (1_000, "1.0k"),
            (11_000, "11.0k"),
            (14_249, "14.2k"),
            (118_800, "118.8k"),
            (999_949, "999.9k"),
            (999_950, "1.0m"),
            (1_250_000, "1.3m"),
            (2_000_000_000, "2.0b"),
        ] {
            assert_eq!(tokens_as_claude_code(count), words, "{count}");
        }
    }

    #[test]
    fn a_runs_row_counts_tokens_in_tenths_up_to_a_million() {
        for (count, words) in [
            (950, "950"),
            (12_000, "12k"),
            (240_640, "240.6k"),
            (999_449, "999.4k"),
            (999_500, "1M"),
            (2_340_000, "2.3M"),
        ] {
            assert_eq!(tokens_in_tenths(count), words, "{count}");
        }
        assert_eq!(
            tokens(240_640),
            "241k",
            "elsewhere a count that size has no tenths"
        );
    }

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
        assert_eq!(tokens(999_499_999), "999M");
        assert_eq!(tokens(999_500_000), "1b");
        assert_eq!(tokens(1_240_000_000), "1.2b");
        assert_eq!(tokens(12_400_000_000), "12b");
        assert_eq!(tokens_as(1_240_000_000, TokenUnit::Lower), "1.2b");
        assert_eq!(tokens_as(1_200_000, TokenUnit::Lower), "1.2m");
        assert_eq!(tokens_as(48_200, TokenUnit::Lower), "48.2k");
    }

    #[test]
    fn windows_and_the_counts_beside_them_share_one_divisor() {
        assert_eq!(window(65_536), "64k");
        assert_eq!(window(262_144), "256k");
        assert_eq!(window(200_000), "200k");
        assert_eq!(window(1_000_000), "1M");
        assert_eq!(window(1_048_576), "1M");
        assert_eq!(tokens_beside(9_551, 65_536), "9.3k");
        assert_eq!(tokens_beside(65_536, 65_536), "64k");
        // Beside a window in thousands a count reads as every other count does.
        assert_eq!(tokens_beside(9_551, 200_000), tokens(9_551));
        assert_eq!(tokens_of_window(9_551, Some(65_536)), "9.3k/64k");
        assert_eq!(tokens_of_window(65_536, Some(65_536)), "64k/64k");
        assert_eq!(tokens_of_window(9_551, None), "9.6k");
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
    fn plan_usage_says_used_or_left() {
        assert_eq!(plan_usage(0.71, PlanUsageWords::Used, false), "71% used");
        assert_eq!(plan_usage(0.71, PlanUsageWords::Left, false), "29% left");
        assert_eq!(plan_usage(0.71, PlanUsageWords::Used, true), "~71% used");
        assert_eq!(plan_usage(1.05, PlanUsageWords::Left, false), "0% left");
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
