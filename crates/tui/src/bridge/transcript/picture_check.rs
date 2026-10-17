//! Compares a drawn screen with its picture cell for cell: the characters with the `.txt`, and
//! the colors, boldness and backgrounds with the `.html` that draws the same grid. A picture
//! names its colors as the panel's own (`--term-dim` and so on), which map to theme roles.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::theme::{Role, Theme};

/// One cell of a picture: its character and how the picture styles it.
#[derive(Debug, Clone, PartialEq)]
pub struct Painted {
    pub ch: char,
    pub color: Option<String>,
    pub background: Option<String>,
    pub bold: bool,
}

/// A cell the screen is known to draw otherwise than its picture, and why.
#[derive(Debug, Clone, Copy)]
pub struct Unlike {
    pub row: usize,
    /// The first and last column, both included.
    pub cols: (usize, usize),
    pub why: &'static str,
}

/// The picture's rows, from its `.html`.
pub fn painted(html: &str) -> Vec<Vec<Painted>> {
    let body = html
        .split_once("<pre class=\"term grid\">")
        .map_or(html, |(_, rest)| rest);
    let body = body.rsplit_once("</pre>").map_or(body, |(grid, _)| grid);
    let mut rows = vec![Vec::new()];
    let mut rest = body;
    while !rest.is_empty() {
        let (style, text, after) = match rest.strip_prefix("<span style=\"") {
            Some(span) => {
                let (style, inner) = span.split_once("\">").expect("a span's style ends");
                let (text, after) = inner.split_once("</span>").expect("a span ends");
                (style, text, after)
            }
            None => {
                let end = rest.find("<span").unwrap_or(rest.len());
                ("", &rest[..end], &rest[end..])
            }
        };
        let mut color = None;
        let mut background = None;
        let mut bold = false;
        for part in style.split(';').filter(|part| !part.is_empty()) {
            match part.split_once(':') {
                Some(("color", value)) => color = Some(value.to_owned()),
                Some(("background", value)) => background = Some(value.to_owned()),
                Some(("font-weight", "700")) => bold = true,
                _ => {}
            }
        }
        for ch in unescape(text).chars() {
            if ch == '\n' {
                rows.push(Vec::new());
                continue;
            }
            rows.last_mut().expect("a row").push(Painted {
                ch,
                color: color.clone(),
                background: background.clone(),
                bold,
            });
        }
        rest = after;
    }
    rows
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
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
    crate::picture_dump::lines(name, lines);
    let want: Vec<&str> = txt.trim_end_matches('\n').split('\n').collect();
    rows_match(name, lines, &want, &painted(html), theme, unlike);
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
    crate::picture_dump::lines(&part, lines);
    let want: Vec<&str> = txt.split('\n').collect();
    let painted = painted(html);
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
    painted: &[Vec<Painted>],
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
                Some(var) => match roles_of(var) {
                    // The picture's foreground is the text's own colour or a colour of its own.
                    [] if var == "var(--term-fg)" => {
                        assert!(style.fg.is_some() || plain(style.fg, theme), "{at}: {var}")
                    }
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
