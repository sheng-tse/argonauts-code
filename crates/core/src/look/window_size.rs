//! How a context window's size reads, in one rule that the screens, the mods and the CLI all
//! call, so 65,536 tokens reads 64k everywhere and a full window never reads 65.5k/64k.
//!
//! A window that is a whole number of thousands divides by 1,000, then a multiple of 1,024
//! divides by 1,024, and anything else by 1,000. Thousands go first so that a window someone
//! wrote in thousands, such as 128,000, reads as written and not as 125k. Only the words change:
//! the exact number stays for the maths and the percentages.
//!
//! | Window    | Reads |
//! |-----------|-------|
//! | 65,536    | 64k   |
//! | 131,072   | 128k  |
//! | 262,144   | 256k  |
//! | 1,048,576 | 1M    |
//! | 128,000   | 128k  |
//! | 200,000   | 200k  |
//! | 256,000   | 256k  |
//! | 1,000,000 | 1M    |
//!
//! A count beside a window takes the window's divisor, so a full 64k window reads 64k/64k and
//! 9,551 tokens of it read 9.3k/64k. A count with no window beside it, such as a total across
//! sessions, keeps the plain thousands.

/// What a window's size divides by: 1,000 or 1,024.
pub fn window_divisor(window: u64) -> u64 {
    if window.is_multiple_of(1_000) {
        1_000
    } else if window.is_multiple_of(1_024) {
        1_024
    } else {
        1_000
    }
}

/// A window's size in words: `64k`, `200k`, `1M`, `1.5M`. A window under one thousand reads as
/// its number.
pub fn window_words(window: u64) -> String {
    let divisor = window_divisor(window);
    if window < divisor {
        return window.to_string();
    }
    let thousands = (window as f64 / divisor as f64).round();
    if thousands < 1_000.0 {
        return format!("{thousands}k");
    }
    let millions = window as f64 / (divisor * divisor) as f64;
    format!("{}M", one_decimal_under(millions, 10.0))
}

/// A token count beside a window, in the window's divisor: `9.3k` of `64k`, `64k` of `64k`.
pub fn tokens_beside(count: u64, window: u64) -> String {
    tokens_by(count, window_divisor(window))
}

/// A token count in thousands of `divisor`: one decimal under 100k and under 10M, a trailing
/// `.0` dropped, as every other token count reads. A divisor of 1,000 gives the plain count
/// that no window stands beside.
pub fn tokens_by(count: u64, divisor: u64) -> String {
    if count < divisor {
        return count.to_string();
    }
    let thousands = count as f64 / divisor as f64;
    // From 999.5 thousands on, the thousands would round to "1000k", so it reads as a million.
    if thousands < 999.5 {
        format!("{}k", one_decimal_under(thousands, 100.0))
    } else {
        let millions = count as f64 / (divisor * divisor) as f64;
        format!("{}M", one_decimal_under(millions, 10.0))
    }
}

/// `value` with one decimal below `limit` and none from it on, and never a trailing `.0`.
fn one_decimal_under(value: f64, limit: f64) -> String {
    let digits = if value < limit {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    match digits.strip_suffix(".0") {
        Some(whole) => whole.to_owned(),
        None => digits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_read_by_one_rule() {
        let examples = [
            (65_536, "64k"),
            (131_072, "128k"),
            (262_144, "256k"),
            (1_048_576, "1M"),
            (200_000, "200k"),
            (1_000_000, "1M"),
            (128_000, "128k"),
            (256_000, "256k"),
            (32_768, "32k"),
            (8_192, "8k"),
            (2_097_152, "2M"),
            (1_500_000, "1.5M"),
            (400_000, "400k"),
        ];
        for (window, words) in examples {
            assert_eq!(window_words(window), words, "{window}");
        }
    }

    #[test]
    fn whole_thousands_win_over_1024() {
        // 128,000 is 125 x 1,024 as well.
        assert_eq!(window_divisor(128_000), 1_000);
        assert_eq!(window_words(128_000), "128k");
        assert_eq!(window_divisor(65_536), 1_024);
        assert_eq!(window_divisor(100_001), 1_000);
    }

    #[test]
    fn odd_and_small_windows_read_plainly() {
        assert_eq!(window_words(0), "0");
        assert_eq!(window_words(512), "512");
        assert_eq!(window_words(1_024), "1k");
        assert_eq!(window_words(65_535), "66k");
        assert_eq!(window_words(999_999), "1M");
    }

    #[test]
    fn a_count_beside_a_window_takes_its_divisor() {
        let examples = [
            (9_551, 65_536, "9.3k"),
            (41_984, 65_536, "41k"),
            (65_536, 65_536, "64k"),
            (7_373, 32_768, "7.2k"),
            (524_288, 1_048_576, "512k"),
            (1_048_576, 1_048_576, "1M"),
            (150_000, 200_000, "150k"),
            (9_551, 200_000, "9.6k"),
            (950, 65_536, "950"),
        ];
        for (count, window, words) in examples {
            assert_eq!(tokens_beside(count, window), words, "{count} of {window}");
        }
    }

    #[test]
    fn a_full_window_never_reads_past_itself() {
        for window in [
            32_768, 65_536, 131_072, 262_144, 200_000, 1_000_000, 1_048_576,
        ] {
            assert_eq!(
                tokens_beside(window, window),
                window_words(window),
                "{window}"
            );
        }
    }

    #[test]
    fn plain_counts_read_as_every_token_count_does() {
        let examples = [
            (950, "950"),
            (3_400, "3.4k"),
            (48_200, "48.2k"),
            (212_000, "212k"),
            (999_499, "999k"),
            (999_500, "1M"),
            (1_200_000, "1.2M"),
            (12_000_000, "12M"),
        ];
        for (count, words) in examples {
            assert_eq!(tokens_by(count, 1_000), words, "{count}");
        }
    }
}
