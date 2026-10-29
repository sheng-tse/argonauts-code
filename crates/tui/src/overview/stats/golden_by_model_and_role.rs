//! By model and By role drawn from their drawings' example numbers and compared with them:
//! the same crew over every day so far, six models in six roles. Then one model of a
//! smaller crew, opened in full.

use argonauts_core::api::UsagePeriod;
use argonauts_core::crew_stats::{Outcomes, StatsRole, WorkRow};
use crossterm::event::KeyCode;
use time::macros::{date, datetime};
use time::{Date, Duration, OffsetDateTime};

use super::golden::*;

/// The drawings' width, and the screen that draws the panel that wide.
const SCREEN: u16 = 118 + 2;

/// A Friday whose ten weeks so far begin on August 9th, so the chart's months are the
/// drawings'.
fn now() -> OffsetDateTime {
    datetime!(2026-10-16 12:00 UTC)
}

/// The Mondays of the ten weeks up to [`now`].
fn mondays() -> Vec<Date> {
    (0..10)
        .rev()
        .map(|back| date!(2026 - 10 - 12) - Duration::weeks(back))
        .collect()
}

/// What `model` did as `role`: `done` pieces of work in `secs` seconds together, `back` of
/// them sending work back, for `tokens` at `cost`. The tokens split as the model's do in all,
/// so each model reads from cache and writes at the rate its card says.
fn work<'a>(
    example: &'a mut Example,
    model: (&str, &str),
    role: StatsRole,
    (tokens, cost, estimated): (u64, f64, bool),
    (done, back, secs): (u32, u32, u64),
) -> &'a mut WorkRow {
    // From cache in each hundred of its input, and written in each thousand of its tokens.
    let (hit, written) = match model {
        OPUS => (71, 688),
        SONNET => (68, 527),
        GPT => (64, 797),
        LUNA => (60, 936),
        PRO => (52, 476),
        _ => (40, 270),
    };
    let output = tokens * written / 1000;
    let cached = (tokens - output) * hit / 100;
    let spent = spent(cached, tokens - output - cached, output, cost, estimated);
    let kind = (role == StatsRole::Worker).then_some("build");
    let row = example.work(model, role, kind, spent, (done, back, 0));
    row.took_secs = secs;
    row
}

/// The crew of both drawings: what each model did in each role, and how the runs ended.
fn crew() -> Example {
    use StatsRole::{Judge, Main, Merger, Orchestrator, Reviewer, Worker};
    let mut example = Example::default();
    let e = &mut example;
    work(e, OPUS, Main, (500_000, 1.10, true), (9, 0, 8_648)).turns = 61;
    work(
        e,
        OPUS,
        Orchestrator,
        (600_000, 1.404, true),
        (14, 0, 1_680),
    );
    work(e, OPUS, Worker, (816_000, 3.40, true), (24, 0, 14_160));
    work(e, OPUS, Merger, (180_000, 0.40, true), (14, 0, 370));
    work(e, SONNET, Worker, (803_000, 1.60, true), (11, 0, 5_720));
    work(e, GPT, Worker, (1_029_000, 2.00, false), (21, 0, 9_870));
    let reviews = &mut work(e, GPT, Reviewer, (168_000, 0.20, false), (14, 2, 616)).review;
    reviews.reasons = [("wrong rounding".to_owned(), 2)].into();
    work(e, LUNA, Worker, (350_000, 0.55, false), (7, 0, 2_520));
    let reviews = &mut work(e, PRO, Reviewer, (430_000, 0.45, true), (49, 4, 1_862)).review;
    reviews.reasons = [("a test missing".to_owned(), 3), ("style".to_owned(), 1)].into();
    work(e, FLASH, Judge, (120_000, 0.10, true), (14, 0, 154));

    example.tasks(OPUS, "build", 23, 1, 0);
    example.sent_back_for(OPUS, &["an edge case"]);
    example.tasks(SONNET, "build", 10, 0, 1);
    example.sent_back_for(SONNET, &["a test missing", "a test missing"]);
    example.tasks(GPT, "build", 19, 2, 0);
    example.sent_back_for(GPT, &["wrong rounding", "wrong rounding"]);
    example.tasks(LUNA, "build", 6, 1, 0);
    example.sent_back_for(LUNA, &["a test missing"]);

    example.stats.workflows = 14;
    example.stats.took_secs = 12 * 3600 + 40 * 60;
    example.stats.outcomes = Outcomes {
        runs: 14,
        runs_merged: 13,
        runs_replanned: 1,
        sided_with_worker: 1,
        sided_with_reviewer: 1,
        merges_clean: 11,
        merges_settled: 3,
        merges_failed: 0,
        checks_failed: 0,
    };
    example
}

/// The six parts of a chart's column for each of the ten weeks, in ten-thousands of tokens,
/// and the pieces of work of each part in each week.
type Weeks = ([[f64; 6]; 10], [[u32; 10]; 6]);

/// Each week's tokens and work, on its Monday, for the six `parts` in the drawing's order.
fn weeks(example: &mut Example, parts: [((&str, &str), StatsRole); 6], (tokens, work): Weeks) {
    for (week, monday) in mondays().into_iter().enumerate() {
        for (part, (model, role)) in parts.into_iter().enumerate() {
            let tokens = (tokens[week][part] * 10_000.0).round() as u64;
            example.day(model, role, monday, tokens, work[part][week]);
        }
    }
}

/// The screen with the keys on the sub-tabs, `right` of them along from By CLI.
fn sub_tab(example: Example, right: usize) -> ratatui::buffer::Buffer {
    sub_tab_at(example, right, SCREEN)
}

/// [`sub_tab`] on a screen `width` cells across.
fn sub_tab_at(example: Example, right: usize, width: u16) -> ratatui::buffer::Buffer {
    let mut keys = vec![KeyCode::Down];
    keys.extend(std::iter::repeat_n(KeyCode::Right, right));
    let usage = example.usage(UsagePeriod::All);
    screen(&theme(), usage, now(), &keys, width)
}

/// `compared`, moved so that row `row` of its drawing, the keys, falls on the screen's row of
/// keys: a screen has them under whatever else it draws.
fn keys_at<'a>(compared: &Compared<'a>, row: usize) -> Compared<'a> {
    let keys = row_of(compared.screen, "r to cycle dates") as isize;
    Compared {
        theme: compared.theme,
        screen: compared.screen,
        at: Offset {
            rows: keys - row as isize,
            columns: compared.at.columns,
        },
        named: compared.named,
    }
}

/// Tab moves the sub-tabs from the cards too, so with the keys in the cards By model stays
/// filled as the drawing has it, whether ↓ or Tab took the keys there.
#[test]
fn in_the_cards_the_chosen_sub_tab_stays_filled() {
    use KeyCode::{Down, Right, Tab};
    let html = include_str!("../../../tests/golden/stats/by_model.html");
    let theme = theme();
    for keys in [[Down, Right, Down], [Down, Down, Tab]] {
        let screen = screen(&theme, crew().usage(UsagePeriod::All), now(), &keys, SCREEN);
        assert!(
            text(&screen).contains("↑↓  model"),
            "{keys:?}: the keys are in the cards"
        );
        let compared = Compared {
            theme: &theme,
            screen: &screen,
            at: Offset {
                rows: 1,
                columns: 1,
            },
            named: Some(models_and_roles),
        };
        compared.assert_matches(&format!("by model after {keys:?}"), html, rows(0..2));
    }
}

#[test]
fn by_model_matches_its_drawing() {
    use StatsRole::{Judge, Reviewer, Worker};
    let mut example = crew();
    let parts = [
        (OPUS, Worker),
        (SONNET, Worker),
        (GPT, Worker),
        (LUNA, Worker),
        (PRO, Reviewer),
        (FLASH, Judge),
    ];
    let tokens = [
        [5.0, 2.0, 3.0, 1.0, 1.0, 0.3],
        [6.0, 2.0, 4.0, 1.0, 1.0, 0.3],
        [5.0, 2.0, 3.0, 1.0, 1.0, 0.3],
        [7.0, 3.0, 4.0, 1.0, 2.0, 0.4],
        [6.0, 2.0, 4.0, 1.0, 2.0, 0.4],
        [8.0, 3.0, 4.0, 1.0, 2.0, 0.4],
        [7.0, 3.0, 5.0, 2.0, 2.0, 0.5],
        [9.0, 3.0, 5.0, 1.0, 2.0, 0.5],
        [8.0, 3.0, 5.0, 2.0, 2.0, 0.5],
        [10.0, 4.0, 6.0, 2.0, 3.0, 0.6001],
    ];
    let work = [
        [3, 5, 2, 4, 7, 1, 3, 5, 6, 8],
        [2, 3, 1, 4, 5, 1, 2, 3, 5, 6],
        [3, 4, 1, 4, 5, 1, 3, 4, 6, 7],
        [1, 2, 1, 2, 3, 1, 1, 2, 3, 4],
        [2, 3, 1, 3, 4, 1, 2, 3, 4, 5],
        [1, 1, 1, 2, 2, 1, 1, 2, 2, 3],
    ];
    weeks(&mut example, parts, (tokens, work));
    let screen = sub_tab(example, 1);
    let html = include_str!("../../../tests/golden/stats/by_model.html");
    // The panel's rule is the screen's first row, a row above the drawing's first.
    let at = Offset {
        rows: 1,
        columns: 1,
    };
    dump("by_model", &screen, at, html);
    let theme = theme();
    let compared = Compared {
        theme: &theme,
        screen: &screen,
        at,
        named: Some(models_and_roles),
    };
    compared.assert_matches("by model", html, rows(0..2));
    keys_at(&compared, 62).assert_matches("by model", html, rows(62..63));
    let known = [
        // 140 tokens a second over 42 minutes would be more than its 0.35M tokens in all.
        reads(html, 45, &[("140 tok/s", "130 tok/s")]),
        // What a piece of work cost is an average, written without the mark of an estimate,
        // as the drawing's other rows have it; and 0.43M tokens over 49 reviews are 8.8k each.
        reads(html, 59, &[("~$0.009     9k  ", "$0.009      8.8k")]),
    ];
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    compared.assert_different_only_in("by model", html, rows(3..61), &known);
}

#[test]
fn by_role_matches_its_drawing() {
    use StatsRole::{Judge, Main, Merger, Orchestrator, Reviewer, Worker};
    let mut example = crew();
    let parts = [
        (OPUS, Main),
        (OPUS, Orchestrator),
        (OPUS, Worker),
        (PRO, Reviewer),
        (FLASH, Judge),
        (OPUS, Merger),
    ];
    let tokens = [
        [2.0, 0.5, 6.0, 1.0, 0.2, 0.3],
        [2.0, 0.6, 7.0, 1.0, 0.2, 0.3],
        [2.0, 0.5, 6.0, 1.0, 0.2, 0.3],
        [3.0, 0.7, 9.0, 2.0, 0.3, 0.4],
        [2.0, 0.6, 8.0, 1.0, 0.3, 0.4],
        [3.0, 0.8, 9.0, 2.0, 0.3, 0.5],
        [3.0, 0.8, 10.0, 2.0, 0.4, 0.5],
        [3.0, 0.9, 11.0, 2.0, 0.4, 0.5],
        [3.0, 0.8, 10.0, 2.0, 0.4, 0.5],
        [4.0, 1.0, 12.0, 3.0, 0.5, 0.6],
    ];
    let work = [
        [2, 3, 1, 3, 4, 2, 3, 4, 5, 6],
        [1, 2, 1, 2, 3, 1, 2, 3, 3, 4],
        [3, 5, 2, 4, 7, 1, 3, 5, 6, 8],
        [2, 3, 1, 3, 4, 1, 2, 3, 4, 5],
        [1, 1, 1, 2, 2, 1, 1, 2, 2, 3],
        [1, 2, 1, 2, 2, 1, 2, 2, 3, 3],
    ];
    weeks(&mut example, parts, (tokens, work));
    let screen = sub_tab(example, 2);
    // The drawing as it was first made, but for three rows of the main role's card, which
    // named a conversation by a word the app no longer uses. Row 30 has the turns, the
    // conversations they were in and how many go to each, `61 in 9 · 7 each`, where it was
    // drawn with the count first; row 32 has `16m each`; and row 33 costs `a conversation`.
    let html = include_str!("../../../tests/golden/stats/by_role.html");
    // The drawing starts at the row under the tabs, two rows down the screen.
    let at = Offset {
        rows: 2,
        columns: 1,
    };
    dump("by_role", &screen, at, html);
    let theme = theme();
    let compared = Compared {
        theme: &theme,
        screen: &screen,
        at,
        named: Some(models_and_roles),
    };
    compared.assert_matches("by role", html, rows(0..1));
    keys_at(&compared, 64).assert_matches("by role", html, rows(64..65));
    // The four models' times a task, as their cards have them, come to 8m 32s over the 63
    // tasks.
    let known = [reads(html, 31, &[("8m 31s a task", "8m 32s a task")])];
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    compared.assert_different_only_in("by role", html, rows(2..50), &known);
    // What each role caught or changed has a table under each bar since, where the drawing
    // has a legend, and is its own picture's.
    let caught = include_str!("../../../tests/golden/stats/caught_or_changed.html");
    let at = Offset {
        rows: row_of(&screen, "◆ what each role caught or changed") as isize - 1,
        columns: 1,
    };
    dump("caught_or_changed", &screen, at, caught);
    let compared = Compared {
        at,
        named: Some(app_drawn),
        ..compared
    };
    compared.assert_matches("by role", caught, rows(0..drawing(caught).len()));
}

/// The main role's row of turns always says how many conversations they were in. Where its
/// card has the room it says what a share is of, and at the drawing's width, one cell short
/// of that, it says `each`.
#[test]
fn the_main_roles_turns_always_say_in_how_many_conversations() {
    let wide = sub_tab_at(crew(), 2, SCREEN + 3);
    let shown = text(&wide);
    assert!(
        shown.contains("│ turns    61 in 9 · 7 a conversation │"),
        "{shown}"
    );
    let drawn = sub_tab(crew(), 2);
    let shown = text(&drawn);
    assert!(
        shown.contains("│ turns    61 in 9 · 7 each          │"),
        "{shown}"
    );
    assert!(shown.contains("│ 9 conversations · 1 model "), "{shown}");
    // Whatever the cards' width, the row counts them.
    for width in [SCREEN - 8, 100, 80] {
        let shown = text(&sub_tab_at(crew(), 2, width));
        assert!(
            (shown.lines()).any(|row| row.contains("│ turns ") && row.contains(" 61 in 9 · 7 ")),
            "{width}:\n{shown}"
        );
    }
    // Cards with room for every row keep 3 blanks after their longest label.
    let roomy = text(&sub_tab_at(crew(), 2, 100));
    assert!(roomy.contains("│ returns   6 · most: "), "{roomy}");
}

/// Three cards to a row end where the box above them does at every width, on By CLI, By
/// model and By role alike: a row's cells left over go to its first cards.
#[test]
fn a_row_of_cards_ends_where_the_box_above_it_does() {
    for width in 110..=143u16 {
        for right in 0..3 {
            let shown = text(&sub_tab_at(crew(), right, width));
            let borders: Vec<&str> = shown
                .lines()
                .map(str::trim_end)
                .filter(|row| row.ends_with('╮') || row.ends_with('╯'))
                .collect();
            assert!(borders.len() >= 6, "{width}, sub-tab {right}:\n{shown}");
            let ends: Vec<usize> = borders.iter().map(|row| row.chars().count()).collect();
            assert!(
                ends.iter().all(|end| *end == usize::from(width) - 1),
                "{width}, sub-tab {right}: {ends:?}\n{shown}"
            );
        }
    }
}

/// A model named as long as a provider's own id for it keeps its numbers in the table of all
/// models at every width: its name gives way in the names' column, what it spent has its
/// thousands marked, and every column of amounts is as wide as its widest, so each row's
/// numbers end where every other row's do.
#[test]
fn a_long_model_name_keeps_its_numbers_in_the_table() {
    let long = (CLAUDE, "sonnet-5-5-20260901-extended-thinking-preview");
    for width in [80u16, 100, 120, 140] {
        let mut example = crew();
        let spent = (89_000_000_000, 12_356.88, true);
        work(
            &mut example,
            long,
            StatsRole::Worker,
            spent,
            (40, 0, 90_000),
        );
        let shown = text(&sub_tab_at(example, 1, width));
        let rows: Vec<&str> = shown.lines().collect();
        let top = rows.iter().position(|row| row.contains("◆ all models"));
        let top = top.unwrap_or_else(|| panic!("{width}:\n{shown}"));
        let bottom = rows[top..].iter().position(|row| row.contains('╰'));
        let together = &rows[top..top + bottom.expect("the box's last row")];
        let row = (together.iter())
            .find(|row| row.contains("~$12,356.88"))
            .unwrap_or_else(|| panic!("{width}:\n{shown}"));
        let numbers: Vec<&str> = row
            .trim_end_matches(['│', ' '])
            .split_whitespace()
            .collect();
        // At 80 columns, and at 120 where the chart stands beside it, amounts this wide leave
        // the names too little room beside the time, so the table gives up its last column.
        let figures = ["89.00b", "100%", "~$12,356.88", "100%", "25h", "0m", "66%"];
        let figures = if matches!(width, 80 | 120) {
            &figures[..4]
        } else {
            &figures[..]
        };
        assert!(numbers.ends_with(figures), "{width}:\n{shown}");
        let ends: Vec<usize> = (together.iter())
            .filter(|row| row.trim_end_matches(['│', ' ']).ends_with('%'))
            .map(|row| row.trim_end_matches(['│', ' ']).chars().count())
            .collect();
        assert_eq!(ends.len(), 7, "a row for each model: {width}:\n{shown}");
        assert!(ends.iter().all(|end| *end == ends[0]), "{width}:\n{shown}");
    }
}

/// A Saturday, so each of its week's seven days has its bar.
fn saturday() -> OffsetDateTime {
    datetime!(2026-10-10 12:00 UTC)
}

/// Haiku 5.5 this week as its opened drawing has it, beside the one other reviewer: what it
/// built and reviewed, and each day's tokens and tasks from Sunday on.
fn opened_example() -> Example {
    use StatsRole::{Judge, Reviewer, Worker};
    let mut example = Example::default();
    let built = spent(2_200_000, 420_000, 80_000, 0.12, false);
    let row = example.work(HAIKU, Worker, Some("build"), built, (3, 0, 175));
    (row.sessions, row.tool_calls) = (3, 123);
    let reviewed = spent(530_000, 90_000, 10_000, 0.052, false);
    example
        .work(HAIKU, Reviewer, None, reviewed, (13, 6, 8))
        .sessions = 13;
    // It signed nothing off to the end, though the try took its tokens and its time.
    let judged = spent(51_000, 39_000, 10_000, 0.018, false);
    example
        .work(HAIKU, Judge, None, judged, (0, 0, 0))
        .took_secs = 1_810;
    example.tasks(HAIKU, "build", 0, 3, 0);
    let why = ["a test missing", "a test missing", "didn't do the task"];
    example.sent_back_for(HAIKU, &why);
    example.work(
        LUNA,
        Reviewer,
        None,
        tokens(120_000, 0.02, true),
        (3, 1, 19),
    );
    let days = [(1, 1), (1, 1), (1, 1), (1, 1), (2, 1), (3, 2), (8, 8)];
    for (k, (tokens, done)) in days.into_iter().enumerate() {
        let day = date!(2026 - 10 - 04) + Duration::days(k as i64);
        example.day(HAIKU, Worker, day, tokens * 10_000, done);
    }
    example
}

#[test]
fn a_model_opened_matches_its_drawing() {
    let usage = opened_example().usage(UsagePeriod::Week);
    // Down to the sub-tabs, along to By model, into the cards, and Enter on the first.
    let keys = [KeyCode::Down, KeyCode::Right, KeyCode::Down, KeyCode::Enter];
    let theme = theme();
    let screen = screen(&theme, usage, saturday(), &keys, SCREEN);
    let html = include_str!("../../../tests/golden/stats/by_model_opened.html");
    let at = Offset {
        rows: row_of(&screen, "██  Haiku 5.5 on Claude Code") as isize - 1,
        columns: 1,
    };
    let compared = Compared {
        theme: &theme,
        screen: &screen,
        at,
        named: Some(one_claude_model),
    };
    // The drawing's left half, its work. Its right half has a legend of the kinds of token
    // where the card has a table since, and is the card's own picture's.
    let work = Region {
        rows: 0..15,
        columns: 0..62,
    };
    compared.assert_matches("a model opened", html, work);
    let card = include_str!("../../../tests/golden/stats/opened_model.html");
    dump("opened_model", &screen, at, card);
    let compared = Compared {
        named: Some(app_drawn),
        ..compared
    };
    compared.assert_matches("a model opened", card, rows(0..drawing(card).len()));
}

#[test]
fn the_period_keys_start_the_key_line_as_their_drawing_has_it() {
    // This month, the keys on the sub-tabs of By CLI.
    let usage = crew().usage(UsagePeriod::Month);
    let theme = theme();
    let screen = screen(&theme, usage, now(), &[KeyCode::Down], SCREEN);
    let html = include_str!("../../../tests/golden/stats/periods_keys.html");
    let at = Offset {
        rows: 1,
        columns: 1,
    };
    dump("periods_keys", &screen, at, html);
    let compared = Compared {
        theme: &theme,
        screen: &screen,
        at,
        named: None,
    };
    compared.assert_matches("the periods", html, rows(0..1));
    keys_at(&compared, 2).assert_matches("the keys", html, rows(2..3));
}
