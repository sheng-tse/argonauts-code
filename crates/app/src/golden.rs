//! Screens the app builds, drawn from their pictures' own examples at the pictures' width and
//! compared with them cell for cell, colours included. Each picture in `tests/golden/` holds
//! only the rows drawn here: its `.txt` the characters, its `.html` the colours.

use std::time::Instant;

use argonauts_tui::render::{Clock, RenderCtx};
use argonauts_tui::theme::{ColorLevel, Glyphs, Role, Theme};
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

/// Writes `lines` as text into the folder `ARGONAUTS_PICTURE_DUMP` names, as `<name>.txt`, so a
/// drawing can be put beside its picture. Nothing is written when the variable isn't set.
pub(crate) fn dump(name: &str, lines: &[Line<'_>]) {
    let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_DUMP") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let text = drawn_rows(lines).join("\n") + "\n";
    if let Err(err) = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(dir.join(format!("{name}.txt")), text))
    {
        eprintln!("couldn't write the drawing of {name}: {err}");
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
    let body = html
        .split_once("<pre")
        .and_then(|(_, rest)| rest.split_once('>'))
        .and_then(|(_, rest)| rest.split_once("</pre>"))
        .map(|(body, _)| body)
        .expect("the picture's cells");
    let plain = Look {
        colour: None,
        bold: false,
    };
    let mut lines = vec![Vec::new()];
    let mut rest = body;
    while !rest.is_empty() {
        let (look, text, after) = match rest.strip_prefix("<span style=\"") {
            Some(span) => {
                let (style, after) = span.split_once("\">").expect("a span's style");
                let (text, after) = after.split_once("</span>").expect("a span's end");
                (look_of(style), text, after)
            }
            None => {
                let end = rest.find("<span").unwrap_or(rest.len());
                (plain.clone(), &rest[..end], &rest[end..])
            }
        };
        let text = text
            .replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("&quot;", "\"")
            .replace("&#x27;", "'")
            .replace("&amp;", "&");
        for c in text.chars() {
            if c == '\n' {
                lines.push(Vec::new());
            } else {
                lines.last_mut().unwrap().push((c, look.clone()));
            }
        }
        rest = after;
    }
    // A picture ends its last row with a newline, or with spaces up to its width.
    while lines
        .last()
        .is_some_and(|line| line.iter().all(|(c, _)| c.is_whitespace()))
    {
        lines.pop();
    }
    lines
}

fn look_of(style: &str) -> Look {
    let colour = if style.contains("#b1b9f9") {
        Some("acc")
    } else {
        ROLES
            .into_iter()
            .map(|(name, _)| name)
            .find(|name| style.contains(&format!("color:var(--term-{name})")))
    };
    Look {
        colour,
        bold: style.contains("font-weight:700"),
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
