//! The Overview's two screens, drawn from the pictures' example numbers at the pictures' width
//! and compared with them cell for cell, colours included. The pictures draw `░ ▒ ▓ █` the way
//! Ghostty does, as `█` at four strengths, so a day's glyph is checked against its strength.
//!
//! The panel is drawn at 120 columns, as every other tab's golden draws it, and every row from
//! the tab row to the key line is compared one row down and one column in, the panel's rule
//! and inset. Where a number in a picture can't come from the numbers beside it, the row says
//! what it draws instead and why.

use std::time::Instant;

use argonauts_testkit::pictures::picture;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::{key, state, tab};
use crate::base::kit::Kit;
use crate::base::theme::{ColorLevel, Glyphs, Role, Theme};
use crate::frame::state::AppState;
use crate::frame::view::{View, ViewCtx};
use crate::panels::OverviewView;
use crate::panels::home::sample;

const OVERVIEW_TEXT: &str = include_str!("../../../../tests/golden/home/ours_overview.txt");
const OVERVIEW_COLOURS: &str = include_str!("../../../../tests/golden/home/ours_overview.html");
const MODELS_TEXT: &str = include_str!("../../../../tests/golden/home/ours_models.txt");
const MODELS_COLOURS: &str = include_str!("../../../../tests/golden/home/ours_models.html");

/// The panel's width for the pictures' 118 cells, and its height: its rule, then the
/// Models picture's 31 rows, or the Overview picture's 33.
const WIDTH: u16 = 120;
const HEIGHT: u16 = 32;
const OVERVIEW_HEIGHT: u16 = 37;

/// A day's strength in the picture, as its quarter: `25%` is the quietest.
fn strength(fg: &str) -> Option<usize> {
    let percent = fg.split('%').next()?.rsplit(' ').next()?;
    Some(match percent {
        "25" => 1,
        "50" => 2,
        "75" => 3,
        _ => 4,
    })
}

/// The colour a picture's variable stands for in the dark theme.
fn colour(theme: &Theme, value: &str) -> Color {
    let kit = Kit::new(theme);
    let variable = value
        .split("var(")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .unwrap_or(value);
    let role = match variable {
        "--term-fg" => return Color::Reset,
        "--term-dim" => Role::Dim,
        "--term-faint" => Role::Faint,
        "--term-acc" => Role::Question,
        "--term-org" => Role::AgentOrange,
        "--term-bg" => Role::BadgeText,
        "--term-band" => Role::UserBand,
        "--v-claude" | "--m-opus" => Role::CliClaude,
        "--v-codex" | "--m-gpt" => Role::CliCodex,
        "--v-agy" | "--m-pro" => Role::CliAgy,
        "--v-opencode" => Role::CliOpenCode,
        "--m-sonnet" => {
            let shade = crate::panels::stats::draw::shade(&kit, Role::CliClaude, 1);
            return shade.fg.unwrap_or(Color::Reset);
        }
        "--line-claude-deep" => {
            let step = crate::base::theme::shades::hues::step_style(&kit, Role::CliClaude, 1);
            return step.fg.unwrap_or(Color::Reset);
        }
        other => panic!("a colour the pictures don't use: {other}"),
    };
    theme.color(role).unwrap_or(Color::Reset)
}

fn theme() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::TrueColor);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn render(view: &mut OverviewView, state: &AppState, height: u16) -> Buffer {
    let theme = theme();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let area = Rect::new(0, 0, WIDTH, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

/// Row `y` from the column after the panel's inset.
fn text_of(buf: &Buffer, y: usize) -> String {
    (1..buf.area.width)
        .map(|x| buf[(x, y as u16)].symbol().to_owned())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// A day's shade as the picture draws it.
fn solid(text: &str) -> String {
    text.replace(['░', '▒', '▓'], "█")
}

/// How one screen should be different from its picture, and why.
struct Screen<'a> {
    text: &'a str,
    colours: &'a str,
    /// Picture rows of the year of days, whose glyphs are drawn solid in the picture.
    days: std::ops::RangeInclusive<usize>,
    /// Picture rows compared by their characters here, whose colours a test of their own
    /// compares.
    colours_elsewhere: std::ops::Range<usize>,
    /// Picture rows this screen draws otherwise: the row, what it draws, and why.
    instead: &'a [(usize, &'a str, &'a str)],
}

impl Screen<'_> {
    fn compare(&self, name: &str, buf: &Buffer) {
        self.compare_rows(name, buf, 0..usize::MAX);
    }

    /// [`Screen::compare`] for the picture's `rows` alone.
    fn compare_rows(&self, name: &str, buf: &Buffer, rows: std::ops::Range<usize>) {
        let theme = theme();
        let picture: Vec<&str> = self.text.lines().collect();
        let width = picture
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(0);
        let drawn =
            crate::testing::pictures::picture_dump::cut(buf, (1, 1), (width, picture.len()));
        crate::testing::pictures::picture_dump::rows(name, &drawn);
        crate::testing::pictures::picture_dump::buffer_page(
            name,
            buf,
            (1, 1),
            (width, picture.len()),
            &theme,
        );
        let cells = picture::grid(self.colours);
        let mut differences = Vec::new();
        for (row, line) in picture.iter().enumerate() {
            if !rows.contains(&row) {
                continue;
            }
            let y = row + 1;
            if !buf[(0, y as u16)].symbol().trim().is_empty() {
                differences.push(format!("row {row}: drawn in the panel's inset"));
            }
            let instead = self.instead.iter().find(|(at, ..)| *at == row);
            let wanted = match instead {
                Some((_, ours, _)) => (*ours).to_owned(),
                None => line.trim_end().to_owned(),
            };
            let got = if self.days.contains(&row) {
                solid(&text_of(buf, y))
            } else {
                text_of(buf, y)
            };
            if got != wanted {
                differences.push(format!(
                    "row {row}:\n  picture {wanted:?}\n  drawn   {got:?}"
                ));
                continue;
            }
            if instead.is_some() || self.colours_elsewhere.contains(&row) {
                continue;
            }
            for (x, cell) in cells[row].iter().enumerate() {
                let drawn = &buf[(x as u16 + 1, y as u16)];
                let place = format!("row {row} column {x} {:?}", cell.ch);
                if self.days.contains(&row) && cell.ch == '█' {
                    let level = cell.color.as_deref().and_then(strength).unwrap_or(4);
                    let glyph = ["░", "▒", "▓", "█"][level - 1];
                    if drawn.symbol() != glyph {
                        differences
                            .push(format!("{place}: drawn {:?}, not {glyph}", drawn.symbol()));
                    }
                }
                let bg = cell
                    .background
                    .as_deref()
                    .map_or(Color::Reset, |bg| colour(&theme, bg));
                if drawn.bg != bg {
                    differences.push(format!("{place}: background {:?}, not {bg:?}", drawn.bg));
                }
                if cell.ch == ' ' {
                    continue;
                }
                let fg = cell
                    .color
                    .as_deref()
                    .map_or(Color::Reset, |fg| colour(&theme, fg));
                if drawn.fg != fg {
                    differences.push(format!("{place}: colour {:?}, not {fg:?}", drawn.fg));
                }
                let bold = drawn.modifier.contains(Modifier::BOLD);
                if bold != cell.bold {
                    differences.push(format!("{place}: bold {bold}, not {}", cell.bold));
                }
            }
        }
        assert!(differences.is_empty(), "{}", differences.join("\n"));
        for (row, ours, why) in self.instead.iter().filter(|(row, ..)| rows.contains(row)) {
            assert_ne!(
                picture[*row].trim_end(),
                *ours,
                "row {row} matches now: {why}"
            );
        }
    }
}

/// The numbers under the year are the numbers picture's rows, whose colours
/// [`the_numbers_match_their_picture`] compares.
#[test]
fn the_overview_sub_tab_matches_its_picture() {
    let state = state();
    let mut view = tab(sample::numbers());
    key(&mut view, &state, crossterm::event::KeyCode::Down);
    let buf = render(&mut view, &state, OVERVIEW_HEIGHT);
    Screen {
        text: OVERVIEW_TEXT,
        colours: OVERVIEW_COLOURS,
        days: 5..=11,
        colours_elsewhere: 17..30,
        instead: &[
            (
                31,
                "Your input and output are ~7087x the tokens in Animal Farm",
                "the picture's ~920x of A Game of Thrones can't come from 69.1M in and 207.3M \
                 out, and the fact is picked by the day, as Claude Code picks one at random",
            ),
            (
                33,
                "Stats from the last 147 days · imported: Claude Code since Oct 2025, Codex since \
                 Feb · ran here: agy, OpenCode",
                "the numbers above it count 147 days, where the picture's last row was drawn \
                 for 365",
            ),
        ],
    }
    .compare("ours_overview", &buf);
}

/// The three groups and the two bars from the numbers picture's example, at its 116 cells,
/// every cell with its colour. The page names the kinds of tokens by its faint, blue and
/// orange, which here are the tokens' own colours. The Tokens group's values stand three
/// cells after its longest label, a CLI's name, and the Days group where it always stood.
#[test]
fn the_numbers_match_their_picture() {
    numbers_match("overview_numbers", &sample::numbers());
}

/// Where some of the period's tokens were free or paid, a third bar says who paid, under the
/// kinds' bar and as wide, with what the paid part cost after its legend.
#[test]
fn who_paid_matches_its_picture() {
    numbers_match("overview_numbers_who_paid", &sample::numbers_who_paid());
}

/// The numbers card drawn from `data` at its picture's width, in a terminal that draws logos,
/// against the picture `name`. Each CLI on the By CLI row has its logo, which the picture
/// writes as its stand-ins.
fn numbers_match(name: &str, data: &argonauts_core::app::overview::OverviewData) {
    use crate::base::kit::logo::golden::{count, pictured, with_stand_ins};
    use crate::panels::home::numbers::Numbers;
    use crate::testing::pictures::golden::{self, Picture, Region, keep};

    let picture = Picture::load(name)
        .standing_for("--term-faint", Role::TokensCached)
        .standing_for("--term-blu", Role::TokensInput)
        .standing_for("--term-org", Role::TokensOutput);
    let theme = pictured(&golden::theme());
    let names = crate::testing::sample::names();
    let numbers = Numbers {
        data,
        names: &names,
    };
    let drawn = numbers.block(&Kit::new(&theme), picture.width());
    let area = Rect::new(0, 0, picture.width() as u16, drawn.lines.len() as u16);
    let mut buf = Buffer::empty(area);
    for (y, line) in drawn.lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, area.width);
    }
    assert_eq!(count(&buf), 4, "a logo for each CLI");
    let shown = with_stand_ins(&buf);
    keep(name, &shown);
    assert_eq!(drawn.lines.len(), picture.rows.len());
    golden::assert_matches(&picture, &shown, &Region::whole(&picture, (0, 0)), &theme);
}

/// Each CLI in the By CLI legend keys its part of the bar with a swatch in its colour, then
/// has its logo where the terminal draws one, then its whole name.
#[test]
fn the_by_cli_legend_keys_each_cli_by_its_colour_then_its_logo() {
    use crate::panels::home::numbers::Numbers;

    let names = crate::testing::sample::names();
    let data = sample::numbers();
    let numbers = Numbers {
        data: &data,
        names: &names,
    };
    let rows = |theme: &Theme| {
        let drawn = numbers.block(&Kit::new(theme), 116);
        let lines = crate::base::kit::logo::golden::lines_with_stand_ins(&drawn.lines);
        crate::base::text::plain_text(&lines)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let plain = theme();
    let without = rows(&plain);
    let pictured = rows(&crate::base::kit::logo::golden::pictured(&plain));
    let legend = |rows: &[String]| rows[rows.len() - 2..].join("\n");
    let without = legend(&without);
    assert!(
        without.contains("▌ Claude Code    88%      ▌ Codex       12%"),
        "{without}"
    );
    assert!(
        without.ends_with("▌ agy            <1%      ▌ OpenCode    <1%"),
        "{without}"
    );
    let pictured = legend(&pictured);
    assert!(
        pictured.contains("▌ ◧◨ Claude Code    88%      ▌ ◧◨ Codex       12%"),
        "{pictured}"
    );
    assert!(
        pictured.ends_with("▌ ◧◨ agy            <1%      ▌ ◧◨ OpenCode    <1%"),
        "{pictured}"
    );
}

/// A font that draws a square or a bar wider than its cell runs it into whatever touches it,
/// so at every width a blank stands between each of them and the words beside it.
#[test]
fn no_word_touches_a_bar_or_a_square_of_the_numbers() {
    use crate::base::kit::bars::touching_bar;
    use crate::panels::home::numbers::Numbers;

    let theme = theme();
    let names = crate::testing::sample::names();
    for data in [sample::numbers(), sample::numbers_who_paid()] {
        let numbers = Numbers {
            data: &data,
            names: &names,
        };
        for width in [116, 100, 80, 60, 40, 24] {
            let drawn = numbers.block(&Kit::new(&theme), width);
            for row in crate::base::text::plain_text(&drawn.lines).lines() {
                assert_eq!(touching_bar(row), None, "{width}: {row:?}");
                assert!(
                    crate::base::text::str_width(row) <= width,
                    "{width}: {row:?}"
                );
            }
        }
    }
}

#[test]
fn the_models_sub_tab_matches_its_picture() {
    let state = state();
    let mut view = tab(sample::models());
    key(&mut view, &state, crossterm::event::KeyCode::Down);
    key(&mut view, &state, crossterm::event::KeyCode::Right);
    let buf = render(&mut view, &state, HEIGHT);
    Screen {
        text: MODELS_TEXT,
        colours: MODELS_COLOURS,
        days: 0..=0,
        colours_elsewhere: 0..0,
        instead: &[(
            16,
            "       Aug 18          Sep 3            Sep 20           Oct 7",
            "each date stands under its own day's column, one day a column; the picture's \
             Sep 1 and Sep 15 sit 17 columns apart for 14 days",
        )],
    }
    .compare("ours_models", &buf);
}

/// The models' comparison fails once one cell of the picture is changed: its character, its
/// colour, its weight or its fill.
#[test]
fn a_picture_changed_in_one_cell_no_longer_matches_the_models() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let state = state();
    let mut view = tab(sample::models());
    key(&mut view, &state, crossterm::event::KeyCode::Down);
    key(&mut view, &state, crossterm::event::KeyCode::Right);
    let buf = render(&mut view, &state, HEIGHT);
    let fails = |text: &str, colours: &str| {
        let screen = Screen {
            text,
            colours,
            days: 0..=0,
            colours_elsewhere: 0..0,
            instead: &[],
        };
        // The row the screen draws otherwise is left out, the rest compared.
        catch_unwind(AssertUnwindSafe(|| {
            screen.compare_rows("changed", &buf, 1..16)
        }))
        .is_err()
    };
    assert!(!fails(MODELS_TEXT, MODELS_COLOURS));
    let grid = picture::grid(MODELS_COLOURS);
    let (row, column) = picture::find(&grid[1..16], |cell| {
        cell.ch.is_alphanumeric() && cell.color.is_some()
    });
    let row = row + 1;
    let changed = |edit: &dyn Fn(&mut picture::Cell)| {
        picture::edited(MODELS_COLOURS, |grid| edit(&mut grid[row][column]))
    };
    let recoloured = changed(&|cell| {
        let other = match cell.color.as_deref() {
            Some("var(--term-dim)") => "var(--term-acc)",
            _ => "var(--term-dim)",
        };
        cell.color = Some(other.to_owned());
    });
    assert!(fails(MODELS_TEXT, &recoloured), "a colour changed");
    assert!(
        fails(MODELS_TEXT, &changed(&|cell| cell.bold ^= true)),
        "a weight changed"
    );
    let filled = changed(&|cell| cell.background = Some("var(--term-band)".to_owned()));
    assert!(fails(MODELS_TEXT, &filled), "a fill changed");
    let mut rows: Vec<String> = MODELS_TEXT.lines().map(str::to_owned).collect();
    let mut chars: Vec<char> = rows[row].chars().collect();
    chars[column] = '#';
    rows[row] = chars.into_iter().collect();
    assert!(
        fails(&rows.join("\n"), MODELS_COLOURS),
        "a character changed"
    );
}

/// Tab moves the sub-tabs from the models too, so with the keys there Models stays filled as
/// the picture has it. Shift+Tab takes the fill along to Overview, which has nothing to pick,
/// so the keys go up to the sub-tabs, and Tab brings it back.
#[test]
fn in_the_models_the_chosen_sub_tab_stays_filled() {
    use crate::panels::view::subtabs::Level;
    use crossterm::event::KeyCode;

    let state = state();
    let mut view = tab(sample::models());
    for code in [KeyCode::Down, KeyCode::Right, KeyCode::Down] {
        key(&mut view, &state, code);
    }
    assert_eq!(view.level(), Level::Cards);
    let head = Screen {
        text: MODELS_TEXT,
        colours: MODELS_COLOURS,
        days: 0..=0,
        colours_elsewhere: 0..0,
        instead: &[],
    };
    head.compare_rows(
        "ours_models_in_the_cards",
        &render(&mut view, &state, HEIGHT),
        0..3,
    );
    key(&mut view, &state, KeyCode::BackTab);
    assert_eq!(view.level(), Level::SubTabs, "Overview has nothing to pick");
    key(&mut view, &state, KeyCode::Tab);
    head.compare_rows(
        "ours_models_after_tab",
        &render(&mut view, &state, HEIGHT),
        0..3,
    );
}
