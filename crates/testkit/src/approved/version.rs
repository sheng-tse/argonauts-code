//! The app's version in its header. A drawing holds the version it was drawn at, and the screen
//! the version of the binary drawing it, which every crate of the workspace shares, the testkit
//! too. So a drawing reads its header's version as the binary's own.

use super::drawing::{BLANK, Wanted};

/// What the header writes before its version.
const BEFORE_VERSION: &str = "Argonauts Code v";

/// The version every crate of the workspace has, the binary's among them.
pub(crate) const OWN: &str = env!("CARGO_PKG_VERSION");

/// Writes `own` over each version after [`BEFORE_VERSION`] in a drawing's rows, its `lines` of
/// text and its `cells`, `width` to a row. The new version takes the paint of the old one's
/// first cell, and what follows on its row moves with it, as the app lays the header out. A
/// version longer than the blanks at its row's end leave room for stays as drawn, so the
/// comparison reports it.
pub(crate) fn as_own(lines: &mut [String], cells: &mut [Wanted], width: usize, own: &str) {
    let blank = Wanted {
        text: ' ',
        key: BLANK,
    };
    for (line, row) in lines.iter_mut().zip(cells.chunks_mut(width)) {
        let Some((start, drawn)) = version_in(row) else {
            continue;
        };
        let room = row.iter().rev().take_while(|cell| **cell == blank).count();
        if own.chars().count() > drawn + room {
            continue;
        }
        let key = row[start].key;
        let mut moved: Vec<Wanted> = row[..start].to_vec();
        moved.extend(own.chars().map(|text| Wanted { text, key }));
        moved.extend_from_slice(&row[start + drawn..]);
        moved.resize(width, blank);
        row.copy_from_slice(&moved);
        let text: String = row.iter().map(|cell| cell.text).collect();
        *line = text.trim_end().to_owned();
    }
}

/// Where a row's version starts after [`BEFORE_VERSION`], and how many cells it takes.
fn version_in(row: &[Wanted]) -> Option<(usize, usize)> {
    let text: Vec<char> = row.iter().map(|cell| cell.text).collect();
    let before: Vec<char> = BEFORE_VERSION.chars().collect();
    let start = text
        .windows(before.len())
        .position(|window| window == before.as_slice())?
        + before.len();
    let cells = text[start..]
        .iter()
        .take_while(|c| c.is_ascii_digit() || **c == '.')
        .count();
    (cells > 0).then_some((start, cells))
}
