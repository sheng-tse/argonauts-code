//! Compares a drawn screen with its picture cell for cell: the characters with the `.txt`, and
//! the colors, boldness and backgrounds with the `.html` that draws the same grid. A picture
//! names its colors as the panel's own (`--term-dim` and so on), which map to theme roles.

use argonauts_testkit::pictures::picture;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::base::theme::{Role, Theme};

/// A cell the screen is known to draw otherwise than its picture, and why.
#[derive(Debug, Clone, Copy)]
pub struct Unlike {
    pub row: usize,
    /// The first and last column, both included.
    pub cols: (usize, usize),
    pub why: &'static str,
}

/// A color the picture spells out, `#rrggbb`, as the theme's color level draws it. `None` for
/// any other color, and where the terminal shows no color at all.
fn spelled_out(var: &str, theme: &Theme) -> Option<Option<Color>> {
    use crate::base::theme::color::{ColorLevel, nearest_ansi16, nearest_ansi256};

    let digits = var.strip_prefix('#').filter(|digits| digits.len() == 6)?;
    let [_, r, g, b] = u32::from_str_radix(digits, 16).ok()?.to_be_bytes();
    Some(match theme.level {
        ColorLevel::TrueColor => Some(Color::Rgb(r, g, b)),
        ColorLevel::Ansi256 => Some(Color::Indexed(nearest_ansi256((r, g, b)))),
        ColorLevel::Ansi16 => Some(nearest_ansi16((r, g, b))),
        ColorLevel::None => None,
    })
}

/// The theme roles a picture's color stands for. Empty for the colors only a highlighter or
/// a chart picks, which are checked for being there at all. The working color has its
/// lighter shade too, which sweeps across the verb and which a still picture can't show, and
/// a diff's green and red are its own shades, for the marker and the rows behind it.
fn roles_of(var: &str) -> &'static [Role] {
    match var {
        "var(--term-dim)" => &[Role::Dim],
        "var(--term-faint)" => &[Role::Faint],
        "var(--term-org)" => &[Role::Accent, Role::Shimmer],
        "var(--term-acc)" => &[Role::Question],
        "var(--term-grn)" => &[Role::Success, Role::AddedMarker],
        "var(--term-red)" => &[Role::Danger, Role::RemovedMarker],
        "var(--term-amb)" => &[Role::Warn],
        "var(--term-band)" => &[Role::UserBand],
        "var(--v-claude)" => &[Role::CliClaude],
        "var(--v-codex)" => &[Role::CliCodex],
        "rgba(255,122,140,.18)" => &[Role::RemovedRow, Role::RemovedWord],
        "rgba(92,200,120,.18)" => &[Role::AddedRow, Role::AddedWord],
        _ => &[],
    }
}

/// Each cell of the drawn rows with its style, the line's own style under the span's.
fn cells(lines: &[Line<'_>]) -> Vec<Vec<(char, Style)>> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .flat_map(|span| {
                    let style = line.style.patch(span.style);
                    span.content.chars().map(move |ch| (ch, style))
                })
                .collect()
        })
        .collect()
}

fn excused(unlike: &[Unlike], row: usize, col: usize) -> bool {
    unlike
        .iter()
        .any(|u| u.row == row && (u.cols.0..=u.cols.1).contains(&col))
}

/// Asserts the drawn rows match the picture, but for the cells `unlike` names.
pub fn assert_matches(
    name: &str,
    lines: &[Line<'_>],
    txt: &str,
    html: &str,
    theme: &Theme,
    unlike: &[Unlike],
) {
    crate::testing::pictures::picture_dump::lines(name, lines);
    crate::testing::pictures::picture_dump::lines_page(name, lines, theme);
    let want: Vec<&str> = txt.trim_end_matches('\n').split('\n').collect();
    rows_match(name, lines, &want, &picture::grid(html), theme, unlike);
}

/// Asserts the drawn `lines` are the picture's rows `rows`, but for the cells `unlike` names,
/// counting rows from the first of `rows`. For a picture whose rows the screen draws a part at
/// a time, with other rows between.
pub fn assert_rows_match(
    name: &str,
    lines: &[Line<'_>],
    (txt, html): (&str, &str),
    rows: std::ops::Range<usize>,
    theme: &Theme,
    unlike: &[Unlike],
) {
    let part = format!("{name}-rows-{}-{}", rows.start, rows.end);
    crate::testing::pictures::picture_dump::lines(&part, lines);
    crate::testing::pictures::picture_dump::lines_page(&part, lines, theme);
    let want: Vec<&str> = txt.split('\n').collect();
    let painted = picture::grid(html);
    rows_match(
        &part,
        lines,
        &want[rows.clone()],
        painted.get(rows).unwrap_or_default(),
        theme,
        unlike,
    );
}

fn rows_match(
    name: &str,
    lines: &[Line<'_>],
    want: &[&str],
    painted: &[Vec<picture::Cell>],
    theme: &Theme,
    unlike: &[Unlike],
) {
    for u in unlike {
        eprintln!(
            "{name}: row {} cols {}-{} are different: {}",
            u.row, u.cols.0, u.cols.1, u.why
        );
    }
    let drawn = cells(lines);
    let text = |row: &[(char, Style)]| -> String {
        let row: String = row.iter().map(|(ch, _)| ch).collect();
        row.trim_end().to_owned()
    };
    let shown: Vec<String> = drawn.iter().map(|row| text(row)).collect();
    let screen = shown.join("\n");
    assert_eq!(drawn.len(), want.len(), "{name}: rows\n{screen}");
    for (row, (wanted, cells)) in want.iter().zip(&drawn).enumerate() {
        let wanted: Vec<char> = wanted.chars().collect();
        let got: Vec<char> = text(cells).chars().collect();
        for col in 0..wanted.len().max(got.len()) {
            let (w, g) = (wanted.get(col), got.get(col));
            if w != g && !excused(unlike, row, col) {
                panic!(
                    "{name}: row {row} col {col}: the picture has {w:?}, the screen {g:?}\n\
                     want: {}\n got: {}\n\n{screen}",
                    want[row], shown[row]
                );
            }
        }
    }
    for (row, cells_painted) in painted.iter().enumerate() {
        for (col, cell) in cells_painted.iter().enumerate() {
            if cell.ch == ' ' || excused(unlike, row, col) {
                continue;
            }
            let Some((_, style)) = drawn.get(row).and_then(|r| r.get(col)) else {
                continue;
            };
            let at = format!("{name}: row {row} col {col} ({:?})", cell.ch);
            match cell.color.as_deref() {
                // A color spelled out is that color, or the text's own where it is the text's.
                Some(var) if let Some(want) = spelled_out(var, theme) => assert!(
                    want.is_none()
                        || style.fg == want
                        || (plain(want, theme) && plain(style.fg, theme)),
                    "{at}: {var}, the screen {:?}",
                    style.fg
                ),
                Some(var) => match roles_of(var) {
                    // The terminal's own foreground, as the text draws it, plain or bright.
                    [] if var == "var(--term-fg)" => assert!(
                        plain(style.fg, theme),
                        "{at}: {var}, the screen {:?}",
                        style.fg
                    ),
                    [] => assert!(style.fg.is_some(), "{at}: {var} is a color"),
                    roles => assert!(
                        roles.iter().any(|role| theme.style(*role).fg == style.fg),
                        "{at}: {var}, the screen {:?}",
                        style.fg
                    ),
                },
                None => assert!(
                    plain(style.fg, theme),
                    "{at}: the picture has the text's own color, the screen {:?}",
                    style.fg
                ),
            }
            assert_eq!(
                style.add_modifier.contains(Modifier::BOLD),
                cell.bold,
                "{at}: bold"
            );
            if let Some(var) = cell.background.as_deref() {
                match roles_of(var) {
                    [] => assert!(style.bg.is_some(), "{at}: {var} is a background"),
                    roles => assert!(
                        roles.iter().any(|role| theme.style(*role).bg == style.bg),
                        "{at}: {var}, the screen {:?}",
                        style.bg
                    ),
                }
            }
        }
    }
}

/// The text's own color: none set, or the roles that draw normal and bright text.
fn plain(fg: Option<Color>, theme: &Theme) -> bool {
    fg.is_none()
        || [Role::Text, Role::Bright]
            .iter()
            .any(|role| theme.style(*role).fg == fg)
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use ratatui::text::Span;

    use super::*;
    use crate::base::theme::Glyphs;

    const TXT: &str = include_str!("../../../tests/golden/waiting_rows.txt");
    const HTML: &str = include_str!("../../../tests/golden/waiting_rows.html");

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    /// The picture's own cells, each drawn in the first role its colour stands for.
    fn painted_lines(theme: &Theme) -> Vec<Line<'static>> {
        let colour = |value: Option<&str>, of: fn(Style) -> Option<Color>| {
            let value = value?;
            if let Some(spelled) = spelled_out(value, theme) {
                return spelled;
            }
            roles_of(value)
                .first()
                .and_then(|role| of(theme.style(*role)))
        };
        picture::grid(HTML)
            .into_iter()
            .take(rows().len())
            .map(|row| {
                let spans: Vec<Span<'static>> = row
                    .into_iter()
                    .map(|cell| {
                        let mut style = Style::default();
                        style.fg = colour(cell.color.as_deref(), |style| style.fg);
                        style.bg = colour(cell.background.as_deref(), |style| style.bg);
                        if cell.bold {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        Span::styled(cell.ch.to_string(), style)
                    })
                    .collect();
                Line::from(spans)
            })
            .collect()
    }

    /// The picture's rows without the spaces that pad them to its width, which a screen never
    /// draws.
    fn rows() -> Vec<String> {
        TXT.lines().map(|row| row.trim_end().to_owned()).collect()
    }

    fn fails(lines: &[Line<'_>], txt: &str, html: &str) -> bool {
        let theme = theme();
        catch_unwind(AssertUnwindSafe(|| {
            assert_matches("changed", lines, txt, html, &theme, &[]);
        }))
        .is_err()
    }

    #[test]
    fn one_changed_cell_fails_the_comparison() {
        let lines = painted_lines(&theme());
        let txt = rows().join("\n");
        assert!(!fails(&lines, &txt, HTML));
        let grid = picture::grid(HTML);
        let (row, column) = picture::find(&grid, |cell| {
            cell.ch.is_alphanumeric() && cell.color.as_deref() == Some("var(--term-dim)")
        });
        let changed = |edit: &dyn Fn(&mut picture::Cell)| {
            picture::edited(HTML, |grid| edit(&mut grid[row][column]))
        };
        let recoloured = changed(&|cell| cell.color = Some("var(--term-red)".to_owned()));
        assert!(fails(&lines, &txt, &recoloured), "a colour changed");
        assert!(
            fails(&lines, &txt, &changed(&|cell| cell.bold ^= true)),
            "a weight changed"
        );
        let filled = changed(&|cell| cell.background = Some("var(--term-band)".to_owned()));
        assert!(fails(&lines, &txt, &filled), "a fill changed");
        let mut rows = rows();
        let mut chars: Vec<char> = rows[row].chars().collect();
        chars[column] = '#';
        rows[row] = chars.into_iter().collect();
        assert!(fails(&lines, &rows.join("\n"), HTML), "a character changed");
    }
}
