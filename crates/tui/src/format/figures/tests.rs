use std::time::Duration;

use argonauts_core::Spend;

use super::Figures::{self, ClaudeCode, Prose, Table};

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;

/// Each row holds one input and what the table, prose and Claude Code styles write for it.
fn check<T: std::fmt::Debug + Copy>(
    rows: &[(T, &str, &str, &str)],
    write: impl Fn(Figures, T) -> String,
) {
    for &(input, table, prose, claude_code) in rows {
        assert_eq!(write(Table, input), table, "Table, {input:?}");
        assert_eq!(write(Prose, input), prose, "Prose, {input:?}");
        assert_eq!(
            write(ClaudeCode, input),
            claude_code,
            "ClaudeCode, {input:?}"
        );
    }
}

#[test]
fn tokens_at_their_boundaries() {
    check(
        &[
            (0, "0.00M", "0", "0"),
            (999, "<0.01M", "999", "999"),
            (1_000, "<0.01M", "1k", "1.0k"),
            (4_999, "<0.01M", "5k", "5.0k"),
            (5_000, "0.01M", "5k", "5.0k"),
            (99_949, "0.10M", "99.9k", "99.9k"),
            (999_499, "1.00M", "999k", "999.5k"),
            (999_500, "1.00M", "1M", "999.5k"),
            (999_950, "1.00M", "1M", "1.0m"),
            (9_994_999, "9.99M", "10M", "10.0m"),
            (9_995_000, "10.00M", "10M", "10.0m"),
            (10_000_000, "10.00M", "10M", "10.0m"),
            (12_690_000, "12.69M", "13M", "12.7m"),
            (999_994_999, "999.99M", "1b", "1.0b"),
            (999_995_000, "1.00b", "1b", "1.0b"),
            (1_200_000_000, "1.20b", "1.2b", "1.2b"),
        ],
        Figures::tokens,
    );
}

#[test]
fn shares_at_their_boundaries() {
    let of = |part: u64| (part, 10_000);
    check(
        &[
            ((0, 0), "0%", "0%", "0.0%"),
            (of(0), "0%", "0%", "0.0%"),
            (of(4), "<1%", "<0.1%", "<0.1%"),
            (of(40), "<1%", "0.4%", "0.4%"),
            (of(49), "<1%", "0.5%", "0.5%"),
            (of(50), "1%", "0.5%", "0.5%"),
            (of(70), "1%", "0.7%", "0.7%"),
            (of(94), "1%", "0.9%", "0.9%"),
            (of(100), "1%", "1%", "1.0%"),
            (of(149), "1%", "1%", "1.5%"),
            (of(150), "2%", "2%", "1.5%"),
            (of(2_600), "26%", "26%", "26.0%"),
            (of(9_949), "99%", "99%", "99.5%"),
            (of(10_000), "100%", "100%", "100.0%"),
            (of(10_500), "105%", "105%", "105.0%"),
        ],
        |figures, (part, whole)| figures.share(part, whole),
    );
}

#[test]
fn spend_reads_one_way_in_every_style() {
    let spent = |cost_usd, cost_estimated, cost_partial| Spend {
        cost_usd,
        cost_estimated,
        cost_partial,
        ..Spend::default()
    };
    for (spent, words) in [
        (spent(0.0, false, false), "$0.00"),
        (spent(0.0, true, false), "$0.00"),
        (spent(0.004, false, false), "<$0.01"),
        (spent(0.005, true, false), "~$0.01"),
        (spent(0.38, true, false), "~$0.38"),
        (spent(5.35, false, false), "$5.35"),
        (spent(1.5, false, true), "$1.50+"),
        (spent(999.995, false, false), "$1,000.00"),
        (spent(12_356.88, true, false), "~$12,356.88"),
    ] {
        for figures in [Table, Prose, ClaudeCode] {
            assert_eq!(figures.spend(&spent), words, "{figures:?}, {spent:?}");
        }
    }
}

#[test]
fn durations_at_their_boundaries() {
    check(
        &[
            (0, "0s", "0s", "0s"),
            (59, "59s", "59s", "59s"),
            (MINUTE, "1m", "1m", "1m 0s"),
            (MINUTE + 2, "1m", "1m", "1m 2s"),
            (HOUR - 1, "59m", "59m", "59m 59s"),
            (HOUR, "1h 0m", "1h 0m", "1h 0m 0s"),
            (99 * MINUTE, "1h 39m", "1h 39m", "1h 39m 0s"),
            (DAY - 1, "23h 59m", "23h 59m", "23h 59m 59s"),
            (DAY, "24h 0m", "1d 0h", "24h 0m 0s"),
            (WEEK + 3 * HOUR, "171h 0m", "7d 3h", "171h 0m 0s"),
        ],
        |figures, secs| figures.duration(Duration::from_secs(secs)),
    );
    assert_eq!(Table.duration(Duration::from_millis(59_999)), "59s");
}

/// The council's working line writes its time with two digits after the first part, and its
/// tokens as the working line writes them.
#[test]
fn the_councils_time_keeps_two_digits_after_its_first_part() {
    for (secs, words) in [
        (14, "14s"),
        (108, "1m 48s"),
        (662, "11m 02s"),
        (HOUR + 5 * MINUTE + 9, "1h 05m 09s"),
    ] {
        assert_eq!(Figures::Council.duration(Duration::from_secs(secs)), words);
    }
    for (count, words) in [(34_500, "34.5k"), (61_200, "61.2k"), (1_200_000, "1.2M")] {
        assert_eq!(Figures::Council.tokens(count), words);
    }
}

#[test]
fn ago_at_its_boundaries() {
    check(
        &[
            (0, "just now", "just now", "now"),
            (1, "just now", "just now", "1s ago"),
            (59, "just now", "just now", "59s ago"),
            (MINUTE, "1m ago", "1 min ago", "1m ago"),
            (HOUR - 1, "59m ago", "59 min ago", "59m ago"),
            (HOUR, "1h ago", "1 h ago", "1h ago"),
            (DAY - 1, "23h ago", "23 h ago", "23h ago"),
            (DAY, "1d ago", "1 day ago", "1d ago"),
            (2 * DAY, "2d ago", "2 days ago", "2d ago"),
            (WEEK - 1, "6d ago", "6 days ago", "6d ago"),
            (WEEK, "1w ago", "7 days ago", "1w ago"),
            (8 * DAY, "1w ago", "8 days ago", "1w ago"),
            (30 * DAY - 1, "4w ago", "29 days ago", "4w ago"),
            (30 * DAY, "1mo ago", "30 days ago", "1mo ago"),
            (365 * DAY - 1, "12mo ago", "364 days ago", "12mo ago"),
            (365 * DAY, "1y ago", "365 days ago", "1y ago"),
        ],
        |figures, secs| figures.ago(Duration::from_secs(secs)),
    );
}

/// The by-model table as drawn at 177 × 40, 12.69M tokens in all: each row's tokens and
/// share down to Gemma's sliver that still rounds to 1%, the two models with nothing, Luna's
/// ~$0.07 of ~$7.93 and the time column.
#[test]
fn a_table_writes_the_by_model_rows_as_drawn() {
    let all = 12_690_000;
    assert_eq!(Table.tokens(all), "12.69M");
    for (tokens, words, share) in [
        (6_500_000, "6.50M", "51%"),
        (1_970_000, "1.97M", "16%"),
        (1_470_000, "1.47M", "12%"),
        (1_430_000, "1.43M", "11%"),
        (630_000, "0.63M", "5%"),
        (390_000, "0.39M", "3%"),
        (200_000, "0.20M", "2%"),
        (90_000, "0.09M", "1%"),
        (0, "0.00M", "0%"),
    ] {
        assert_eq!(Table.tokens(tokens), words);
        assert_eq!(Table.share(tokens, all), share, "{words}");
    }
    assert_eq!(Table.share(7, 793), "1%");
    for (secs, took) in [(21 * MINUTE + 40, "21m"), (4 * MINUTE, "4m"), (0, "0s")] {
        assert_eq!(Table.duration(Duration::from_secs(secs)), took);
    }
}

/// Every row of the by-model table as drawn at 120 × 38 for a crew of fourteen models over
/// four CLIs, 14.14M tokens in all: whole shares down to the 0.08M that still rounds to 1%,
/// and the two under half a percent.
#[test]
fn a_table_writes_every_row_of_a_large_crew_as_drawn() {
    let rows = [
        (6_500_000, "6.50M", "46%"),
        (1_970_000, "1.97M", "14%"),
        (1_470_000, "1.47M", "10%"),
        (1_430_000, "1.43M", "10%"),
        (840_000, "0.84M", "6%"),
        (630_000, "0.63M", "4%"),
        (390_000, "0.39M", "3%"),
        (310_000, "0.31M", "2%"),
        (200_000, "0.20M", "1%"),
        (120_000, "0.12M", "1%"),
        (90_000, "0.09M", "1%"),
        (80_000, "0.08M", "1%"),
        (60_000, "0.06M", "<1%"),
        (50_000, "0.05M", "<1%"),
    ];
    let all = rows.iter().map(|&(tokens, ..)| tokens).sum();
    assert_eq!(all, 14_140_000);
    for (tokens, words, share) in rows {
        assert_eq!(Table.tokens(tokens), words);
        assert_eq!(Table.share(tokens, all), share, "{words}");
    }
}

/// An agent's finished row in the conversation, `Done (6 tool uses · 30.3k tokens · 1m 2s)`.
#[test]
fn claude_codes_rows_read_as_it_writes_them() {
    for (tokens, secs, words, took) in [
        (54_600, 21, "54.6k", "21s"),
        (30_300, 62, "30.3k", "1m 2s"),
        (22_400, 160, "22.4k", "2m 40s"),
    ] {
        assert_eq!(ClaudeCode.tokens(tokens), words);
        assert_eq!(ClaudeCode.duration(Duration::from_secs(secs)), took);
    }
}

/// Each CLI's parts on the Context tab as drawn, in tenths of its window, with a part too
/// small for a tenth still showing; and how full the window is, in whole percents, as the
/// tab's first line, the crew's rows and the context mod all write it.
#[test]
fn the_context_tab_writes_each_part_as_drawn() {
    let claude_code = (
        1_000_000,
        32_298,
        &[
            (4_380, "0.4%"),
            (19_380, "1.9%"),
            (705, "0.1%"),
            (960, "0.1%"),
            (503, "0.1%"),
            (6_360, "0.6%"),
            (10, "<0.1%"),
            (934_702, "93.5%"),
            (33_000, "3.3%"),
        ][..],
        "3%",
    );
    let codex = (
        872_000,
        61_400,
        &[
            (15_236, "1.7%"),
            (1_200, "0.1%"),
            (44_964, "5.2%"),
            (810_600, "93.0%"),
        ][..],
        "7%",
    );
    let opencode = (
        65_536,
        23_700,
        &[
            (7_743, "11.8%"),
            (1_200, "1.8%"),
            (14_757, "22.5%"),
            (41_836, "63.8%"),
        ][..],
        "36%",
    );
    let agy = (
        1_000_000,
        38_900,
        &[
            (4_226, "0.4%"),
            (1_200, "0.1%"),
            (33_474, "3.3%"),
            (961_100, "96.1%"),
        ][..],
        "4%",
    );
    for (window, used, parts, full) in [claude_code, codex, opencode, agy] {
        for &(tokens, share) in parts {
            assert_eq!(
                ClaudeCode.share(tokens, window),
                share,
                "{tokens} of {window}"
            );
        }
        assert_eq!(Table.share(used, window), full, "{used} of {window}");
    }
    for (used, window, full) in [
        (13_300, 1_000_000, "1%"),
        (30_500, 872_000, "3%"),
        (21_000, 1_000_000, "2%"),
    ] {
        assert_eq!(Table.share(used, window), full, "{used} of {window}");
    }
}

/// The mods as drawn: spend by CLI, tokens by kind with the day's total, and checks by how
/// they ended.
#[test]
fn the_mods_write_their_figures_as_drawn() {
    let spent = |cost_usd, cost_estimated| Spend {
        cost_usd,
        cost_estimated,
        ..Spend::default()
    };
    for (cents, cost, words, share) in [
        (131, spent(1.31, true), "~$1.31", "70%"),
        (45, spent(0.45, false), "$0.45", "24%"),
        (10, spent(0.10, true), "~$0.10", "5%"),
    ] {
        assert_eq!(Table.spend(&cost), words);
        assert_eq!(Table.share(cents, 186), share, "{words}");
    }
    assert_eq!(Table.spend(&spent(1.86, true)), "~$1.86");
    for (tokens, words, share) in [
        (950_000, "950k", "68%"),
        (380_000, "380k", "27%"),
        (70_000, "70k", "5%"),
    ] {
        assert_eq!(Prose.tokens(tokens), words);
        assert_eq!(Table.share(tokens, 1_400_000), share, "{words}");
    }
    assert_eq!(Prose.tokens(1_400_000), "1.4M");
    for (runs, share) in [(7, "78%"), (1, "11%")] {
        assert_eq!(Table.share(runs, 9), share, "{runs} of 9");
    }
}

#[test]
fn prose_says_how_long_ago_in_words() {
    assert_eq!(
        Prose.ago(Duration::from_secs(8 * DAY + 5 * HOUR)),
        "8 days ago"
    );
    assert_eq!(Prose.ago(Duration::from_secs(3 * MINUTE + 59)), "3 min ago");
}
