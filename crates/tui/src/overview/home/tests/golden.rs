//! The Overview's two screens, drawn from the pictures' example numbers at the pictures' width
//! and compared with them cell for cell, colours included. The pictures draw `░ ▒ ▓ █` the way
//! Ghostty does, as `█` at four strengths, so a day's glyph is checked against its strength.
//!
//! The panel is drawn at 120 columns, as every other tab's golden draws it, and every row from
//! the tab row to the key line is compared one row down and one column in, the panel's rule
//! and inset. Where a number in a picture can't come from the numbers beside it, the row says
//! what it draws instead and why.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::{key, state, tab};
use crate::kit::Kit;
use crate::overview::OverviewView;
use crate::overview::home::sample;
use crate::state::AppState;
use crate::theme::{ColorLevel, Glyphs, Role, Theme};
use crate::views::{View, ViewCtx};

const OVERVIEW_TEXT: &str = include_str!("../pictures/ours_overview.txt");
const OVERVIEW_COLOURS: &str = include_str!("../pictures/ours_overview.html");
const MODELS_TEXT: &str = include_str!("../pictures/ours_models.txt");
const MODELS_COLOURS: &str = include_str!("../pictures/ours_models.html");

/// The panel's width for the pictures' 118 cells, and its height: its rule, then the
/// picture's 31 rows.
const WIDTH: u16 = 120;
const HEIGHT: u16 = 32;

/// One cell of a picture: its character and how the page colours it.
#[derive(Debug, Clone)]
struct Painted {
    ch: char,
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
}

/// The picture's cells, row by row, from its page's spans.
fn painted(html: &str) -> Vec<Vec<Painted>> {
    let body = html
        .split_once("<pre class=\"term grid\">")
        .and_then(|(_, rest)| rest.split_once("</pre>"))
        .map(|(body, _)| body)
        .expect("a picture's grid");
    body.split('\n').map(painted_row).collect()
}

fn painted_row(mut line: &str) -> Vec<Painted> {
    let mut cells = Vec::new();
    while !line.is_empty() {
        let (style, text, rest) = match line.strip_prefix("<span style=\"") {
            Some(span) => {
                let (style, rest) = span.split_once("\">").expect("a span's style");
                let (text, rest) = rest.split_once("</span>").expect("a span's end");
                (style, text, rest)
            }
            None => {
                let end = line.find('<').unwrap_or(line.len());
                ("", &line[..end], &line[end..])
            }
        };
        let mut fg = None;
        let mut bg = None;
        let mut bold = false;
        for part in style.split(';') {
            if let Some(value) = part.strip_prefix("color:") {
                fg = Some(value.to_owned());
            } else if let Some(value) = part.strip_prefix("background:") {
                bg = Some(value.to_owned());
            } else if part == "font-weight:700" {
                bold = true;
            }
        }
        for ch in unescape(text).chars() {
            cells.push(Painted {
                ch,
                fg: fg.clone(),
                bg: bg.clone(),
                bold,
            });
        }
        line = rest;
    }
    cells
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

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
        "--m-sonnet" => {
            let shade = crate::overview::stats::draw::shade(&kit, Role::CliClaude, 1);
            return shade.fg.unwrap_or(Color::Reset);
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
        let drawn = crate::picture_dump::cut(buf, (1, 1), (width, picture.len()));
        crate::picture_dump::rows(name, &drawn);
        let cells = painted(self.colours);
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
                    let level = cell.fg.as_deref().and_then(strength).unwrap_or(4);
                    let glyph = ["░", "▒", "▓", "█"][level - 1];
                    if drawn.symbol() != glyph {
                        differences
                            .push(format!("{place}: drawn {:?}, not {glyph}", drawn.symbol()));
                    }
                }
                let bg = cell
                    .bg
                    .as_deref()
                    .map_or(Color::Reset, |bg| colour(&theme, bg));
                if drawn.bg != bg {
                    differences.push(format!("{place}: background {:?}, not {bg:?}", drawn.bg));
                }
                if cell.ch == ' ' {
                    continue;
                }
                let fg = cell
                    .fg
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
    let buf = render(&mut view, &state, HEIGHT);
    Screen {
        text: OVERVIEW_TEXT,
        colours: OVERVIEW_COLOURS,
        days: 5..=11,
        colours_elsewhere: 17..25,
        instead: &[
            (
                26,
                "Your input and output are ~7087x the tokens in Animal Farm",
                "the picture's ~920x of A Game of Thrones can't come from 69.1M in and 207.3M \
                 out, and the fact is picked by the day, as Claude Code picks one at random",
            ),
            (
                28,
                "Stats from the last 147 days · imported: Claude Code since Oct 2025, Codex since \
                 Feb, agy since Jun",
                "the numbers above it count 147 days, where the picture's last row was drawn \
                 for 365",
            ),
        ],
    }
    .compare("ours_overview", &buf);
}

/// The three groups and the two bars from the numbers picture's example, at its 116 cells,
/// every cell with its colour. The page names the kinds of tokens by its faint, blue and
/// orange, which here are the tokens' own colours. The picture writes the app's name in full,
/// which it was first drawn without: the Tokens group's values and notes and the whole Days
/// group stand three cells further right for it, and Sessions where it was.
#[test]
fn the_numbers_match_their_picture() {
    use crate::overview::golden::{self, Picture, Region};
    use crate::overview::home::numbers::Numbers;

    let picture = Picture::load("overview_numbers")
        .standing_for("--term-faint", Role::TokensCached)
        .standing_for("--term-blu", Role::TokensInput)
        .standing_for("--term-org", Role::TokensOutput);
    let theme = golden::theme();
    let names = crate::overview::sample::names();
    let data = sample::numbers();
    let numbers = Numbers {
        data: &data,
        names: &names,
    };
    let drawn = numbers.block(&Kit::new(&theme), picture.width());
    assert_eq!(drawn.lines.len(), picture.rows.len());
    let area = Rect::new(0, 0, picture.width() as u16, drawn.lines.len() as u16);
    let mut buf = Buffer::empty(area);
    for (y, line) in drawn.lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, area.width);
    }
    golden::assert_matches(&picture, &buf, &Region::whole(&picture, (0, 0)), &theme);
}

/// A font that draws a square or a bar wider than its cell runs it into whatever touches it,
/// so at every width a blank stands between each of them and the words beside it.
#[test]
fn no_word_touches_a_bar_or_a_square_of_the_numbers() {
    use crate::kit::bars::touching_bar;
    use crate::overview::home::numbers::Numbers;

    let theme = theme();
    let names = crate::overview::sample::names();
    let data = sample::numbers();
    let numbers = Numbers {
        data: &data,
        names: &names,
    };
    for width in [116, 100, 80, 60, 40, 24] {
        let drawn = numbers.block(&Kit::new(&theme), width);
        for row in crate::text::plain_text(&drawn.lines).lines() {
            assert_eq!(touching_bar(row), None, "{width}: {row:?}");
            assert!(crate::text::str_width(row) <= width, "{width}: {row:?}");
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

/// Tab moves the sub-tabs from the models too, so with the keys there Models stays filled as
/// the picture has it, and Tab takes the fill along to Overview and back.
#[test]
fn in_the_models_the_chosen_sub_tab_stays_filled() {
    use crate::overview::subtabs::Level;
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
    key(&mut view, &state, KeyCode::Tab);
    key(&mut view, &state, KeyCode::BackTab);
    assert_eq!(
        view.level(),
        Level::Cards,
        "Tab leaves the keys in the models"
    );
    head.compare_rows(
        "ours_models_after_tab",
        &render(&mut view, &state, HEIGHT),
        0..3,
    );
}
