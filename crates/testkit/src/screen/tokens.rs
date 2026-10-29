//! A row read as Ghostty with Iosevka paints it: runs of glyphs between blanks, each a word, a
//! figure or a mark, and the blanks between them.

use super::colour::Colour;
use super::grid::{Area, Grid};

/// Glyphs Iosevka draws two cells wide although they take one, so the blank after each is
/// painted over (`wide_glyphs` keeps screens from crowding them).
const PAINTED_WIDE: [&str; 6] = ["■", "→", "←", "↑", "↓", "…"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Holds a letter: a word, a name, a version beside its name such as `5.5`.
    Word,
    /// A figure as a table writes one: `12`, `$5.35`, `~$1.55`, `68%`, `<1%`, `1.97M`, `3h`.
    Figure,
    /// Glyphs alone: a swatch, a dot, a bar, a logo, a pointer, a frame.
    Mark,
    /// One word set on a background of its own with a blank at each end, as ` new ` is.
    Badge,
}

/// One run of glyphs on a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub row: u16,
    /// The first cell it takes.
    pub start: u16,
    /// One past the last cell it paints, the blank a wide-painted glyph covers included.
    pub end: u16,
    pub text: String,
    pub kind: Kind,
}

impl Token {
    /// The blanks between this token and `next` on the same row.
    pub fn gap_to(&self, next: &Token) -> u16 {
        next.start.saturating_sub(self.end)
    }
}

/// The row's tokens inside `area`, left to right.
pub(crate) fn tokens(grid: &Grid, area: Area, row: u16) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut col = area.x;
    while col < area.right() {
        if let Some(badge) = badge_at(grid, area, row, col) {
            col = badge.end;
            tokens.push(badge);
            continue;
        }
        if grid.cell(row, col).is_blank() {
            col += 1;
            continue;
        }
        let start = col;
        let mut text = String::new();
        while col < area.right() && !grid.cell(row, col).is_blank() {
            text.push_str(&grid.cell(row, col).text);
            col += 1;
        }
        let mut end = col;
        let painted_over = PAINTED_WIDE.iter().any(|wide| text.ends_with(wide));
        if painted_over && end < area.right() {
            end += 1;
        }
        let kind = kind(&text);
        // "2h 20m" is one figure, written in two parts.
        if let Some(last) = tokens.last_mut()
            && kind == Kind::Figure
            && last.kind == Kind::Figure
            && start == last.end + 1
            && is_duration(&last.text)
            && is_duration(&text)
        {
            last.text = format!("{} {text}", last.text);
            last.end = end;
            continue;
        }
        tokens.push(Token {
            row,
            start,
            end,
            text,
            kind,
        });
    }
    tokens
}

/// The badge starting at `col`: a run of cells on one background that isn't the terminal's
/// own, a blank at each end and one word between.
fn badge_at(grid: &Grid, area: Area, row: u16, col: u16) -> Option<Token> {
    let background = grid.cell(row, col).background;
    if background == Colour::Own || !grid.cell(row, col).is_blank() {
        return None;
    }
    if col > area.x && grid.cell(row, col - 1).background == background {
        return None;
    }
    let mut end = col;
    while end < area.right() && grid.cell(row, end).background == background {
        end += 1;
    }
    let text = grid.text(row, col, end);
    let word = text.trim();
    let padded = text.starts_with(' ') && text.ends_with(' ');
    let one_word = !word.is_empty() && !word.contains(' ');
    (padded && one_word).then(|| Token {
        row,
        start: col,
        end,
        text: word.to_owned(),
        kind: Kind::Badge,
    })
}

fn kind(text: &str) -> Kind {
    // A list's number, as in "1.  Council", keeps to the words after it as a bullet does.
    let ordinal = text
        .strip_suffix('.')
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()));
    if ordinal {
        Kind::Mark
    } else if is_figure(text) {
        Kind::Figure
    } else if text.chars().any(char::is_alphanumeric) {
        Kind::Word
    } else {
        Kind::Mark
    }
}

/// Whether `text` is a figure a column of numbers holds. A bare `5.5` is a version, part of a
/// name, so a figure with a point and nothing around it isn't one.
pub(crate) fn is_figure(text: &str) -> bool {
    let text = text.strip_prefix('(').unwrap_or(text);
    let text = text.strip_suffix(')').unwrap_or(text);
    let body = text.trim_start_matches(['~', '<', '>', '≈', '+', '−', '-']);
    let body = body.trim_start_matches(['$', '€', '£']);
    let digits = body
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(body.len());
    let (number, unit) = body.split_at(digits);
    if !number.starts_with(|c: char| c.is_ascii_digit()) || number.ends_with(['.', ',']) {
        return false;
    }
    const UNITS: [&str; 15] = [
        "", "%", "k", "K", "M", "b", "B", "G", "T", "x", "×", "s", "m", "h", "d",
    ];
    if !UNITS.contains(&unit) && unit != "ms" && unit != "w" {
        return false;
    }
    let bare_version = unit.is_empty() && number.contains('.') && body.len() == text.len();
    !bare_version
}

/// A figure counting time in days, hours, minutes or seconds.
fn is_duration(text: &str) -> bool {
    is_figure(text) && text.ends_with(['w', 'd', 'h', 'm', 's'])
}
