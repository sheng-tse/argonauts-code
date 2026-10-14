//! Writes what a golden test drew for one of the pictures into the folder that
//! `ARGONAUTS_PICTURE_DUMP` names, a row a line, so it can be put beside its picture. A picture
//! checked in parts is written a part at a time, each named by its rows. Nothing is written when
//! the variable isn't set.

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::text::Line;

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
