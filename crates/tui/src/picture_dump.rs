//! Writes what a golden test drew for one of the pictures into the folder that
//! `ARGONAUTS_PICTURE_DUMP` names, a row a line, so it can be put beside its picture. A picture
//! checked in parts is written a part at a time, each named by its rows, and beside each `.txt`
//! an `.html` of the same cells in the pictures' own form, colours included. Nothing is written
//! when the variable isn't set.

use std::path::{Path, PathBuf};

use argonauts_testkit::picture;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::theme::{Role, Theme};

/// `rows` as drawn for the picture `name`.
pub fn rows(name: &str, rows: &[String]) {
    let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_DUMP").map(PathBuf::from) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let text: String = rows
        .iter()
        .map(|row| format!("{}\n", row.trim_end()))
        .collect();
    let _ = std::fs::write(dir.join(format!("{name}.txt")), text);
}

pub fn lines(name: &str, lines: &[Line<'_>]) {
    let drawn: Vec<String> = lines.iter().map(ToString::to_string).collect();
    rows(name, &drawn);
}

/// The cells of `buf` from `at` (column, row), `size` (columns, rows) of them.
pub fn cut(buf: &Buffer, at: (usize, usize), size: (usize, usize)) -> Vec<String> {
    (0..size.1)
        .map(|row| {
            (0..size.0)
                .map(|column| {
                    let place = ((at.0 + column) as u16, (at.1 + row) as u16);
                    buf.cell(place).map_or(" ", |cell| cell.symbol()).to_owned()
                })
                .collect()
        })
        .collect()
}

/// The pictures' names for the theme's colours. A colour no name here stands for is written as
/// it is, `#rrggbb`.
const NAMES: &[(&str, Role)] = &[
    ("var(--term-fg)", Role::Text),
    ("var(--term-dim)", Role::Dim),
    ("var(--term-faint)", Role::Faint),
    ("var(--term-org)", Role::Accent),
    ("var(--term-org)", Role::Shimmer),
    ("var(--term-acc)", Role::Question),
    ("var(--term-grn)", Role::Success),
    ("var(--term-red)", Role::Danger),
    ("var(--term-amb)", Role::Warn),
    ("var(--term-band)", Role::UserBand),
    ("var(--v-claude)", Role::CliClaude),
    ("var(--v-codex)", Role::CliCodex),
];

/// `lines` as drawn, in the pictures' own form: an `.html` page whose spans give each cell the
/// colour, fill and weight the screen drew it in, beside the `.txt` that [`lines`] writes.
pub fn lines_page(name: &str, lines: &[Line<'_>], theme: &Theme) {
    let cells: Vec<Vec<(String, Style)>> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .flat_map(|span| {
                    let style = line.style.patch(span.style);
                    span.content.chars().map(move |ch| (ch.to_string(), style))
                })
                .collect()
        })
        .collect();
    page(name, &cells, theme);
}

/// The cells of `buf` that [`cut`] reads, as [`lines_page`] writes them.
pub fn buffer_page(
    name: &str,
    buf: &Buffer,
    at: (usize, usize),
    size: (usize, usize),
    theme: &Theme,
) {
    page(name, &buffer_cells(buf, at, size), theme);
}

/// All of `buf` as a page, written as `<stem>.html` into `dir`.
pub fn screen_page_in(dir: &Path, stem: &str, buf: &Buffer, theme: &Theme) {
    let size = (usize::from(buf.area.width), usize::from(buf.area.height));
    write_page(dir, stem, &buffer_cells(buf, (0, 0), size), theme);
}

fn buffer_cells(
    buf: &Buffer,
    at: (usize, usize),
    size: (usize, usize),
) -> Vec<Vec<(String, Style)>> {
    (0..size.1)
        .map(|row| {
            (0..size.0)
                .filter_map(|column| {
                    let place = ((at.0 + column) as u16, (at.1 + row) as u16);
                    buf.cell(place)
                        .map(|cell| (cell.symbol().to_owned(), cell.style()))
                })
                .collect()
        })
        .collect()
}

fn page(name: &str, cells: &[Vec<(String, Style)>], theme: &Theme) {
    if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_DUMP").map(PathBuf::from) {
        write_page(&dir, name, cells, theme);
    }
}

fn write_page(dir: &Path, name: &str, cells: &[Vec<(String, Style)>], theme: &Theme) {
    let grid: Vec<Vec<picture::Cell>> = cells
        .iter()
        .map(|row| {
            let mut row: Vec<picture::Cell> = row
                .iter()
                .flat_map(|(symbol, style)| {
                    symbol.chars().map(move |ch| picture::Cell {
                        ch,
                        color: named(style.fg, theme, |role| theme.style(role).fg),
                        background: named(style.bg, theme, |role| theme.style(role).bg),
                        bold: style.add_modifier.contains(Modifier::BOLD),
                    })
                })
                .collect();
            while row
                .last()
                .is_some_and(|cell| cell.ch == ' ' && cell.background.is_none())
            {
                row.pop();
            }
            row
        })
        .collect();
    let page = format!("<div class=\"scroll\">{}</div>\n", picture::page(&grid));
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(format!("{name}.html")), page);
}

/// What a picture calls `colour`: the name of the role the theme draws in it, or the colour
/// spelled out. `None` where the terminal's own colour shows.
fn named(
    colour: Option<Color>,
    theme: &Theme,
    of: impl Fn(Role) -> Option<Color>,
) -> Option<String> {
    let colour = colour.filter(|colour| *colour != Color::Reset)?;
    let named = NAMES
        .iter()
        .find(|(_, role)| of(*role) == Some(colour))
        .map(|(name, _)| (*name).to_owned());
    named.or_else(|| {
        if theme.style(Role::Bright).fg == Some(colour) {
            return Some("var(--term-fg)".to_owned());
        }
        Some(match colour {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            other => format!("{other:?}"),
        })
    })
}
