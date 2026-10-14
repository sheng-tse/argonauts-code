use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::ModelTokens;
use argonauts_core::status_tab::{CliHealth, Covers, HealthRows, ModelStanding, Standing};
use ratatui::style::Color;
use time::OffsetDateTime;
use time::macros::datetime;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::card;
use crate::format::WallClock;
use crate::kit::{Drawn, Frame, Kit};
use crate::overview::golden::{Picture, Region, differences, theme};
use crate::overview::shades::{ModelColors, shade};
use crate::theme::Role;

/// The screen the picture was drawn for: its card is 118 cells from border to border, a column
/// in from each side.
const SCREEN: usize = 120;

fn model(name: &str, state: Standing) -> ModelStanding {
    ModelStanding {
        name: name.to_owned(),
        state,
        why: String::new(),
        covers: None,
        next_check: None,
    }
}

fn troubled(name: &str, state: Standing, why: &str, next: OffsetDateTime) -> ModelStanding {
    ModelStanding {
        why: why.to_owned(),
        next_check: Some(next),
        ..model(name, state)
    }
}

fn cli(id: &str, name: &str, main: bool, models: Vec<ModelStanding>) -> CliHealth {
    CliHealth {
        id: id.to_owned(),
        name: name.to_owned(),
        installed: true,
        main,
        models,
    }
}

/// The picture's moment: Opus 5.5 went out at 1:58 PM and is tested at 2:03 PM, Sonnet 5.5 does
/// its review meanwhile, and GPT 5.6 Luna reconnected twice.
pub(crate) fn picture_rows() -> HealthRows {
    let fallback = ModelStanding {
        covers: Some(Covers {
            job: "review:due-tests".to_owned(),
            cli: "claude-code".to_owned(),
            model: "Opus 5.5".to_owned(),
        }),
        ..model("Sonnet 5.5", Standing::Healthy)
    };
    HealthRows {
        clis: vec![
            cli(
                "claude-code",
                "Claude Code",
                true,
                vec![
                    troubled(
                        "Opus 5.5",
                        Standing::Overloaded,
                        "2 overload errors in a row",
                        datetime!(2026-10-08 14:03 UTC),
                    ),
                    fallback,
                    model("Haiku 5.5", Standing::Healthy),
                ],
            ),
            cli(
                "codex",
                "Codex",
                false,
                vec![
                    model("GPT 5.6", Standing::Healthy),
                    troubled(
                        "GPT 5.6 Luna",
                        Standing::Degraded,
                        "reconnected twice in 2 minutes",
                        datetime!(2026-10-08 14:01 UTC),
                    ),
                ],
            ),
            cli(
                "antigravity-cli",
                "Antigravity CLI",
                false,
                vec![
                    model("Gemini 3.6 Pro", Standing::Healthy),
                    model("Gemini 3.6 Flash", Standing::Healthy),
                ],
            ),
            CliHealth {
                installed: false,
                ..cli("opencode", "OpenCode", false, Vec::new())
            },
        ],
        quiet_secs: Some(120),
        cooldown_secs: 300,
    }
}

fn draw(health: &HealthRows, screen: usize) -> Drawn {
    draw_ranked(health, screen, &[])
}

/// The card with every model's tokens in all, which rank each CLI's models for their shades.
fn draw_ranked(health: &HealthRows, screen: usize, ranked: &[ModelTokens]) -> Drawn {
    let theme = theme();
    let kit = Kit::new(&theme);
    let clock = WallClock::pinned(datetime!(2026-10-08 13:59 UTC));
    let names = DisplayNames::default();
    let shades = ModelColors::new(ranked, &names);
    card(health, &kit, Frame::view(screen), &clock, shades)
}

fn rows(drawn: &Drawn) -> Vec<String> {
    let texts = drawn.lines.iter().map(|line| {
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        text
    });
    texts.collect()
}

/// The card, cell for cell, as its picture draws it: the picture's own rows in the names the
/// code uses now, without its closing line about how health was counted before.
#[test]
fn the_card_matches_its_picture() {
    let golden = include_str!("pictures/health_card.txt");
    let drawn = rows(&draw(&picture_rows(), SCREEN));
    let wanted: Vec<String> = golden.lines().map(|line| format!(" {line}")).collect();
    for (at, (got, want)) in drawn.iter().zip(&wanted).enumerate() {
        assert_eq!(got.trim_end(), want.trim_end(), "row {at}");
    }
    assert_eq!(drawn.len(), wanted.len(), "{drawn:#?}");
}

/// The card on a screen of its own, one column in, as the picture has it.
fn buffer(drawn: &Drawn, screen: usize) -> Buffer {
    let area = Rect::new(0, 0, screen as u16, drawn.lines.len() as u16);
    let mut buf = Buffer::empty(area);
    for (y, line) in drawn.lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, area.width);
    }
    buf
}

/// Every cell's colour and weight as the picture's `.html` has them. The picture writes a
/// model's name in the shade of its place among its CLI's models, a colour of its own rather
/// than one of the theme's names, so those cells are compared with the shade each model draws in
/// (`each_state_has_its_colour_and_each_model_its_clis`), and every other cell here.
#[test]
fn the_cards_colours_match_its_picture() {
    let theme = theme();
    let picture = Picture::load("health_card");
    let drawn = draw(&picture_rows(), SCREEN);
    let buf = buffer(&drawn, SCREEN);
    let region = Region::whole(&picture, (1, 0));
    let unlike = differences_past_model_names(&picture, &buf, &region, &theme);
    assert!(unlike.is_empty(), "{}", unlike.join("\n"));
}

/// The cells where `buf` is different from the card's picture, leaving out the ones that hold
/// a model's name, whose shade the picture doesn't name.
pub(crate) fn differences_past_model_names(
    picture: &Picture,
    buf: &Buffer,
    region: &Region,
    theme: &crate::theme::Theme,
) -> Vec<String> {
    let model_names = [
        "Opus 5.5",
        "Sonnet 5.5",
        "Haiku 5.5",
        "GPT 5.6 Luna",
        "GPT 5.6",
        "Gemini 3.6 Pro",
        "Gemini 3.6 Flash",
    ];
    let texts = picture.text();
    let named = |row: usize, column: usize| {
        let text: Vec<char> = texts[row].chars().collect();
        model_names.iter().any(|name| {
            let name: Vec<char> = name.chars().collect();
            (0..text.len())
                .any(|at| text[at..].starts_with(&name) && (at..at + name.len()).contains(&column))
        })
    };
    differences(picture, buf, region, theme)
        .into_iter()
        .filter(|found| {
            let mut words = found.split_whitespace();
            let row = words.nth(1).and_then(|row| row.parse::<usize>().ok());
            let column = words
                .nth(1)
                .and_then(|column| column.trim_end_matches(':').parse::<usize>().ok());
            !matches!((row, column), (Some(row), Some(column)) if named(row, column))
        })
        .collect()
}

/// The colour of the first cell of `word` on the card's row that holds `row_holds`.
fn color_of(drawn: &Drawn, row_holds: &str, word: &str) -> Option<Color> {
    let line = drawn.lines.iter().find(|line| {
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        text.contains(row_holds)
    })?;
    let span = line.spans.iter().find(|span| span.content.contains(word))?;
    span.style.fg
}

#[test]
fn each_state_has_its_colour_and_each_model_its_clis() {
    let theme = theme();
    let kit = Kit::new(&theme);
    let drawn = draw(&picture_rows(), SCREEN);
    let role = |role: Role| kit.style(role).fg;
    assert_eq!(
        color_of(&drawn, "Opus 5.5 ", "overloaded"),
        role(Role::Danger)
    );
    assert_eq!(
        color_of(&drawn, "Haiku 5.5", "healthy"),
        role(Role::Success)
    );
    assert_eq!(
        color_of(&drawn, "GPT 5.6 Luna", "degraded"),
        role(Role::Warn)
    );
    assert_eq!(
        color_of(&drawn, "one test task decides", "testing"),
        role(Role::Accent)
    );
    assert_eq!(
        color_of(&drawn, "every model on the CLI", "down"),
        role(Role::Danger)
    );
    // The reason and the next check stay quiet beside the state.
    assert_eq!(
        color_of(&drawn, "Opus 5.5 ", "2 overload errors"),
        role(Role::Dim)
    );
    assert_eq!(color_of(&drawn, "Opus 5.5 ", "2:03 PM"), role(Role::Dim));
    // A CLI's first model takes its colour, as its dot does, and each further one a shade.
    assert_eq!(color_of(&drawn, "Opus 5.5 ", "● "), role(Role::CliClaude));
    assert_eq!(
        color_of(&drawn, "Opus 5.5 ", "Opus 5.5"),
        role(Role::CliClaude)
    );
    assert_eq!(
        color_of(&drawn, "GPT 5.6 Luna", "GPT 5.6 Luna"),
        shade(&kit, Role::CliCodex, 1).fg
    );
    // The model a job was asked of keeps its own colour on the row of the one doing it.
    assert_eq!(
        color_of(&drawn, "review:due-tests", "Sonnet 5.5"),
        shade(&kit, Role::CliClaude, 1).fg
    );
    assert_eq!(
        color_of(&drawn, "review:due-tests", "Opus 5.5"),
        role(Role::CliClaude)
    );
    assert_eq!(
        color_of(&drawn, "review:due-tests", "↷ review:due-tests for"),
        role(Role::Dim)
    );
    assert_eq!(
        color_of(&drawn, "OpenCode", "not installed"),
        role(Role::Dim)
    );
}

/// A model draws in the shade it has on every tab, by its place among its CLI's models in
/// tokens over all time, whatever row of the card it is on: Sonnet 5.5 has run the most
/// here and Haiku 5.5 the next most, so they take Claude Code's own colour and its deep
/// shade from the second and third rows. Opus 5.5, which the numbers haven't met, comes after
/// them, on its own row and where another model's row names it.
#[test]
fn a_model_keeps_the_shade_it_has_on_every_tab() {
    let theme = theme();
    let kit = Kit::new(&theme);
    let tokens = |model: &str, tokens| ModelTokens {
        agent: "claude-code".to_owned(),
        model: Some(model.to_owned()),
        tokens,
    };
    let ranked = [
        tokens("claude-haiku-5-5", 400),
        tokens("claude-sonnet-5-5", 900),
    ];
    let drawn = draw_ranked(&picture_rows(), SCREEN, &ranked);
    let shade_of = |nth| shade(&kit, Role::CliClaude, nth).fg;
    assert_eq!(color_of(&drawn, "Sonnet 5.5 ", "Sonnet 5.5"), shade_of(0));
    assert_eq!(color_of(&drawn, "Haiku 5.5 ", "Haiku 5.5"), shade_of(1));
    assert_eq!(color_of(&drawn, "Opus 5.5 ", "Opus 5.5"), shade_of(2));
    assert_eq!(
        color_of(&drawn, "review:due-tests", "Opus 5.5"),
        shade_of(2)
    );
    // Another CLI's models keep the card's order while the numbers know none of them.
    assert_eq!(
        color_of(&drawn, "GPT 5.6 Luna", "GPT 5.6 Luna"),
        shade(&kit, Role::CliCodex, 1).fg
    );
}

#[test]
fn a_cli_that_is_down_says_so_on_every_model_with_its_test() {
    let down = |name: &str, why: &str| {
        troubled(name, Standing::Down, why, datetime!(2026-10-08 14:05 UTC))
    };
    let health = HealthRows {
        clis: vec![cli(
            "claude-code",
            "Claude Code",
            true,
            vec![
                down("Opus 5.5", "2 overload errors in a row"),
                down("Sonnet 5.5", "2 server errors in a row"),
                down("Haiku 5.5", "two of its models are out"),
            ],
        )],
        ..picture_rows()
    };
    let drawn = rows(&draw(&health, SCREEN));
    let haiku = drawn.iter().find(|row| row.contains("Haiku 5.5")).unwrap();
    assert!(haiku.contains("▪ down"), "{haiku}");
    assert!(haiku.contains("two of its models are out"), "{haiku}");
    assert!(haiku.contains("2:05 PM, a test task"), "{haiku}");
}

#[test]
fn the_legend_follows_your_health_settings() {
    let health = HealthRows {
        quiet_secs: Some(60),
        cooldown_secs: 90,
        ..picture_rows()
    };
    let drawn = rows(&draw(&health, SCREEN));
    let row = |word: &str| drawn.iter().find(|row| row.contains(word)).unwrap().clone();
    assert!(row("new work goes").contains("after a quiet minute"));
    assert!(row("a fallback steps in").contains("a test in 90 seconds"));
}

/// The text of the card's rows that hold `word`.
fn rows_with<'a>(drawn: &'a [String], word: &str) -> Vec<&'a str> {
    let holding = drawn.iter().filter(|row| row.contains(word));
    holding.map(String::as_str).collect()
}

/// A card a few cells narrower than its picture keeps every word: the state and the reason
/// give up the blank cell each has to spare, so when a model is checked next still fits, and
/// every row's columns still start under their headings.
#[test]
fn a_card_a_little_narrower_than_its_picture_cuts_nothing() {
    for screen in [SCREEN - 1, SCREEN - 2, SCREEN - 3] {
        let drawn = rows(&draw(&picture_rows(), screen));
        assert!(
            !drawn.iter().any(|row| row.contains('…')),
            "{screen}: {drawn:#?}"
        );
        for said in [
            "2:03 PM, a test task",
            "2:01 PM, if quiet",
            "after 2 quiet minutes",
            "out if silent an hour",
        ] {
            assert_eq!(rows_with(&drawn, said).len(), 1, "{screen}: {said}");
        }
        let column = |row: &str, word: &str| row.find(word).map(|at| row[..at].chars().count());
        let heads = rows_with(&drawn, "next check")[0];
        for (head, word, held_by) in [
            ("state", "▪ overloaded", "Opus 5.5"),
            ("why", "2 overload errors", "Opus 5.5"),
            ("next check", "2:03 PM", "Opus 5.5"),
            ("state", "▪ degraded", "new work goes"),
            ("why", "new work goes", "new work goes"),
            ("next check", "after 2 quiet", "new work goes"),
        ] {
            let row = rows_with(&drawn, held_by)[0];
            assert_eq!(column(row, word), column(heads, head), "{screen}: {row}");
        }
    }
}

/// The models' column gives up its spare cells too, once the others have none: with shorter
/// names than the picture's, a card several cells narrower still keeps every word.
#[test]
fn shorter_model_names_leave_a_narrower_card_whole() {
    let mut health = picture_rows();
    health.clis.truncate(2);
    let drawn = rows(&draw(&health, SCREEN - 7));
    assert!(!drawn.iter().any(|row| row.contains('…')), "{drawn:#?}");
    assert_eq!(rows_with(&drawn, "out if silent an hour").len(), 1);
    // "GPT 5.6 Luna" and the two blank cells after it are the column now.
    let heads = rows_with(&drawn, "next check")[0];
    let at = |word: &str| heads.find(word).unwrap();
    assert_eq!(
        at("state") - at("model"),
        "GPT 5.6 Luna".len() + 2,
        "{heads}"
    );
    // The sentence under the legend is a cell longer than the card, so it ends on a row of
    // its own.
    assert_eq!(rows_with(&drawn, "take the CLI").len(), 1, "{drawn:#?}");
    assert_eq!(rows_with(&drawn, " │ down.  ").len(), 1, "{drawn:#?}");
}

#[test]
fn a_narrow_card_cuts_its_rows_and_keeps_its_border() {
    for screen in [60, 80, 100] {
        let drawn = rows(&draw(&picture_rows(), screen));
        for row in &drawn {
            assert_eq!(crate::text::str_width(row), screen - 1, "{screen}: {row:?}");
        }
    }
}

#[test]
fn before_doctor_has_checked_the_card_says_so() {
    let drawn = rows(&draw(&HealthRows::default(), SCREEN));
    assert!(drawn.iter().any(|row| row.contains("/doctor checks them")));
}
