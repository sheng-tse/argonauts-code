//! By model with many models, drawn from the numbers of its drawings and compared with them:
//! fifteen models at 120 and at 80 columns, and forty by themselves and filtered. Every model
//! keeps a row, in a window of nine whose last row holds the sums of the rows out of view.

use argonauts_core::api::UsagePeriod;
use argonauts_core::crew_stats::StatsRole;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::logos::{Logo, LogoMark};
use argonauts_core::model_catalog::CatalogEntry;
use argonauts_core::names::ModelRef;
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use time::macros::{date, datetime};
use time::{Date, Duration, OffsetDateTime};

use super::golden::*;
use crate::kit::Kit;
use crate::kit::logo::golden::{pictured, with_stand_ins};
use crate::kit::logo::logo_spans;
use crate::theme::Role;

/// The drawings' widths, and the screens that draw the panel that wide.
const SCREEN: u16 = 118 + 2;
const NARROW: u16 = 78 + 2;

/// The CLIs and endpoints of the drawings.
const CC: &str = CLAUDE;
const CX: &str = CODEX;
const AG: &str = AGY;
const OC: &str = "opencode";
const OR: &str = "openrouter";
const OL: &str = "ollama";

/// A model of a drawing: its CLI, its id and the name it shows, its tokens in ten-thousands,
/// what it cost in cents, and its sessions' minutes.
type Row = (&'static str, &'static str, &'static str, u64, u64, u64);

const FIFTEEN: [Row; 15] = [
    (CC, "claude-opus-5-5", "Opus 5.5", 260, 710, 460),
    (CX, "gpt-6-sol", "6 Sol", 145, 290, 190),
    (CC, "claude-sonnet-5-5", "Sonnet 5.5", 110, 205, 125),
    (AG, "gemini-3.8-flash-high", "3.8 Flash High", 95, 80, 62),
    (CX, "gpt-6-luna", "6 Luna", 88, 70, 80),
    (OC, "glm-5.3-flash-free", "GLM 5.3 Flash Free", 70, 0, 55),
    (CC, "claude-haiku-5-5", "Haiku 5.5", 42, 31, 38),
    (AG, "gemini-3.1-pro-high", "3.1 Pro High", 40, 110, 41),
    (
        OC,
        "deepseek-v4.1-flash-free",
        "DeepSeek V4.1 Flash Free",
        33,
        0,
        24,
    ),
    (OR, "z-ai/glm-5", "GLM 5", 30, 62, 18),
    (CX, "gpt-5.6-sol", "5.6 Sol", 21, 40, 22),
    (
        CC,
        "claude-opus-5-5[1m]",
        "Opus 5.5 (1M context)",
        18,
        95,
        14,
    ),
    (OL, "qwen3-coder:30b", "Qwen3 Coder 30B", 15, 0, 35),
    (AG, "gemini-3.8-flash-low", "3.8 Flash Low", 12, 6, 5),
    (CX, "gpt-5.6-luna", "5.6 Luna", 6, 4, 6),
];

const FORTY: [Row; 40] = [
    (CC, "claude-opus-5-5", "Opus 5.5", 420, 1134, 455),
    (CX, "gpt-6-sol", "6 Sol", 240, 480, 316),
    (CC, "claude-sonnet-5-5", "Sonnet 5.5", 180, 333, 204),
    (CX, "gpt-6-luna", "6 Luna", 130, 104, 119),
    (AG, "gemini-3.8-flash-high", "3.8 Flash High", 110, 92, 71),
    (OC, "glm-5.3-flash-free", "GLM 5.3 Flash Free", 92, 0, 72),
    (CC, "claude-haiku-5-5", "Haiku 5.5", 64, 46, 57),
    (OR, "z-ai/glm-5", "GLM 5", 58, 64, 35),
    (CX, "gpt-6-astra", "6 Astra", 52, 182, 46),
    (
        OC,
        "deepseek-v4.1-flash-free",
        "DeepSeek V4.1 Flash Free",
        48,
        0,
        32,
    ),
    (
        AG,
        "gemini-3.8-flash-medium",
        "3.8 Flash Medium",
        46,
        28,
        27,
    ),
    (AG, "gemini-3.1-pro-high", "3.1 Pro High", 44, 121, 44),
    (OR, "moonshotai/kimi-k2.6", "Kimi K2.6", 41, 53, 28),
    (OL, "qwen3-coder:30b", "Qwen3 Coder 30B", 38, 0, 85),
    (OR, "qwen/qwen3-coder", "Qwen3 Coder", 35, 32, 23),
    (
        CC,
        "claude-opus-5-5[1m]",
        "Opus 5.5 (1M context)",
        31,
        161,
        20,
    ),
    (OC, "big-pickle", "Big Pickle", 30, 0, 23),
    (CX, "gpt-5.6-sol", "5.6 Sol", 26, 49, 22),
    (OR, "deepseek/deepseek-v4", "DeepSeek V4", 22, 13, 14),
    (AG, "gemini-3.8-flash-low", "3.8 Flash Low", 21, 9, 9),
    (
        OC,
        "mimo-v2.6-flash-free",
        "MiMo V2.6 Flash Free",
        17,
        0,
        14,
    ),
    (OR, "minimax/minimax-m2", "MiniMax M2", 15, 12, 15),
    (CC, "claude-fable-5-1", "Fable 5.1", 12, 14, 12),
    (OR, "google/gemini-3.6-pro", "Gemini 3.6 Pro", 12, 35, 11),
    (
        OC,
        "nemotron-3.5-super-vl-free",
        "Nemotron 3.5 Super VL Free",
        11,
        0,
        10,
    ),
    (OC, "ollama/qwen3-coder:30b", "Qwen3 Coder 30B", 10, 0, 21),
    (CX, "gpt-5.6-terra", "5.6 Terra", 9, 11, 13),
    (OL, "gemma3:12b", "Gemma3 12B", 9, 0, 25),
    (AG, "gemini-3.1-pro-low", "3.1 Pro Low", 8, 17, 6),
    (AG, "gemini-3.7-flash-high", "3.7 Flash High", 7, 6, 4),
    (
        OC,
        "longcat-2.5-preview-free",
        "Longcat 2.5 Preview Free",
        6,
        0,
        5,
    ),
    (OR, "mistralai/devstral-medium", "Devstral Medium", 6, 5, 7),
    (CX, "gpt-5.6-luna", "5.6 Luna", 5, 3, 10),
    (
        AG,
        "claude-sonnet-4-6-thinking",
        "Sonnet 4.6 Thinking",
        4,
        8,
        5,
    ),
    (
        OC,
        "nemotron-3-ultra-free",
        "Nemotron 3 Ultra Free",
        4,
        0,
        7,
    ),
    (OR, "x-ai/grok-code-fast-1", "Grok Code Fast 1", 4, 2, 3),
    (OC, "ollama/gemma3:12b", "Gemma3 12B", 3, 0, 7),
    (OR, "anthropic/claude-sonnet-5.5", "Sonnet 5.5", 3, 11, 3),
    (AG, "gpt-oss-120b-medium", "GPT OSS 120B Medium", 2, 1, 1),
    (OC, "beam-free", "Beam Free", 1, 0, 3),
];

/// Each CLI's part of the chart's ten weeks, from the bottom, in eighths of the fullest week:
/// Claude Code, Codex, agy and OpenCode, as the drawings stack them.
const WEEKS_FIFTEEN: [[u64; 4]; 10] = [
    [1, 0, 0, 0],
    [2, 0, 0, 0],
    [2, 0, 0, 0],
    [3, 0, 0, 0],
    [3, 0, 0, 0],
    [3, 1, 0, 0],
    [3, 2, 0, 0],
    [3, 3, 0, 0],
    [3, 3, 1, 0],
    [3, 3, 1, 1],
];
const WEEKS_FORTY: [[u64; 4]; 10] = [
    [1, 0, 0, 0],
    [2, 0, 0, 0],
    [2, 0, 0, 0],
    [3, 0, 0, 0],
    [3, 0, 0, 0],
    [3, 2, 0, 0],
    [3, 2, 0, 0],
    [3, 3, 0, 0],
    [3, 3, 1, 0],
    [3, 3, 1, 1],
];

/// A Friday whose ten weeks so far begin on August 9th, so the chart's months are the
/// drawings'.
fn now() -> OffsetDateTime {
    datetime!(2026-10-16 12:00 UTC)
}

fn mondays() -> Vec<Date> {
    (0..10)
        .rev()
        .map(|back| date!(2026 - 10 - 12) - Duration::weeks(back))
        .collect()
}

/// The crew of a drawing: each model's work as a worker, Claude Code's and agy's costs
/// estimated, then the chart's weeks on each CLI's first model, the workflows, the tasks
/// merged and the time they all took.
fn crew(
    rows: &[Row],
    weeks: &[[u64; 4]; 10],
    (workflows, merged, minutes): (u32, u32, u64),
) -> Example {
    let mut example = Example::default();
    for (cli, id, _, tokens, cents, mins) in rows {
        let estimated = [CC, AG].contains(cli);
        let spent = super::golden::tokens(tokens * 10_000, *cents as f64 / 100.0, estimated);
        example.work(
            (cli, id),
            StatsRole::Worker,
            Some("build"),
            spent,
            (1, 0, mins * 60),
        );
    }
    let first = |cli: &str| {
        let (cli, id, ..) = rows
            .iter()
            .find(|row| row.0 == cli)
            .expect("a model of the CLI");
        (*cli, *id)
    };
    for (week, monday) in mondays().into_iter().enumerate() {
        for (at, cli) in [CC, CX, AG, OC].into_iter().enumerate() {
            let tokens = weeks[week][at] * 100_000;
            if tokens > 0 {
                example.day(first(cli), StatsRole::Worker, monday, tokens, 1);
            }
        }
    }
    example.tasks(first(CC), "build", merged, 0, 0);
    example.stats.workflows = workflows;
    example.stats.took_secs = minutes * 60;
    example
}

/// The drawings' CLIs and models, each by the name it shows.
fn named(rows: &[Row]) -> DisplayNames {
    let mut names = names();
    for (id, name) in [(OC, "OpenCode"), (OR, "OpenRouter"), (OL, "Ollama")] {
        names.clis.insert(id.to_owned(), name.to_owned());
    }
    for (cli, id, name, ..) in rows {
        names.catalog.models.push(CatalogEntry {
            name: Some((*name).to_owned()),
            ..CatalogEntry::new(ModelRef::new(*cli, Some((*id).to_owned())))
        });
    }
    names
}

/// By model's screen `width` wide after `keys`, the logos drawn as pictures and written as
/// their stand-ins.
fn by_model(example: Example, rows: &[Row], keys: &[KeyCode], width: u16) -> Buffer {
    let usage = example.usage(UsagePeriod::All);
    let theme = pictured(&theme());
    let screen = screen_named(&theme, usage, now(), keys, (width, None), named(rows));
    with_stand_ins(&screen)
}

/// From the tabs: ↓ to the sub-tabs and → to By model.
const ON_BY_MODEL: [KeyCode; 2] = [KeyCode::Down, KeyCode::Right];

/// The drawings' own names for colours: each CLI's hue, an endpoint's, and a logo's.
fn many_models(kit: &Kit<'_>, name: &str) -> Vec<Option<Color>> {
    let cli = |agent: &str| kit.fg(Role::for_cli(agent)).fg;
    match name {
        "#f7a07f" => vec![cli(CC)],
        "#6a9ee6" => vec![cli(CX)],
        "#6cd1cf" => vec![cli(AG)],
        "#c27cb4" => vec![cli(OC)],
        // The drawings give the endpoints a violet and a sand. The colour set has no hue for
        // an endpoint, so it draws in the grey it gives any CLI it has none for.
        "#a99cf0" => vec![cli(OR)],
        "#d4c08a" => vec![cli(OL)],
        "#8a8fa8" => vec![kit.fg(Role::CatOther).fg],
        "logo" => {
            let theme = pictured(kit.theme);
            (LogoMark::ALL.into_iter())
                .map(|mark| {
                    let logo = Logo {
                        agent: argonauts_core::names::AgentId(String::new()),
                        mark: Some(mark),
                    };
                    logo_spans(&logo, &theme)[0].style.fg
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Where the drawing's box is on the screen: its top row is the one over the title.
fn box_at(screen: &Buffer) -> Offset {
    Offset {
        rows: row_of(screen, "◆ all models") as isize - 1,
        columns: 1,
    }
}

/// The cells where the drawing draws a CLI's models after its third in the CLI's hue. The
/// colour set draws them in the grey of the rest, as its own picture has them.
fn in_the_grey_of_the_rest(html: &str, rows: std::ops::Range<usize>) -> Vec<String> {
    let hues = ["#f7a07f", "#6a9ee6", "#6cd1cf", "#c27cb4"];
    let drawing = drawing(html);
    let mut known = Vec::new();
    for row in rows {
        for (col, cell) in drawing[row].iter().enumerate() {
            let hued = cell
                .color
                .as_deref()
                .is_some_and(|color| hues.contains(&color));
            if cell.ch == '░' && hued {
                known.push(format!("row {row} col {col} '░': drawing {:?}", cell.color));
            }
        }
    }
    known
}

/// The drawing colours the bars' groups in the CLIs' order whatever CLIs a bar has, so where
/// OpenCode's free models leave the spend bar, its last group, OpenRouter's, comes in
/// OpenCode's colour.
fn spend_in_opencode_colour(html: &str) -> Vec<String> {
    let drawing = drawing(html);
    let spend = (drawing.iter())
        .position(|row| {
            row.iter()
                .map(|cell| cell.ch)
                .collect::<String>()
                .contains("spend  ")
        })
        .expect("the spend bar");
    let row = &drawing[spend];
    let last = row
        .iter()
        .rposition(|cell| cell.ch == ' ' && cell.color.is_none());
    (row.iter().enumerate())
        .filter(|(col, cell)| {
            last.is_some_and(|last| *col > 50 && *col < last)
                && cell.color.as_deref() == Some("#c27cb4")
        })
        .map(|(col, cell)| {
            format!(
                "row {spend} col {col} {:?}: drawing {:?}",
                cell.ch, cell.color
            )
        })
        .collect()
}

/// The chart's months, which the drawing writes plain and the app dim.
fn plain_months(html: &str) -> Vec<String> {
    let drawing = drawing(html);
    let mut known = Vec::new();
    for (at, row) in drawing.iter().enumerate() {
        let text: String = row.iter().map(|cell| cell.ch).collect();
        let Some(start) = text.find("│ Aug") else {
            continue;
        };
        let start = text[..start].chars().count() + 2;
        for (col, cell) in row.iter().enumerate().skip(start).take(24) {
            if cell.ch != ' ' && cell.color.is_none() {
                known.push(format!("row {at} col {col} {:?}: drawing None", cell.ch));
            }
        }
    }
    known
}

/// Asserts the screen writes the drawing's row `row` with the same characters, whatever
/// their colours: a title's words, which the app draws bold and dim where the drawing
/// writes them plain.
fn assert_reads(name: &str, html: &str, screen: &Buffer, at: Offset, row: usize) {
    let wanted: String = drawing(html)[row].iter().map(|cell| cell.ch).collect();
    let y = (row as isize + at.rows) as u16;
    let drawn: String = (0..wanted.chars().count())
        .map(|col| {
            let cell = &screen[((col as isize + at.columns) as u16, y)];
            match cell.symbol() {
                "" => " ".to_owned(),
                symbol => symbol.to_owned(),
            }
        })
        .collect();
    assert_eq!(drawn, wanted, "{name}, row {row}:\n{}", text(screen));
}

/// Compares the box with its drawing: the title by its words, every other row cell for cell,
/// different only where `known` says and where the drawing draws a CLI's fourth model on in
/// its hue.
fn assert_box(name: &str, html: &str, screen: &Buffer, known: &[String]) {
    let at = box_at(screen);
    assert_reads(name, html, screen, at, 1);
    let theme = theme();
    let compared = Compared {
        theme: &theme,
        screen,
        at,
        named: Some(many_models),
    };
    let last = drawing(html).len();
    let mut known = known.to_vec();
    known.extend(in_the_grey_of_the_rest(html, 2..last));
    known.extend(spend_in_opencode_colour(html));
    known.extend(plain_months(html));
    known.sort_by_key(|line| differences_order(line));
    known.dedup();
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    compared.assert_different_only_in(name, html, rows(2..last), &known);
}

/// The order [`differences`] finds lines in: by row, a row's characters first, then by column.
fn differences_order(line: &str) -> (usize, usize) {
    let number = |after: &str| {
        line.split_once(after)
            .and_then(|(_, rest)| rest.split([' ', ':']).next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    let col = if line.contains(" col ") {
        number(" col ") + 1
    } else {
        0
    };
    (number("row "), col)
}

/// Fifteen models: eight rows and the sums of the seven out of view, the most tokens first,
/// over bars that keep each CLI's models together and its fourth and after in one run.
#[test]
fn fifteen_models_match_their_drawing() {
    let example = crew(&FIFTEEN, &WEEKS_FIFTEEN, (21, 88, 1175));
    let screen = by_model(example, &FIFTEEN, &ON_BY_MODEL, SCREEN);
    let html = include_str!("../../../tests/golden/stats/many_models_15.html");
    dump("many_models_15", &screen, box_at(&screen), html);
    assert_box("fifteen models", html, &screen, &[]);
    let shown = text(&screen);
    assert!(
        shown.contains("tab By CLI, By model, By role · ↓  into the table · enter opens a model"),
        "{shown}"
    );
}

/// At 80 columns the chart gives way to the table, and the title keeps the totals the
/// shares are of.
#[test]
fn fifteen_models_at_80_columns_match_their_drawing() {
    let example = crew(&FIFTEEN, &WEEKS_FIFTEEN, (21, 88, 1175));
    let screen = by_model(example, &FIFTEEN, &ON_BY_MODEL, NARROW);
    let html = include_str!("../../../tests/golden/stats/many_models_15_narrow.html");
    dump("many_models_15_narrow", &screen, box_at(&screen), html);
    assert_box("fifteen models at 80 columns", html, &screen, &[]);
}

/// Forty models across six CLIs keep the box as tall as fifteen do: each CLI's group in a
/// bar is at most three textures and one run of the fourth.
#[test]
fn forty_models_match_their_drawing() {
    let example = crew(&FORTY, &WEEKS_FORTY, (57, 236, 1887));
    let screen = by_model(example, &FORTY, &ON_BY_MODEL, SCREEN);
    let html = include_str!("../../../tests/golden/stats/many_models_40.html");
    dump("many_models_40", &screen, box_at(&screen), html);
    assert_box("forty models", html, &screen, &[]);
}

/// `/` and `gemini` keep the seven models whose name or id says so, the filter's row holding
/// their sums in the table's columns, and Enter keeps the filter with no row pointed at.
#[test]
fn forty_models_filtered_match_their_drawing() {
    use KeyCode::{Char, Down, Enter};
    let mut keys = ON_BY_MODEL.to_vec();
    keys.extend([Down, Char('/')]);
    keys.extend("gemini".chars().map(Char));
    keys.push(Enter);
    let example = crew(&FORTY, &WEEKS_FORTY, (57, 236, 1887));
    let screen = by_model(example, &FORTY, &keys, SCREEN);
    let html = include_str!("../../../tests/golden/stats/many_models_40_gemini.html");
    dump("many_models_40_gemini", &screen, box_at(&screen), html);
    let known = [
        // The rows' times come to 2h 52m: 1h 11m, 27m, 44m, 9m, 11m, 6m and 4m.
        reads(html, 8, &[("2h 56m", "2h 52m")]),
        // The drawing starts this one title a cell before the names, as it does no other.
        reads(html, 9, &[("           model  ", "            model ")]),
    ];
    assert_box("forty models filtered", html, &screen, &known);
    let shown = text(&screen);
    assert!(
        shown.contains(
            "esc clears gemini · ↑↓  model · enter opens it · ←  →  sort: tokens · / search"
        ),
        "{shown}"
    );
}

/// ↓ into the table points at its first row, and ↓ again at the next: the pointed model's
/// share stays lit in all three bars and at the bottom of the chart's columns, and every
/// other cell keeps its glyph in grey, so the bars keep their shape.
#[test]
fn the_pointed_model_stays_lit_in_every_bar() {
    let mut keys = ON_BY_MODEL.to_vec();
    keys.extend([KeyCode::Down, KeyCode::Down]);
    let example = crew(&FIFTEEN, &WEEKS_FIFTEEN, (21, 88, 1175));
    let screen = by_model(example, &FIFTEEN, &keys, SCREEN);
    let html = include_str!("../../../tests/golden/stats/many_models_pointed.html");
    dump("many_models_pointed", &screen, box_at(&screen), html);
    assert_box("the pointed model", html, &screen, &[]);
    let shown = text(&screen);
    assert!(
        shown.contains(
            "r to cycle dates · ↑↓  model · ←  →  sort: tokens · / search · enter opens it · esc leaves the table"
        ),
        "{shown}"
    );
}

/// End goes to the last row, and the window's first row holds the sums of the rows above it.
#[test]
fn end_shows_the_last_rows_under_the_sums_of_the_rest() {
    let mut keys = ON_BY_MODEL.to_vec();
    keys.extend([KeyCode::Down, KeyCode::End]);
    let example = crew(&FIFTEEN, &WEEKS_FIFTEEN, (21, 88, 1175));
    let screen = by_model(example, &FIFTEEN, &keys, SCREEN);
    let html = include_str!("../../../tests/golden/stats/many_models_end.html");
    dump("many_models_end", &screen, box_at(&screen), html);
    assert_box("the last rows", html, &screen, &[]);
}

/// Where the box of every other model starts on the screen: its top row is the one over its
/// title.
fn lines_at(screen: &Buffer) -> Offset {
    Offset {
        rows: row_of(screen, "◆ every other model, one line each") as isize - 1,
        columns: 1,
    }
}

/// Fifteen models get six cards, the six with the most tokens in the table's order, and the
/// other nine a line each in a box under the cards, in the same order.
#[test]
fn cards_stop_at_six_and_every_other_model_gets_a_line() {
    let example = crew(&FIFTEEN, &WEEKS_FIFTEEN, (21, 88, 1175));
    let screen = by_model(example, &FIFTEEN, &ON_BY_MODEL, SCREEN);
    let shown = text(&screen);
    let cards: Vec<&str> = shown.lines().filter(|row| row.contains(" tok │")).collect();
    assert_eq!(cards.len(), 2, "two rows of three cards: {shown}");
    for (row, names) in cards.iter().zip([
        ["Opus 5.5", "6 Sol", "Sonnet 5.5"],
        ["3.8 Flash High", "6 Luna", "GLM 5.3 Flash Free"],
    ]) {
        let at: Vec<usize> = names
            .iter()
            .map(|name| row.find(name).unwrap_or_else(|| panic!("{name}: {shown}")))
            .collect();
        assert!(at.is_sorted(), "{row}");
    }
    let html = include_str!("../../../tests/golden/stats/every_other_model.html");
    let at = lines_at(&screen);
    dump("every_other_model", &screen, at, html);
    let theme = theme();
    let compared = Compared {
        theme: &theme,
        screen: &screen,
        at,
        named: Some(many_models),
    };
    compared.assert_matches("every other model", html, rows(0..drawing(html).len()));
}

/// At 80 columns the lines leave out the time a piece took, the weeks and the cache hit.
#[test]
fn a_narrow_line_keeps_what_it_did_and_used() {
    let example = crew(&FIFTEEN, &WEEKS_FIFTEEN, (21, 88, 1175));
    let screen = by_model(example, &FIFTEEN, &ON_BY_MODEL, NARROW);
    let shown = text(&screen);
    let top = row_of(&screen, "◆ every other model, one line each");
    let titles = shown.lines().nth(top + 2).unwrap_or_default();
    for title in ["model", "did", "passed", "tokens", "spend"] {
        assert!(titles.contains(title), "{title}: {shown}");
    }
    for left_out in ["each", "weeks", "cache"] {
        assert!(!titles.contains(left_out), "{left_out}: {shown}");
    }
    let haiku = shown
        .lines()
        .skip(top)
        .find(|row| row.contains("Haiku 5.5"))
        .unwrap_or_else(|| panic!("{shown}"));
    assert!(
        haiku
            .trim_end_matches(['│', ' '])
            .ends_with("0.42M   ~$0.31"),
        "{haiku}"
    );
}

/// Where the terminal draws logos, each card's title starts with the logo of whoever serves
/// its model, and By CLI's cards and legend with their CLI's in place of the dot.
#[test]
fn the_cards_start_with_their_logos() {
    let example = crew(&FIFTEEN, &WEEKS_FIFTEEN, (21, 88, 1175));
    let shown = text(&by_model(example, &FIFTEEN, &ON_BY_MODEL, SCREEN));
    for title in ["◧◨ █ Opus 5.5", "◧◨ █ 6 Sol", "◧◨ ▚ Sonnet 5.5"] {
        assert!(shown.contains(title), "{title}: {shown}");
    }
    // GLM's maker has no logo, so GLM's card on OpenCode starts with its swatch.
    assert!(shown.contains("│ █ GLM 5.3 Flash Free"), "{shown}");
    let example = crew(&FIFTEEN, &WEEKS_FIFTEEN, (21, 88, 1175));
    let shown = text(&by_model(example, &FIFTEEN, &[KeyCode::Down], SCREEN));
    for title in [
        "│ ◧◨ Claude Code",
        "│ ◧◨ Codex",
        "│ ◧◨ agy",
        "│ ◧◨ OpenRouter",
    ] {
        assert!(shown.contains(title), "{title}: {shown}");
    }
    // The table under the bar by CLI starts each CLI's row with its logo too.
    let row = (shown.lines())
        .find(|row| row.contains("◧◨ ██  Claude Code"))
        .unwrap_or_else(|| panic!("{shown}"));
    let words = row.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words.contains("Claude Code 4.30M 44%"), "{row}");
}
