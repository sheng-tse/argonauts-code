use argonauts_core::api::UsagePeriod;
use argonauts_core::crew_stats::StatsRole;
use time::macros::{date, datetime};
use time::{Duration, OffsetDateTime};

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
    use StatsRole::{Judge, Orchestrator, Reviewer, Worker};
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
    let now = next_friday();
    let usage = by_cli_example(now).usage(UsagePeriod::All);
    let screen = screen(&theme(), usage, now, &[], SCREEN);
    dump(
        "by_cli_after",
        &screen,
        AT,
        include_str!("../../../tests/golden/stats/by_cli_after.html"),
    );
    assert_by_cli(&screen);
}

/// The records spell Opus 5.5 three ways, as the alias `opus`, as `claude-opus-5-5` and with
/// a date, and a row that spent nothing as a reviewer did no review: the screen is the one
/// the drawing has, a row for Opus 5.5 wherever it shows and none that counts nothing.
#[test]
fn a_model_spelled_three_ways_draws_as_one() {
    use argonauts_core::model_catalog::CatalogEntry;
    use argonauts_core::names::ModelRef;

    let now = next_friday();
    let mut example = by_cli_example(now);
    let stats = &mut example.stats;
    let is_opus = |model: &Option<String>| model.as_deref() == Some(OPUS.1);
    let dated = || Some("claude-opus-5-5-20261001".to_owned());
    for row in stats.work.iter_mut().filter(|row| is_opus(&row.model)) {
        if row.role == StatsRole::Orchestrator {
            row.model = Some("opus".to_owned());
        }
    }
    for task in stats.tasks.iter_mut().filter(|task| is_opus(&task.model)) {
        task.model = dated();
    }
    let days = stats.days.iter_mut().filter(|part| is_opus(&part.model));
    for (k, part) in days.enumerate() {
        part.model = if k % 2 == 0 {
            dated()
        } else {
            Some("opus".to_owned())
        };
    }
    example.work(
        (CLAUDE, "opus"),
        StatsRole::Reviewer,
        None,
        Default::default(),
        (0, 0, 0),
    );

    let mut names = names();
    let mut opus = CatalogEntry::new(ModelRef::new(CLAUDE, Some(OPUS.1.to_owned())));
    opus.name = Some("Opus 5.5".to_owned());
    opus.aliases = Some(vec!["opus".to_owned()]);
    names.catalog.models.push(opus);
    let usage = example.usage(UsagePeriod::All);
    let screen = screen_named(&theme(), usage, now, &[], (SCREEN, None), names);
    assert_eq!(text(&screen).matches("Opus 5.5").count(), {
        let once = by_cli_example(now).usage(UsagePeriod::All);
        text(&screen_named(
            &theme(),
            once,
            now,
            &[],
            (SCREEN, None),
            self::names(),
        ))
        .matches("Opus 5.5")
        .count()
    });
    assert_by_cli(&screen);
}

/// The By CLI screen of [`by_cli_example`] against its drawing.
fn assert_by_cli(screen: &ratatui::buffer::Buffer) {
    let theme = theme();
    let screen = screen.clone();
    let html = include_str!("../../../tests/golden/stats/by_cli_after.html");
    assert_matches("by CLI", html, &theme, &screen, AT, words(html, 1));
    assert_matches("by CLI", html, &theme, &screen, AT, periods(html));
    // The card's first row has its own drawing since: over every day so far it says whose
    // work this is.
    assert_matches("by CLI", html, &theme, &screen, AT, rows(3..4));
    assert_matches("by CLI", html, &theme, &screen, AT, rows(5..18));
    // The drawing leaves each card's dot without its CLI's color, which the one card's
    // drawing gives it.
    let dots = [
        "row 20 col 2 '●': drawing None",
        "row 20 col 42 '○': drawing None",
        "row 20 col 82 '○': drawing None",
    ];
    assert_different_only_in("by CLI", html, &theme, &screen, AT, rows(19..27), &dots);
    // A card has a blank row between its parts since, so what follows its tasks is a row
    // lower for each part before it. Its plan's row is the one card's drawing's too.
    let down = |by: isize| Offset {
        rows: AT.rows + by,
        columns: AT.columns,
    };
    assert_matches("by CLI", html, &theme, &screen, down(1), rows(27..28));
    assert_matches("by CLI", html, &theme, &screen, down(2), rows(28..29));
    assert_matches("by CLI", html, &theme, &screen, down(3), rows(30..32));
    // The drawing gives Sonnet 5.5's 11 of the 63 tasks 18 of the workers' bar's 98 cells,
    // where they come to 17.1, so its part is a cell longer there and GPT 5.6's a cell
    // shorter.
    let known = ["row 38 col 72 '━': drawing Some(\"m-sonnet\")"];
    assert_different_only_in(
        "by CLI",
        html,
        &theme,
        &screen,
        down(3),
        rows(32..47),
        &known,
    );
}

/// This week's numbers, each day's tokens by CLI from Sunday up to Friday.
fn week_example() -> Example {
    use StatsRole::{Reviewer, Worker};
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
    let html = include_str!("../../../tests/golden/stats/week_card.html");
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
    assert_different_only_in("the week", html, &theme, &screen, at, rows(0..15), &known);
}

#[test]
fn the_all_together_card_up_close_matches_its_drawing() {
    use StatsRole::{Reviewer, Worker};
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
    let html = include_str!("../../../tests/golden/stats/together_zoom_after.html");
    let at = Offset {
        rows: 7,
        columns: 35,
    };
    dump("together_zoom_after", &screen, at, html);
    assert_matches("up close", html, &theme, &screen, at, rows(0..11));
}

/// A Saturday, so each of its week's seven days has its bar.
fn saturday() -> OffsetDateTime {
    datetime!(2026-10-10 12:00 UTC)
}

/// Claude Code this week, with the numbers of a run where Haiku 5.5 built three tasks, each
/// sent back once, and gave thirteen reviews.
fn one_card_example(now: OffsetDateTime) -> Example {
    use StatsRole::{Reviewer, Worker};
    let mut example = Example::default();
    example.work(
        HAIKU,
        Worker,
        Some("build"),
        tokens(3_430_000, 0.19, false),
        (3, 0, 175),
    );
    example.tasks(HAIKU, "build", 0, 3, 0);
    example.work(HAIKU, Reviewer, None, Default::default(), (13, 6, 0));
    example.plan(CLAUDE, "Max", true, [0.07, 0.55], now);
    for (k, work) in [1, 1, 1, 1, 2, 3, 8].into_iter().enumerate() {
        let day = date!(2026 - 10 - 04) + Duration::days(k as i64);
        example.day(HAIKU, Worker, day, 0, work);
    }
    example
}

#[test]
fn a_clis_card_matches_its_drawing() {
    let theme = theme();
    let now = saturday();
    let usage = one_card_example(now).usage(UsagePeriod::Week);
    // The one card takes the row: 56 cells, a column in from each side of the screen.
    let screen = screen(&theme, usage, now, &[], 58);
    let html = include_str!("../../../tests/golden/stats/cli_card.html");
    let at = Offset {
        rows: row_of(&screen, "● Claude Code") as isize - 1,
        columns: 1,
    };
    dump("cli_card", &screen, at, html);
    let compared = Compared {
        theme: &theme,
        screen: &screen,
        at,
        named: Some(one_claude_model),
    };
    compared.assert_matches("a CLI's card", html, rows(0..15));
}

/// The card's comparison fails once one cell of its drawing is changed: its character, its
/// color or its weight.
#[test]
fn a_drawing_changed_in_one_cell_no_longer_matches_the_card() {
    use argonauts_testkit::picture;

    let theme = theme();
    let now = saturday();
    let usage = one_card_example(now).usage(UsagePeriod::Week);
    let screen = screen(&theme, usage, now, &[], 58);
    let html = include_str!("../../../tests/golden/stats/cli_card.html");
    let at = Offset {
        rows: row_of(&screen, "● Claude Code") as isize - 1,
        columns: 1,
    };
    let found = |page: &str| {
        let drawing = drawing(page);
        differences_as(
            &theme,
            &drawing,
            &screen,
            at,
            rows(0..15),
            Some(one_claude_model),
        )
    };
    assert!(found(html).is_empty());
    let (row, column) = picture::find(&picture::grid(html), |cell| {
        cell.ch.is_alphanumeric() && cell.color.as_deref() == Some("var(--term-dim)")
    });
    let changed = |edit: &dyn Fn(&mut picture::Cell)| {
        found(&picture::edited(html, |grid| edit(&mut grid[row][column])))
    };
    assert_eq!(
        changed(&|cell| cell.color = Some("var(--term-red)".to_owned())).len(),
        1
    );
    assert_eq!(changed(&|cell| cell.ch = '#').len(), 1);
    assert_eq!(changed(&|cell| cell.bold ^= true).len(), 1);
}

#[test]
fn the_all_together_title_matches_its_drawing() {
    use StatsRole::Worker;
    let theme = theme();
    let now = next_friday();
    let mut example = Example::default();
    example.work(
        HAIKU,
        Worker,
        Some("build"),
        tokens(3_570_000, 0.19, false),
        (3, 0, 175),
    );
    let usage = example.usage(UsagePeriod::All);
    let screen = screen_beside(&theme, usage, now, &[], (SCREEN, Some(69_100_000_000)));
    let html = include_str!("../../../tests/golden/stats/stats_header.html");
    let row = row_of(&screen, "◆ all together") as isize;
    let text: String = drawing(html)[0].iter().map(|cell| cell.ch).collect();
    let words = text.find("   ").expect("the gap between its two sides");
    let numbers = text.rfind("   ").expect("the gap between its two sides") + 3;
    // The drawing is the row by itself, the screen's width. Inside its card the words start
    // two cells in and the numbers end two cells from the end, the gap between them four
    // cells shorter.
    let inside = |columns: isize| Offset { rows: row, columns };
    let words = cells(0, 0..text[..words].chars().count());
    let numbers = cells(0, text[..numbers].chars().count()..WIDTH);
    assert_matches("the title", html, &theme, &screen, inside(3), words);
    assert_matches("the title", html, &theme, &screen, inside(-1), numbers);
}

#[test]
fn the_rows_of_how_work_fared_match_their_drawing() {
    let theme = theme();
    let kit = crate::kit::Kit::new(&theme);
    let fared = |first, once, more| super::draw::Fared { first, once, more };
    let lines = vec![
        super::cli::outcome_row(&kit, "tasks", fared(0, 2, 0), "first pass"),
        super::cli::outcome_row(&kit, "reviews", fared(2, 1, 0), "accepted"),
    ];
    let screen = lines_screen(lines, 40);
    let html = include_str!("../../../tests/golden/stats/zoom_first_after.html");
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
            StatsRole::Worker,
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
            assert_eq!(crate::kit::bars::touching_bar(row), None, "{width}: {row}");
        }
        assert!(
            shown.contains("… ━"),
            "{width}: the long name is cut:\n{shown}"
        );
    }
}

/// The picture of one model in one row, from its numbers: Claude Code's records spell Haiku 5.5
/// two ways, as the alias `haiku` with 3.47M tokens and 3 tasks and as `claude-haiku-5-5` with
/// 2.03M and 6, and the orchestrators are a session of `haiku` that planned nothing,
/// `claude-haiku-5-5` with 2 workflows and GPT 5.6 Luna with 1.
fn one_model_two_ways(now: OffsetDateTime) -> Example {
    use StatsRole::{Orchestrator, Worker};
    let alias = (CLAUDE, "haiku");
    let mut example = Example::default();
    example.work(
        alias,
        Worker,
        Some("build"),
        split(3_470_000, 0.20, false),
        (3, 0, 175),
    );
    example.tasks(alias, "build", 3, 0, 0);
    example.work(
        HAIKU,
        Worker,
        Some("build"),
        split(2_030_000, 0.12, false),
        (6, 0, 175),
    );
    example.tasks(HAIKU, "build", 6, 0, 0);
    example.work(alias, Orchestrator, None, Default::default(), (0, 0, 0));
    example.work(HAIKU, Orchestrator, None, Default::default(), (2, 0, 60));
    example.work(LUNA, Orchestrator, None, Default::default(), (1, 0, 60));
    example.stats.workflows = 3;
    example.plan(CLAUDE, "Max", true, [0.07, 0.55], now);
    example.plan(CODEX, "Plus", false, [0.01, 0.11], now);
    example
}

/// The picture `stats_dupes` sums up two places of the By CLI screen, which its "After" rows
/// stand for: a CLI's card and its rows of models, and who did what's orchestrators. They are
/// compared by what they say, since the picture leaves out what draws around them:
/// - "Claude Code  models" is the card's title row, `● Claude Code` with `5.5M tok` at the
///   right, in the card's frame.
/// - "  ▪ Haiku 5.5     5.50M  · 9 tasks" is the card's one row of Haiku 5.5,
///   `■ Haiku 5.5  ━━━━  5.5M · 9 tasks`: the card's legend square in the model's own shade
///   where the picture has `▪` in `#f3b27a`, a bar between the name and the tokens, and the
///   tokens to two figures as every card writes them, `5.5M` for `5.50M`.
/// - "who did what · orchestrators" is the card `◆ who did what` with `orchestrator` at the
///   left of its row's bar.
/// - "  Haiku 5.5 2 · GPT 5.6 Luna 1" is that row's legend,
///   `■ Haiku 5.5 2   ■ GPT 5.6 Luna 1   of 3 workflows`, a square before each model and the
///   whole the row is a part of after them.
#[test]
fn one_model_spelled_two_ways_is_one_row_as_its_picture_says() {
    use argonauts_core::model_catalog::CatalogEntry;
    use argonauts_core::names::ModelRef;

    let now = next_friday();
    let mut names = names();
    let mut haiku = CatalogEntry::new(ModelRef::new(CLAUDE, Some(HAIKU.1.to_owned())));
    haiku.name = Some("Haiku 5.5".to_owned());
    haiku.aliases = Some(vec!["haiku".to_owned()]);
    names.catalog.models.push(haiku);
    let usage = one_model_two_ways(now).usage(UsagePeriod::All);
    let screen = screen_named(&theme(), usage, now, &[], (SCREEN, None), names);
    let shown: Vec<String> = text(&screen).lines().map(str::to_owned).collect();
    let picture: Vec<&str> = include_str!("../../../tests/golden/stats/stats_dupes.txt")
        .lines()
        .collect();
    let after = picture
        .iter()
        .position(|row| row.starts_with(" After:"))
        .expect("the picture's after");
    let after = &picture[after + 1..];

    // The Claude Code card: one row of Haiku 5.5, with the picture's model, tokens and tasks.
    let card = shown
        .iter()
        .position(|row| row.contains("● Claude Code"))
        .unwrap();
    assert!(after[0].starts_with("Claude Code"), "{after:?}");
    let card_end = card
        + shown[card..]
            .iter()
            .position(|row| row.contains('╰'))
            .unwrap();
    // The card is the left one of two side by side.
    let left = |row: &String| -> String { row.chars().take(SCREEN as usize / 2).collect() };
    let haiku_rows: Vec<String> = shown[card..card_end]
        .iter()
        .map(left)
        .filter(|row| row.contains("Haiku 5.5"))
        .collect();
    assert_eq!(haiku_rows.len(), 1, "{shown:#?}");
    let (name, rest) = after[1]
        .trim_start_matches([' ', '▪'])
        .split_at("Haiku 5.5".len());
    let (tokens, tasks) = rest.trim().split_once("  ").unwrap();
    let row = &haiku_rows[0];
    assert!(row.contains(&format!("■ {name} ")), "{row}");
    let figures = |tokens: &str| -> f64 { tokens.trim_end_matches('M').parse().unwrap() };
    let drawn = row
        .split_whitespace()
        .find(|word| word.ends_with('M') && word.parse::<f64>().is_err())
        .unwrap();
    assert_eq!(figures(drawn), figures(tokens), "{row}");
    assert!(row.contains(&format!("{drawn} {}", tasks.trim())), "{row}");

    // Who did what: the orchestrators are Haiku 5.5 once, with its 2, and GPT 5.6 Luna.
    assert_eq!(after[2], "who did what · orchestrators");
    let who = shown
        .iter()
        .position(|row| row.contains("◆ who did what"))
        .unwrap();
    let orchestrators = who
        + shown[who..]
            .iter()
            .position(|row| row.contains("│ orchestrator "))
            .unwrap();
    let legend = shown[orchestrators + 1].trim_matches([' ', '│']);
    let models: Vec<&str> = legend
        .split('■')
        .map(|part| part.split("   of ").next().unwrap_or_default().trim())
        .filter(|part| !part.is_empty())
        .collect();
    let pictured: Vec<&str> = after[3].trim().split(" · ").collect();
    assert_eq!(models, pictured, "{legend}");
    assert!(legend.ends_with("of 3 workflows"), "{legend}");
    assert!(!shown.iter().any(|row| row.contains("Haiku 5.5 0")));
}
