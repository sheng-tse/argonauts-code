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
use crate::overview::OverviewView;
use crate::overview::home::sample;
use crate::state::AppState;
use crate::theme::{ColorLevel, Glyphs, Role, Theme};
use crate::views::{View, ViewCtx};
use crate::widgets::Kit;

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
        "--term-acc" => Role::Card,
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

/// What one screen should differ from its picture by, and why.
struct Screen<'a> {
    text: &'a str,
    colours: &'a str,
    /// Picture rows of the year of days, whose glyphs are drawn solid in the picture.
    days: std::ops::RangeInclusive<usize>,
    /// Picture rows this screen draws otherwise: the row, what it draws, and why.
    instead: &'a [(usize, &'a str, &'a str)],
}

impl Screen<'_> {
    fn compare(&self, name: &str, buf: &Buffer) {
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
            if instead.is_some() {
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
        for (row, ours, why) in self.instead {
            assert_ne!(
                picture[*row].trim_end(),
                *ours,
                "row {row} matches now: {why}"
            );
        }
    }
}

#[test]
fn the_overview_sub_tab_matches_its_picture() {
    let state = state();
    let mut view = tab(sample::overview());
    key(&mut view, &state, crossterm::event::KeyCode::Down);
    let buf = render(&mut view, &state, HEIGHT);
    Screen {
        text: OVERVIEW_TEXT,
        colours: OVERVIEW_COLOURS,
        days: 5..=11,
        instead: &[
            (
                26,
                "Your input and output are ~42051x the tokens in Animal Farm",
                "the picture's ~920x of A Game of Thrones can't come from 1.2b in and 0.4b out \
                 (that is ~4304x), and the fact is picked by the day, as Claude Code picks one \
                 at random",
            ),
            (
                27,
                "93% of your input came from cache; 64% of your tokens ran in subagents",
                "110.0b read of 118.0b input, cache writes included, is 93%, not the picture's 97%",
            ),
        ],
    }
    .compare("ours_overview", &buf);
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
        instead: &[(
            16,
            "       Aug 18          Sep 3            Sep 20           Oct 7",
            "each date stands under its own day's column, one day a column; the picture's \
             Sep 1 and Sep 15 sit 17 columns apart for 14 days",
        )],
    }
    .compare("ours_models", &buf);
}
