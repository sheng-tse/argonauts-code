//! Screens the app builds, drawn from their pictures' own examples at the pictures' width and
//! compared with them cell for cell, colours included. Each picture in `tests/golden/` holds
//! only the rows drawn here: its `.txt` the characters, its `.html` the colours.

use std::time::Instant;

use argonauts_testkit::pictures::picture;
use argonauts_tui::base::draw::render::{Clock, RenderCtx};
use argonauts_tui::base::theme::{ColorLevel, Glyphs, Role, Theme};
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

/// The pictures' width.
pub(crate) const WIDTH: u16 = 118;

pub(crate) fn theme() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::TrueColor);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

pub(crate) fn still(theme: &Theme) -> RenderCtx<'_> {
    RenderCtx::new(WIDTH, theme, Clock::still(Instant::now()))
}

/// Each cell's look in a picture: its colour by name, and whether it is bold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Look {
    colour: Option<&'static str>,
    bold: bool,
}

/// The pictures' colour names, each with the theme role that draws it. The accent is the
/// periwinkle questions are drawn in.
const ROLES: [(&str, Role); 6] = [
    ("acc", Role::Question),
    ("dim", Role::Dim),
    ("faint", Role::Faint),
    ("grn", Role::Success),
    ("red", Role::Danger),
    ("amb", Role::Warn),
];

/// The picture's rows, without the spaces that pad them to its width.
pub(crate) fn picture_rows(text: &str) -> Vec<String> {
    text.lines().map(|row| row.trim_end().to_owned()).collect()
}

/// Writes `lines` into the folder `ARGONAUTS_PICTURE_DUMP` names, as `<name>.txt` and as
/// `<name>.html` in the pictures' own form, colours included, so a drawing can be put beside
/// its picture. Nothing is written when the variable isn't set.
pub(crate) fn dump(name: &str, lines: &[Line<'_>]) {
    let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_DUMP") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let text = drawn_rows(lines).join("\n") + "\n";
    let page = format!(
        "<div class=\"scroll\">{}</div>\n",
        drawn_page(&theme(), lines)
    );
    if let Err(err) = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(dir.join(format!("{name}.txt")), text))
        .and_then(|()| std::fs::write(dir.join(format!("{name}.html")), page))
    {
        eprintln!("couldn't write the drawing of {name}: {err}");
    }
}

/// `lines` as a picture's page: each cell in the colour the picture names it by, or spelled
/// out where no name draws in it, and its weight.
fn drawn_page(theme: &Theme, lines: &[Line<'_>]) -> String {
    let grid: Vec<Vec<picture::Cell>> = lines
        .iter()
        .map(|line| {
            let mut row: Vec<picture::Cell> = line
                .spans
                .iter()
                .flat_map(|span| {
                    let style = line.style.patch(span.style);
                    span.content.chars().map(move |ch| picture::Cell {
                        ch,
                        color: match colour_name(theme, style.fg) {
                            None => None,
                            Some("another colour") => style.fg.map(spelled_out),
                            Some(name) => Some(format!("var(--term-{name})")),
                        },
                        background: None,
                        bold: style.add_modifier.contains(Modifier::BOLD),
                    })
                })
                .collect();
            while row.last().is_some_and(|cell| cell.ch == ' ') {
                row.pop();
            }
            row
        })
        .collect();
    picture::page(&grid)
}

fn spelled_out(colour: Color) -> String {
    match colour {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        other => format!("{other:?}"),
    }
}

pub(crate) fn drawn_rows(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            text.trim_end().to_owned()
        })
        .collect()
}

/// Every drawn cell's colour and weight against the picture's, blanks aside, which show only
/// their background.
pub(crate) fn assert_looks(theme: &Theme, lines: &[Line<'_>], html: &str) {
    assert_looks_except(theme, lines, html, |_, _| false);
}

/// [`assert_looks`], leaving out the cells `drawn_otherwise` names by row and column, which a
/// test lists with the reason the screen draws them otherwise.
pub(crate) fn assert_looks_except(
    theme: &Theme,
    lines: &[Line<'_>],
    html: &str,
    drawn_otherwise: impl Fn(usize, usize) -> bool,
) {
    let picture = picture_looks(html);
    assert_eq!(lines.len(), picture.len(), "the picture's rows");
    for (row, (line, want)) in lines.iter().zip(&picture).enumerate() {
        let cells = line.spans.iter().flat_map(|span| {
            let style = line.style.patch(span.style);
            span.content.chars().map(move |c| (c, style))
        });
        for (column, ((c, style), (want_c, want))) in cells.zip(want).enumerate() {
            assert_eq!(c, *want_c, "row {row} column {column}");
            if c.is_whitespace() || drawn_otherwise(row, column) {
                continue;
            }
            let got = Look {
                colour: colour_name(theme, style.fg),
                bold: style.add_modifier.contains(Modifier::BOLD),
            };
            assert_eq!(&got, want, "row {row} column {column} {c:?}");
        }
    }
}

fn picture_looks(html: &str) -> Vec<Vec<(char, Look)>> {
    let mut lines: Vec<Vec<(char, Look)>> = picture::grid(html)
        .iter()
        .map(|row| row.iter().map(|cell| (cell.ch, look_of(cell))).collect())
        .collect();
    // A picture ends its last row with a newline, or with spaces up to its width.
    while lines
        .last()
        .is_some_and(|line| line.iter().all(|(c, _)| c.is_whitespace()))
    {
        lines.pop();
    }
    lines
}

fn look_of(cell: &picture::Cell) -> Look {
    let colour = match cell.color.as_deref() {
        Some("#b1b9f9") => Some("acc"),
        Some(colour) => ROLES
            .into_iter()
            .map(|(name, _)| name)
            .find(|name| colour == format!("var(--term-{name})")),
        None => None,
    };
    Look {
        colour,
        bold: cell.bold,
    }
}

/// The picture's name for a drawn colour; `None` for the terminal's own foreground, which the
/// theme draws as plain or bright text.
fn colour_name(theme: &Theme, colour: Option<Color>) -> Option<&'static str> {
    let plain = [Role::Text, Role::Bright].map(|role| theme.style(role).fg);
    if plain.contains(&colour) {
        return None;
    }
    let named = ROLES
        .into_iter()
        .find(|(_, role)| theme.style(*role).fg == colour);
    named.map(|(name, _)| name).or(Some("another colour"))
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use argonauts_testkit::pictures::picture;
    use ratatui::style::Style;
    use ratatui::text::Span;

    use super::*;

    const HTML: &str = include_str!("../../tests/golden/trust_question.html");

    /// The picture's own cells, each drawn in the role its colour names.
    fn painted(theme: &Theme) -> Vec<Line<'static>> {
        picture_looks(HTML)
            .into_iter()
            .map(|row| {
                let spans = row.into_iter().map(|(c, look)| {
                    let role = ROLES
                        .into_iter()
                        .find(|(name, _)| Some(*name) == look.colour);
                    let mut style = role.map_or(Style::default(), |(_, role)| theme.style(role));
                    if look.bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    Span::styled(c.to_string(), style)
                });
                Line::from(spans.collect::<Vec<_>>())
            })
            .collect()
    }

    fn fails(theme: &Theme, lines: &[Line<'_>], html: &str) -> bool {
        catch_unwind(AssertUnwindSafe(|| assert_looks(theme, lines, html))).is_err()
    }

    #[test]
    fn one_changed_cell_fails_the_comparison() {
        let theme = theme();
        let lines = painted(&theme);
        assert_looks(&theme, &lines, HTML);
        let grid = picture::grid(HTML);
        let (row, column) = picture::find(&grid, |cell| cell.color.is_some());
        let recoloured = picture::edited(HTML, |grid| {
            let cell = &mut grid[row][column];
            cell.color = match cell.color.as_deref() {
                Some("var(--term-red)") => Some("var(--term-dim)".to_owned()),
                _ => Some("var(--term-red)".to_owned()),
            };
        });
        assert!(fails(&theme, &lines, &recoloured), "a colour changed");
        let respelled = picture::edited(HTML, |grid| grid[row][column].ch = '#');
        assert!(fails(&theme, &lines, &respelled), "a character changed");
        let bolder = picture::edited(HTML, |grid| grid[row][column].bold ^= true);
        assert!(fails(&theme, &lines, &bolder), "a weight changed");
    }
}
