use argonauts_core::api::UsagePeriod;
use argonauts_core::crew_stats::CrewRole;
use crossterm::event::KeyCode;
use time::macros::{date, datetime};
use time::{Date, Duration, OffsetDateTime};

use super::golden::*;

/// The drawings' width, and the screen that draws the panel that wide.
const WIDTH: usize = 118;
const SCREEN: u16 = WIDTH as u16 + 2;
/// The panel's rule is the screen's first row, a row above the drawing's first.
const AT: Offset = Offset {
    rows: 1,
    columns: 1,
};

/// The words at the start of row `row` that the tab writes: the line under the tabs up to
/// the sub-tabs, and the keys up to the panel's own way out.
fn words(html: &str, row: usize) -> Region {
    let text: String = drawing(html)[row].iter().map(|cell| cell.ch).collect();
    let end = ["   ", " · esc"]
        .iter()
        .filter_map(|stop| text.find(stop))
        .min()
        .unwrap_or(text.len());
    cells(row, 0..text[..end].chars().count())
}

/// The periods at the right of the tabs: `Today  Week  Month  All`, the one shown in the
/// accent.
fn periods(html: &str) -> Region {
    let text: String = drawing(html)[0].iter().map(|cell| cell.ch).collect();
    let start = text[..text.find("Today").expect("the periods")]
        .chars()
        .count();
    cells(0, start..text.chars().count())
}

/// A Friday.
fn friday() -> OffsetDateTime {
    datetime!(2026-10-09 12:00 UTC)
}

/// The Sundays the ten weeks up to `friday` start on.
fn weeks() -> Vec<Date> {
    (0..10)
        .rev()
        .map(|back| date!(2026 - 10 - 04) - Duration::weeks(back))
        .collect()
}

/// Every model as the by-model drawing has it, over every day so far.
fn by_model_example(now: OffsetDateTime) -> Example {
    use CrewRole::{Judge, Orchestrator, Reviewer, Worker};
    let mut example = Example::default();
    example.work(OPUS, Orchestrator, None, Default::default(), (41, 0, 110));
    example.work(
        OPUS,
        Worker,
        Some("build"),
        tokens(41_200_000, 84.0, true),
        (312, 0, 280),
    );
    example.tasks(OPUS, "build", 293, 17, 2);
    let gpt = spent(15_900_000, 6_600_000, 900_000, 31.0, false);
    example.work(GPT, Worker, Some("build"), gpt, (152, 0, 200));
    example.work(GPT, Worker, Some("tests"), Default::default(), (78, 0, 200));
    example.work(GPT, Worker, Some("docs"), Default::default(), (38, 0, 200));
    example.work(GPT, Reviewer, None, Default::default(), (118, 9, 52));
    example.tasks(GPT, "build", 137, 13, 2);
    example.tasks(GPT, "tests", 72, 6, 0);
    example.tasks(GPT, "docs", 32, 5, 1);
    example.work(
        SONNET,
        Worker,
        Some("build"),
        tokens(15_600_000, 19.0, true),
        (141, 0, 190),
    );
    example.tasks(SONNET, "build", 128, 12, 1);
    example.work(
        PRO,
        Reviewer,
        None,
        tokens(8_300_000, 6.10, true),
        (612, 41, 44),
    );
    example.work(
        LUNA,
        Worker,
        Some("build"),
        tokens(7_100_000, 5.90, false),
        (93, 0, 135),
    );
    example.tasks(LUNA, "build", 80, 11, 2);
    example.work(
        FLASH,
        Judge,
        None,
        tokens(2_400_000, 0.90, true),
        (177, 9, 30),
    );
    example.fallbacks(GPT, OPUS, 6);
    example.plan(CODEX, "Plus", false, [0.01, 0.11], now);
    // Each week of GPT 5.6's tokens on its Monday, the busiest day its own, and Opus 5.5's
    // busiest week the scale they're drawn on.
    let unit = 100_000;
    let gpt_weeks = [3, 4, 1, 4, 5, 1, 3, 4, 6, 3];
    for (sunday, n) in weeks().into_iter().zip(gpt_weeks) {
        example.day(GPT, Worker, sunday + Duration::days(1), n * unit, 1);
    }
    example.day(GPT, Worker, date!(2026 - 10 - 06), 4 * unit, 1);
    example.day(OPUS, Worker, weeks()[0] + Duration::days(1), 8 * unit, 1);
    example
}

#[test]
fn by_model_matches_its_drawing() {
    let theme = theme();
    let now = friday();
    let usage = by_model_example(now).usage(UsagePeriod::All);
    // The picked row stands opened under the table.
    let keys = [KeyCode::Char('m'), KeyCode::Down];
    let screen = screen(&theme, usage, now, &keys, SCREEN);
    let html = include_str!("screens/by_model_after.html");
    dump("by_model_after", &screen, AT, html);
    assert_matches("by model", html, &theme, &screen, AT, rows(3..25));
    assert_matches("by model", html, &theme, &screen, AT, words(html, 1));
    assert_matches("by model", html, &theme, &screen, AT, periods(html));
    assert_matches("by model", html, &theme, &screen, AT, words(html, 26));
}

/// Every role as the by-role drawing has it, over every day so far.
fn by_role_example() -> Example {
    use CrewRole::{Judge, Orchestrator, Reviewer, Worker};
    let mut example = Example::default();
    example.work(
        OPUS,
        Orchestrator,
        None,
        tokens(4_900_000, 11.0, true),
        (41, 0, 110),
    );
    for (model, millions, cost, did, fared) in [
        (OPUS, 37_100_000, 80.0, 312, (293, 17, 2)),
        (SONNET, 15_600_000, 19.0, 141, (128, 12, 1)),
        (GPT, 19_800_000, 23.0, 268, (241, 24, 3)),
        (LUNA, 7_100_000, 6.0, 93, (80, 11, 2)),
    ] {
        example.work(
            model,
            Worker,
            Some("build"),
            tokens(millions, cost, true),
            (did, 0, 220),
        );
        example.tasks(model, "build", fared.0, fared.1, fared.2);
    }
    let pro = spent(6_100_000, 1_900_000, 300_000, 6.10, true);
    let review = &mut example.work(PRO, Reviewer, None, pro, (612, 41, 46)).review;
    review.reasons = [
        ("a test missing".to_owned(), 23),
        ("a check failed".to_owned(), 15),
        ("didn't do the task".to_owned(), 3),
    ]
    .into();
    review.checks = vec!["node --test".to_owned(), "pytest".to_owned()];
    (review.passed, review.failed, review.ran_own) = (590, 22, 15);
    let gpt = spent(2_700_000, 800_000, 100_000, 3.30, false);
    let review = &mut example.work(GPT, Reviewer, None, gpt, (118, 9, 46)).review;
    review.reasons = [("didn't do the task".to_owned(), 9)].into();
    review.checks = vec!["pytest".to_owned()];
    (review.passed, review.failed, review.ran_own) = (98, 20, 4);
    example.work(
        FLASH,
        Judge,
        None,
        tokens(2_400_000, 0.90, true),
        (177, 9, 30),
    );
    // The reviews of each week on its Monday, the last week's on the busiest day, and the
    // workers' busiest week the scale they're drawn on.
    let reviews = [2, 3, 1, 3, 4, 1, 2, 3, 4, 5];
    for (sunday, n) in weeks().into_iter().zip(reviews) {
        example.day(PRO, Reviewer, sunday + Duration::days(1), 0, n);
    }
    example.day(OPUS, Worker, weeks()[0] + Duration::days(2), 0, 8);
    example
}

#[test]
fn by_role_matches_its_drawing() {
    let theme = theme();
    let usage = by_role_example().usage(UsagePeriod::All);
    let keys = [KeyCode::Char('r'), KeyCode::Down, KeyCode::Down];
    let screen = screen(&theme, usage, friday(), &keys, SCREEN);
    let html = include_str!("screens/by_role_after.html");
    dump("by_role_after", &screen, AT, html);
    assert_matches("by role", html, &theme, &screen, AT, rows(3..31));
    assert_matches("by role", html, &theme, &screen, AT, words(html, 1));
    assert_matches("by role", html, &theme, &screen, AT, words(html, 32));
}

/// A Friday a week on, so the weeks so far begin on August 9th.
fn next_friday() -> OffsetDateTime {
    datetime!(2026-10-16 12:00 UTC)
}

/// Tokens in all, 68 in each hundred from cache, 28 new and 4 output.
fn split(total: u64, cost: f64, estimated: bool) -> argonauts_core::Spend {
    let cached = total * 68 / 100;
    let output = total * 4 / 100;
    spent(cached, total - cached - output, output, cost, estimated)
}

/// Every CLI as the by-CLI screen has it, over every day so far.
fn by_cli_example(now: OffsetDateTime) -> Example {
    use CrewRole::{Judge, Orchestrator, Reviewer, Worker};
    let mut example = Example::default();
    example.work(OPUS, Orchestrator, None, Default::default(), (14, 0, 0));
    example.work(
        OPUS,
        Worker,
        Some("build"),
        split(2_100_000, 5.90, true),
        (24, 0, 252),
    );
    example.tasks(OPUS, "build", 23, 1, 0);
    example.work(
        SONNET,
        Worker,
        Some("build"),
        split(800_000, 2.00, true),
        (11, 0, 252),
    );
    example.tasks(SONNET, "build", 10, 1, 0);
    example.work(
        GPT,
        Worker,
        Some("build"),
        split(1_200_000, 2.30, false),
        (21, 0, 185),
    );
    example.work(GPT, Reviewer, None, Default::default(), (14, 2, 50));
    example.tasks(GPT, "build", 19, 2, 0);
    example.work(
        LUNA,
        Worker,
        Some("build"),
        split(350_000, 0.45, false),
        (7, 0, 185),
    );
    example.tasks(LUNA, "build", 6, 0, 1);
    example.work(PRO, Reviewer, None, split(430_000, 0.47, true), (49, 4, 41));
    example.work(FLASH, Judge, None, split(120_000, 0.08, true), (14, 1, 41));
    example.stats.workflows = 14;
    example.stats.took_secs = 12 * 3600 + 40 * 60;
    example.plan(CLAUDE, "Max", true, [0.01, 0.56], now);
    example.plan(CODEX, "Plus", false, [0.01, 0.11], now);
    example.plan(AGY, "Pro", false, [0.02, 0.01], now);
    // Each week's tokens by CLI for the chart, and its pieces of work for the cards, on its
    // Monday.
    let tokens = [
        (9, 5, 2),
        (12, 6, 2),
        (8, 7, 1),
        (14, 8, 3),
        (11, 9, 2),
        (16, 7, 3),
        (13, 10, 4),
        (18, 9, 3),
        (15, 11, 4),
        (20, 12, 5),
    ];
    let claude = [3, 5, 2, 4, 7, 1, 3, 5, 6, 8];
    let codex = [3, 4, 1, 4, 5, 1, 3, 4, 6, 7];
    let agy = [2, 3, 1, 3, 4, 1, 2, 3, 4, 5];
    let sundays = (0..10)
        .rev()
        .map(|back| date!(2026 - 10 - 11) - Duration::weeks(back));
    for (k, sunday) in sundays.enumerate() {
        let monday = sunday + Duration::days(1);
        let unit = 100_000;
        example.day(OPUS, Worker, monday, tokens[k].0 * unit, claude[k]);
        example.day(GPT, Worker, monday, tokens[k].1 * unit, codex[k]);
        example.day(PRO, Reviewer, monday, tokens[k].2 * unit, agy[k]);
    }
    example
}

#[test]
fn by_cli_matches_its_drawing() {
    let theme = theme();
    let now = next_friday();
    let usage = by_cli_example(now).usage(UsagePeriod::All);
    let screen = screen(&theme, usage, now, &[], SCREEN);
    let html = include_str!("screens/by_cli_after.html");
    dump("by_cli_after", &screen, AT, html);
    assert_matches("by CLI", html, &theme, &screen, AT, words(html, 1));
    assert_matches("by CLI", html, &theme, &screen, AT, periods(html));
    // What ran through Argonauts has its card under who did what, which the drawing ends
    // before, so its keys are read where the screen has them.
    let keys = Offset {
        rows: row_of(&screen, "1-4 period") as isize - 48,
        columns: 1,
    };
    assert_matches("by CLI", html, &theme, &screen, keys, words(html, 48));
    assert_matches("by CLI", html, &theme, &screen, AT, rows(3..18));
    // The drawing leaves each card's dot without its CLI's color, which the two cards'
    // drawing gives it, and names the plan's weekly window `7d`, where the Usage tab's name
    // for it, which every card uses, is `week`.
    let plans = [
        "row 29:",
        "  drawing |│ 5h ━━━━━━  1%   7d ━━━━━━ 56%      │  │ 5h ━━━━━━  1%   7d ━━━━━━ 11%      │  │ 5h ━━━━━━  2%   7d ━━━━━━  1%      │|",
        "  drawn   |│ 5h ━━━━━━  1%   week ━━━━━━ 56%    │  │ 5h ━━━━━━  1%   week ━━━━━━ 11%    │  │ 5h ━━━━━━  2%   week ━━━━━━  1%    │|",
    ]
    .join("\n");
    let cards = [
        "row 20 col 2 '●': drawing None",
        "row 20 col 42 '○': drawing None",
        "row 20 col 82 '○': drawing None",
        &plans,
    ];
    assert_differs_only("by CLI", html, &theme, &screen, AT, rows(19..32), &cards);
    // The drawing gives Sonnet 5.5's 11 of the 63 tasks 18 in each hundred where they are
    // 17, so its part of the workers' bar is a cell longer there, and GPT 5.6's starts a
    // cell later.
    let known = [
        "row 38 col 72 '━': drawing Some(\"m-sonnet\")",
        "row 38 col 104 '━': drawing Some(\"m-gpt\")",
    ];
    assert_differs_only("by CLI", html, &theme, &screen, AT, rows(32..47), &known);
}

/// This week's numbers, each day's tokens by CLI from Sunday up to Friday.
fn week_example() -> Example {
    use CrewRole::{Reviewer, Worker};
    let mut example = Example::default();
    let unit = 10_000;
    example.work(
        OPUS,
        Worker,
        Some("build"),
        spent(44 * unit, 18 * unit, 2 * unit, 0.0, false),
        (19, 0, 0),
    );
    example.tasks(OPUS, "build", 17, 2, 0);
    example.work(
        GPT,
        Reviewer,
        None,
        spent(23 * unit, 10 * unit, unit, 0.0, false),
        (19, 2, 0),
    );
    example.work(
        PRO,
        Reviewer,
        None,
        spent(8 * unit, 3 * unit, unit, 0.0, false),
        (4, 0, 0),
    );
    example.stats.workflows = 4;
    let days = [
        (2, 1, 0),
        (6, 4, 1),
        (9, 5, 2),
        (5, 6, 1),
        (11, 6, 3),
        (8, 4, 2),
    ];
    for (k, (claude, codex, agy)) in days.into_iter().enumerate() {
        let day = date!(2026 - 10 - 04) + Duration::days(k as i64);
        example.day(OPUS, Worker, day, claude * unit, 0);
        example.day(GPT, Reviewer, day, codex * unit, 0);
        example.day(PRO, Reviewer, day, agy * unit, 0);
    }
    example
}

#[test]
fn the_week_matches_its_drawing() {
    let theme = theme();
    let usage = week_example().usage(UsagePeriod::Week);
    let screen = screen(&theme, usage, friday(), &[], SCREEN);
    let html = include_str!("screens/week_card.html");
    let at = Offset {
        rows: 4,
        columns: 1,
    };
    // The drawing's bar of how the tasks fared has the shares of every day so far, while
    // its numbers under it are this week's: 17 of 19 passed first time and none was sent
    // back twice, so the bar has two cells fewer in green and none in red.
    let known = [
        "row 12 col 107 '━': drawing Some(\"term-grn\")",
        "row 12 col 108 '━': drawing Some(\"term-grn\")",
        "row 12 col 114 '━': drawing Some(\"term-red\")",
        "row 12 col 115 '━': drawing Some(\"term-red\")",
    ];
    dump("week_card", &screen, at, html);
    assert_differs_only("the week", html, &theme, &screen, at, rows(0..15), &known);
}

#[test]
fn the_all_together_card_up_close_matches_its_drawing() {
    use CrewRole::{Reviewer, Worker};
    let theme = theme();
    let mut example = Example::default();
    let claude = spent(2_610_000, 520_000, 114_000, 0.17, false);
    example.work(HAIKU, Worker, Some("build"), claude, (1, 0, 0));
    example.tasks(HAIKU, "build", 0, 1, 0);
    let codex = spent(98_000, 24_000, 2_000, 0.02, true);
    example.work(LUNA, Reviewer, None, codex, (3, 1, 0));
    let usage = example.usage(UsagePeriod::Week);
    // Two cells wider, so the bars beside the chart are 84 cells.
    let screen = screen(&theme, usage, friday(), &[], SCREEN + 2);
    let html = include_str!("screens/together_zoom_after.html");
    let at = Offset {
        rows: 7,
        columns: 35,
    };
    dump("together_zoom_after", &screen, at, html);
    assert_matches("up close", html, &theme, &screen, at, rows(0..11));
}

/// Claude Code and Codex this week, with the numbers of a run where Claude Code built two
/// tasks and reviewed nine, and Codex only reviewed.
fn two_cards_example(now: OffsetDateTime) -> Example {
    use CrewRole::{Reviewer, Worker};
    let mut example = Example::default();
    example.work(
        HAIKU,
        Worker,
        Some("build"),
        tokens(3_240_000, 0.17, false),
        (2, 0, 173),
    );
    example.tasks(HAIKU, "build", 0, 2, 0);
    example.work(HAIKU, Reviewer, None, Default::default(), (9, 2, 0));
    example.work(
        LUNA,
        Reviewer,
        None,
        tokens(120_000, 0.02, true),
        (3, 1, 12),
    );
    example.plan(CLAUDE, "Max", true, [0.06, 0.37], now);
    example.plan(CODEX, "Plus", false, [0.0, 0.01], now);
    let claude = [1, 1, 1, 1, 3, 8];
    let codex = [1, 1, 1, 1, 2, 8];
    for (k, (claude, codex)) in claude.into_iter().zip(codex).enumerate() {
        let day = date!(2026 - 10 - 04) + Duration::days(k as i64);
        example.day(HAIKU, Worker, day, 0, claude);
        example.day(LUNA, Reviewer, day, 0, codex);
    }
    example
}

#[test]
fn two_cards_match_their_drawing() {
    let theme = theme();
    let now = friday();
    let usage = two_cards_example(now).usage(UsagePeriod::Week);
    // Two cards of 56 cells and the two between them.
    let screen = screen(&theme, usage, now, &[], 116);
    let html = include_str!("screens/cards_after.html");
    let at = Offset {
        rows: row_of(&screen, "● Claude Code") as isize - 1,
        columns: 1,
    };
    // The drawing colours GPT 5.6 Luna in the shade it has beside GPT 5.6 on the all-together
    // drawing, though it is Codex's only model here; every card gives its first model its
    // CLI's first shade, as the drawing itself does for Haiku 5.5 on Claude Code's card.
    let luna: Vec<String> =
        std::iter::once("row 4 col 60 '■': drawing Some(\"m-luna\")".to_owned())
            .chain((76..=90).map(|col| format!("row 4 col {col} '━': drawing Some(\"m-luna\")")))
            .collect();
    let known: Vec<&str> = luna.iter().map(String::as_str).collect();
    dump("cards_after", &screen, at, html);
    assert_differs_only("two cards", html, &theme, &screen, at, rows(0..11), &known);
}

#[test]
fn the_rows_of_how_work_fared_match_their_drawing() {
    let theme = theme();
    let kit = crate::widgets::Kit::new(&theme);
    let fared = |first, once, more| super::draw::Fared { first, once, more };
    let lines = vec![
        super::cli::outcome_row(&kit, "tasks", fared(0, 2, 0), "first pass"),
        super::cli::outcome_row(&kit, "reviews", fared(2, 1, 0), "accepted"),
    ];
    let screen = lines_screen(lines, 40);
    let html = include_str!("screens/zoom_first_after.html");
    let at = Offset {
        rows: 0,
        columns: 0,
    };
    dump("zoom_first_after", &screen, at, html);
    assert_matches("how work fared", html, &theme, &screen, at, rows(0..2));
}

/// A model with a long name is cut to the widest column the drawings give names, with a blank
/// before its bar, so every card keeps its bars and counts at every width.
#[test]
fn a_long_model_name_leaves_every_card_its_bars() {
    let theme = theme();
    let now = next_friday();
    let long = (CLAUDE, "opus-extended-thinking-5.5");
    for width in [80u16, 100, 120, 140] {
        let mut example = by_cli_example(now);
        example.work(
            long,
            CrewRole::Worker,
            Some("build"),
            tokens(900_000, 1.0, false),
            (3, 0, 60),
        );
        let screen = screen(&theme, example.usage(UsagePeriod::All), now, &[], width);
        let shown = text(&screen);
        let rows: Vec<&str> = shown.lines().filter(|line| line.contains("│ ■ ")).collect();
        assert!(rows.len() >= 3, "{width}:\n{shown}");
        for row in rows {
            assert!(row.contains("━━━━"), "{width}: a bar on {row}");
            assert_eq!(
                crate::widgets::bars::touching_bar(row),
                None,
                "{width}: {row}"
            );
        }
        assert!(
            shown.contains("… ━"),
            "{width}: the long name is cut:\n{shown}"
        );
    }
}

/// Opened, a model's counts each keep a blank before the words after them, however wide the
/// count, and its tokens split stays on one row.
#[test]
fn an_opened_model_keeps_a_blank_after_each_count() {
    let theme = theme();
    let now = friday();
    for width in [80u16, 100, 120, 140] {
        let usage = by_model_example(now).usage(UsagePeriod::All);
        let screen = screen(&theme, usage, now, &[KeyCode::Char('m')], width);
        let shown = text(&screen);
        let row = shown
            .lines()
            .find(|line| line.contains("│ orchestrator"))
            .unwrap_or_else(|| panic!("{width}: Opus 5.5 opened:\n{shown}"));
        assert!(row.contains("41 workflows "), "{width}: {row}");
        assert!(!row.contains("workflows1"), "{width}: {row}");
        for line in shown.lines() {
            assert_eq!(
                crate::widgets::bars::touching_bar(line),
                None,
                "{width}: {line}"
            );
        }
    }
    let usage = by_model_example(now).usage(UsagePeriod::All);
    let screen = screen(&theme, usage, now, &[KeyCode::Char('m')], SCREEN);
    let shown = text(&screen);
    let split = shown
        .lines()
        .find(|line| line.contains("■ from cache"))
        .expect("the tokens split");
    assert!(split.contains("■ output"), "one row: {split}");
}
