//! The widget kit: the context card against the mockups it was designed from, cell by cell,
//! every form at every width, and a sweep of values that must never move a row or a column.

mod common;

use argonauts_tui::stability::{self, WIDTHS, samples};
use argonauts_tui::text::{spans_width, str_width};
use argonauts_tui::theme::Role;
use argonauts_tui::theme::{ColorLevel, Glyphs, Theme};
use argonauts_tui::widgets::{
    Chart, ContextCard, Drawn, Form, Frame, Kit, Levels, Limit, LimitKind, ListRow, ListSlots,
    MeterRow, MeterSlots, State, States, Window, bodies, stack_cards,
};
use common::{styled, text};
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;

/// The mockups' scenario: Opus 5.5 on Claude Code, 212k of a 1M window, compacting at 950k.
fn card() -> ContextCard {
    let mut card =
        ContextCard::claude_code(Some([3_400, 12_000, 2_600, 1_100, 4_900, 2_200, 186_000]));
    card.used = Some(212_200);
    card.window = Some(1_000_000);
    card.limit = Some(Limit {
        at: 950_000,
        kind: LimitKind::Compacts,
    });
    card.key = Some(String::from("ctrl+x c"));
    card
}

fn theme(base: Theme, level: ColorLevel) -> Theme {
    let mut theme = base.with_level(level);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn dark() -> Theme {
    theme(Theme::dark(), ColorLevel::TrueColor)
}

fn plain() -> Theme {
    theme(Theme::dark(), ColorLevel::None)
}

#[derive(Debug, Clone, PartialEq)]
struct Cell {
    symbol: String,
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
}

fn cells(line: &Line<'_>) -> Vec<Cell> {
    let mut out = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        for grapheme in span.content.graphemes(true) {
            out.push(Cell {
                symbol: grapheme.to_owned(),
                fg: style.fg,
                bg: style.bg,
                bold: style.add_modifier.contains(Modifier::BOLD),
            });
        }
    }
    out
}

/// The mockup kit's ANSI rows: every span starts with a full `ESC[0;…m`.
fn parse_ans(text: &str) -> Vec<Vec<Cell>> {
    text.lines()
        .map(|row| {
            let mut out = Vec::new();
            for chunk in row.split('\x1b').filter(|c| !c.is_empty()) {
                let Some((codes, body)) = chunk.strip_prefix('[').and_then(|c| c.split_once('m'))
                else {
                    continue;
                };
                let (fg, bg, bold) = sgr(codes);
                for grapheme in body.graphemes(true) {
                    out.push(Cell {
                        symbol: grapheme.to_owned(),
                        fg,
                        bg,
                        bold,
                    });
                }
            }
            out
        })
        .collect()
}

fn sgr(codes: &str) -> (Option<Color>, Option<Color>, bool) {
    const NORMAL: [Color; 8] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
    ];
    const BRIGHT: [Color; 8] = [
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    let codes: Vec<u8> = codes.split(';').filter_map(|c| c.parse().ok()).collect();
    let (mut fg, mut bg, mut bold) = (None, None, false);
    let mut k = 0;
    while k < codes.len() {
        match codes[k] {
            1 => bold = true,
            c @ 30..=37 => fg = Some(NORMAL[usize::from(c - 30)]),
            c @ 90..=97 => fg = Some(BRIGHT[usize::from(c - 90)]),
            c @ 40..=47 => bg = Some(NORMAL[usize::from(c - 40)]),
            c @ 100..=107 => bg = Some(BRIGHT[usize::from(c - 100)]),
            c @ (38 | 48) => {
                let color = Color::Rgb(codes[k + 2], codes[k + 3], codes[k + 4]);
                if c == 38 {
                    fg = Some(color);
                } else {
                    bg = Some(color);
                }
                k += 4;
            }
            _ => {}
        }
        k += 1;
    }
    (fg, bg, bold)
}

/// Every cell the mockup draws, the same here: glyph, colors, and weight. Past the drawn
/// width the mockup only pads with blanks.
fn assert_matches_mockup(drawn: &Drawn, mockup: &str) {
    let want = parse_ans(mockup);
    assert_eq!(drawn.lines.len(), want.len(), "rows");
    let mut wrong = Vec::new();
    for (row, (line, want)) in drawn.lines.iter().zip(&want).enumerate() {
        let got = cells(line);
        for (col, want) in want.iter().enumerate() {
            let blank = Cell {
                symbol: String::from(" "),
                fg: None,
                bg: None,
                bold: false,
            };
            let got = got.get(col).unwrap_or(&blank);
            if got != want {
                wrong.push(format!("row {row} col {col}: want {want:?}, got {got:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} cells differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn the_context_card_matches_its_mockups_cell_by_cell() {
    let cases = [
        (dark(), include_str!("fixtures/mods/context-card.ans")),
        (
            theme(Theme::light(), ColorLevel::TrueColor),
            include_str!("fixtures/mods/context-card-light.ans"),
        ),
        (
            theme(Theme::dark(), ColorLevel::Ansi16),
            include_str!("fixtures/mods/context-card-ansi16.ans"),
        ),
    ];
    for (theme, mockup) in cases {
        let drawn = card().draw(&Kit::new(&theme), Form::Full, 100);
        assert_matches_mockup(&drawn, mockup);
    }
    let drawn = card().draw(&Kit::new(&plain()), Form::Full, 100);
    let want: Vec<&str> = include_str!("fixtures/mods/context-card-nocolor.txt")
        .lines()
        .map(str::trim_end)
        .collect();
    assert_eq!(text(&drawn.lines).lines().collect::<Vec<_>>(), want);
}

const FORMS: [(Form, &str); 4] = [
    (Form::Full, "full"),
    (Form::Compact, "compact"),
    (Form::Line, "line"),
    (Form::Chip, "chip"),
];

/// Every form on one page, as the no-color form draws it.
fn forms_page(card: &ContextCard, theme: &Theme, width: u16) -> String {
    let kit = Kit::new(theme);
    let mut page = Vec::new();
    for (form, name) in FORMS {
        let drawn = card.draw(&kit, form, usize::from(width));
        stability::assert_drawn(&drawn, width);
        page.push(format!("{name}:\n{}", text(&drawn.lines)));
    }
    page.push(format!("words:\n{}", card.words().join("\n")));
    page.join("\n\n")
}

#[test]
fn every_form_at_every_width() {
    for width in WIDTHS {
        insta::assert_snapshot!(
            format!("context_card_{width}"),
            forms_page(&card(), &plain(), width)
        );
        // The color forms draw the same cells, so the frame checker passes them too.
        for theme in [dark(), theme(Theme::light(), ColorLevel::TrueColor)] {
            for (form, _) in FORMS {
                let drawn = card().draw(&Kit::new(&theme), form, usize::from(width));
                stability::assert_drawn(&drawn, width);
            }
        }
    }
}

#[test]
fn the_color_forms_at_the_mockup_widths() {
    let themes = [
        ("dark", dark()),
        ("light", theme(Theme::light(), ColorLevel::TrueColor)),
        ("ansi16", theme(Theme::dark(), ColorLevel::Ansi16)),
    ];
    for (name, theme) in themes {
        for width in [80, 100, 120] {
            let drawn = card().draw(&Kit::new(&theme), Form::Full, width);
            insta::assert_snapshot!(format!("context_card_{name}_{width}"), styled(&drawn.lines));
        }
    }
}

#[test]
fn a_screen_reader_hears_the_card_as_lines() {
    assert_eq!(
        card().words(),
        [
            "context: 212k of 1M, 21%, compacts at 950k",
            "context: system prompt 3.4k 0.3%, tools 12k 1%, mcp tools 2.6k 0.3%, agents 1.1k 0.1%",
            "context: memory files 4.9k 0.5%, skills 2.2k 0.2%, messages 186k 19%, free 788k",
        ]
    );
    let mut near = card();
    near.used = Some(930_000);
    assert!(
        near.words()[0].contains("93%, danger"),
        "{:?}",
        near.words()
    );
}

/// The mockup's card with `used` of the window in use, messages taking what the other
/// categories don't. Past the window, free is gone and the number keeps counting.
fn card_at(used: u64) -> ContextCard {
    let mut card = card();
    let mut left = used;
    for part in card.parts.iter_mut().take(6) {
        let tokens = part.tokens.unwrap_or(0).min(left);
        part.tokens = Some(tokens);
        left -= tokens;
    }
    card.parts[6].tokens = Some(left);
    card.used = Some(used);
    card
}

/// Where the frame's borders, the legend's swatches, and the badge's bracket sit on each row.
fn layout(drawn: &Drawn) -> Vec<Vec<usize>> {
    drawn
        .lines
        .iter()
        .map(|line| {
            let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let mut col = 0;
            let mut marks = Vec::new();
            for grapheme in row.graphemes(true) {
                if ["│", "╭", "╮", "╰", "╯", "▌", "]"].contains(&grapheme) {
                    marks.push(col);
                }
                col += str_width(grapheme);
            }
            marks
        })
        .collect()
}

#[test]
fn values_never_move_a_row_or_a_column() {
    let window = 1_000_000;
    let mut sweep: Vec<(String, ContextCard)> = [0.0, 0.09, 0.10, 0.99, 1.0, 1.05]
        .into_iter()
        .map(|p: f64| {
            (
                format!("{:.0}%", p * 100.0),
                card_at((p * window as f64) as u64),
            )
        })
        .collect();
    let mut empty = card();
    empty.used = None;
    for part in &mut empty.parts {
        part.tokens = None;
    }
    sweep.push((String::from("no reading yet"), empty));
    let mut full = card_at(window);
    full.parts
        .iter_mut()
        .take(6)
        .for_each(|p| p.tokens = Some(0));
    full.parts[6].tokens = Some(window);
    sweep.push((String::from("all messages"), full));
    for width in WIDTHS {
        let kit_theme = plain();
        let kit = Kit::new(&kit_theme);
        let mut page = Vec::new();
        let mut first: Option<(usize, Vec<Vec<usize>>)> = None;
        for (name, card) in &sweep {
            for form in [Form::Full, Form::Compact] {
                let drawn = card.draw(&kit, form, usize::from(width));
                stability::assert_drawn(&drawn, width);
                if form == Form::Full {
                    let shape = (drawn.height(), layout(&drawn));
                    match &first {
                        None => first = Some(shape),
                        Some(want) => assert_eq!(&shape, want, "{name} at {width} columns"),
                    }
                    page.push(format!("{name}:\n{}", text(&drawn.lines)));
                }
            }
            let line = card.draw(&kit, Form::Line, usize::from(width));
            assert_eq!(
                spans_width(&line.lines[0].spans),
                usize::from(width),
                "{name}"
            );
        }
        if width == 100 {
            insta::assert_snapshot!("context_card_sweep_100", page.join("\n\n"));
        }
    }
}

#[test]
fn cjk_and_long_names_stay_in_their_slots() {
    let mut card = card();
    card.subject = Some(format!("{} · {}", samples::CJK, samples::LONG_PATH));
    card.parts[0].name = String::from(samples::CJK);
    card.parts[6].name = samples::long_word();
    for width in WIDTHS {
        for theme in [plain(), dark()] {
            for (form, _) in FORMS {
                let drawn = card.draw(&Kit::new(&theme), form, usize::from(width));
                stability::assert_drawn(&drawn, width);
            }
        }
    }
    insta::assert_snapshot!(
        "context_card_long_names_80",
        forms_page(&card, &plain(), 80)
    );
}

#[test]
fn a_cli_without_a_breakdown_shows_estimates() {
    let mut card = ContextCard::estimate(Some(18_400), Some(96_300));
    card.used = Some(114_700);
    card.window = Some(272_000);
    card.limit = Some(Limit {
        at: 240_000,
        kind: LimitKind::Rotates,
    });
    card.subject = Some(String::from("task-002 · GPT 5.6 on Codex"));
    insta::assert_snapshot!(
        "context_card_estimate_100",
        forms_page(&card, &plain(), 100)
    );
}

#[test]
fn a_crowded_screen_keeps_cards_apart() {
    let theme = dark();
    let kit = Kit::new(&theme);
    let cards = stack_cards([
        card().card(&kit, Form::Compact, Frame::view(100)),
        card().card(&kit, Form::Compact, Frame::view(100)),
    ]);
    stability::assert_drawn(&cards, 100);
    assert_eq!(cards.meters.len(), 2);
}

/// Rows as text, trailing spaces trimmed.
fn rows(drawn: &Drawn) -> Vec<String> {
    text(&drawn.lines).lines().map(str::to_owned).collect()
}

#[test]
fn quota_windows_pair_as_claude_codes_usage_draws_them() {
    let kit_theme = plain();
    let kit = Kit::new(&kit_theme);
    let window = |title: &str, used: f64, resets: &str| Window {
        title: title.to_owned(),
        used,
        level: Levels::QUOTA.level(used * 100.0),
        resets: Some(resets.to_owned()),
    };
    let claude = [
        window("Current session", 0.71, "Resets 4:40pm"),
        window("Current week", 0.38, "Resets Tue 10pm"),
    ];
    let drawn = bodies::windows(&kit, &claude, 94);
    assert_eq!(
        rows(&drawn),
        [
            "Current session                                  Current week",
            "██████████████████████████░░░░░░░░░░ 71% used    ██████████████░░░░░░░░░░░░░░░░░░░░░░ 38% used",
            "Resets 4:40pm                                    Resets Tue 10pm",
        ]
    );
    let codex = [
        window("5-hour limit", 0.92, "Resets 2:15pm"),
        window("Weekly limit", 0.41, "Resets Fri 9am"),
    ];
    let drawn = bodies::windows(&kit, &codex, 94);
    assert_eq!(
        rows(&drawn)[1],
        "█████████████████████████████████░░░ 92% used    ███████████████░░░░░░░░░░░░░░░░░░░░░ 41% used"
    );
    // Narrower, they stack, and every meter still has text rows around it.
    for width in [40, 60, 73, 94] {
        let all = bodies::windows(&kit, &[claude.clone(), codex.clone()].concat(), width);
        stability::assert_drawn(&all, width as u16);
    }
}

#[test]
fn stacked_quota_rows_are_line_meters() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let meter = |label: &str, ratio: f64, note: &str| MeterRow::Meter {
        label: label.to_owned(),
        ratio,
        level: Levels::QUOTA.level(ratio * 100.0),
        unit: String::from(" used"),
        note: note.to_owned(),
        tick: None,
    };
    let rows_in = [
        meter("Claude 5h", 0.71, "Resets 4:40pm"),
        meter("Codex 5h", 0.92, "Resets 2:15pm"),
        MeterRow::Text {
            label: String::from("agy day"),
            words: vec![
                kit.span("✗ out until Tue 3:45pm", Role::Error),
                kit.dim(" · its tasks moved to Codex"),
            ],
        },
    ];
    let slots = MeterSlots {
        label: 10,
        note: 15,
    };
    let drawn = bodies::meters(&kit, &rows_in, 90, slots);
    assert_eq!(
        rows(&drawn),
        [
            "Claude 5h  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸─────────────── 71% used  Resets 4:40pm",
            "Codex 5h   ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━──── 92% used  Resets 2:15pm",
            "agy day    ✗ out until Tue 3:45pm · its tasks moved to Codex",
        ]
    );
    assert!(drawn.meters.is_empty(), "line meters may stack");
    for width in WIDTHS {
        let inner = usize::from(width).saturating_sub(10);
        stability::assert_drawn(&bodies::meters(&kit, &rows_in, inner, slots), width);
    }
}

#[test]
fn a_list_lines_up_its_states_names_and_numbers() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let row = |state, name: &str, word: &str, note: &str, spend: f64, time: &str| ListRow {
        state,
        name: name.to_owned(),
        word: word.to_owned(),
        note: note.to_owned(),
        ratio: Some(spend / 1.20),
        numbers: vec![format!("${spend:.2}"), time.to_owned()],
    };
    let runs = [
        row(
            State::Working,
            "run-03  Export reports as CSV",
            "running",
            "1/4 tasks",
            1.20,
            "51m",
        ),
        row(
            State::Done,
            "run-02  Fix the flaky export test",
            "merged",
            "2/2 tasks",
            0.39,
            "12m40s",
        ),
    ];
    let slots = ListSlots {
        name: 38,
        word: 9,
        note: 11,
        numbers: vec![5, 6],
    };
    let drawn = bodies::list(&kit, &States::default(), &runs, 94, &slots);
    assert_eq!(
        rows(&drawn),
        [
            "◐ run-03  Export reports as CSV         running  1/4 tasks  ▰▰▰▰▰▰▰▰▰▰           $1.20     51m",
            "✔ run-02  Fix the flaky export test     merged   2/2 tasks  ▰▰▰▱▱▱▱▱▱▱           $0.39  12m40s",
        ]
    );
    for width in WIDTHS {
        let inner = usize::from(width).saturating_sub(6);
        let drawn = bodies::list(&kit, &States::default(), &runs, inner, &slots);
        stability::assert_drawn(&drawn, width);
    }
}

#[test]
fn history_stands_in_two_rows_over_its_days() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let chart = Chart {
        values: vec![
            2.10, 3.10, 0.0, 1.20, 1.90, 0.90, 0.0, 0.0, 2.80, 2.20, 0.95, 1.05, 1.62, 1.86,
        ],
        label: String::from("$3.10"),
        letters: "MTWTFSSMTWTFSS".chars().map(String::from).collect(),
        role: Role::CatMessages,
        last_role: Role::Accent,
    };
    assert_eq!(
        rows(&bodies::columns(&kit, &chart)),
        [
            "$3.10 ▃ █     ▂       ▆ ▃       ▂",
            "      █ █   ▆ █ ▅     █ █ ▅ ▅ █ █",
            "      M T W T F S S M T W T F S S",
        ]
    );
}
