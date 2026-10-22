//! Writes what a golden test drew for one of the pictures into the folder that
//! `ARGONAUTS_PICTURE_DUMP` names, so it can be put beside its picture: a `.txt` of its
//! characters, and an `.html` of the same cells in the pictures' own form. Nothing is written
//! when the variable isn't set.

use std::path::PathBuf;

use crate::picture::{self, Cell};

/// `text` as drawn for the picture `name`, its rows' trailing blanks left off. Text a terminal
/// is sent may hold its colour codes, which the `.html` draws and the `.txt` leaves out.
pub fn dump(name: &str, text: &str) {
    let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_DUMP").map(PathBuf::from) else {
        return;
    };
    let grid: Vec<Vec<Cell>> = text.lines().map(cells).collect();
    let rows: String = grid
        .iter()
        .map(|row| format!("{}\n", picture::text(row).trim_end()))
        .collect();
    let page = format!("<div class=\"scroll\">{}</div>\n", picture::page(&grid));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{name}.txt")), rows);
    let _ = std::fs::write(dir.join(format!("{name}.html")), page);
}

/// The cells of one line a terminal is sent, in the looks its codes set: dim, bold, and the
/// basic colours, named as the pictures name them.
fn cells(line: &str) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut look = Cell::plain(' ');
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let codes: String = chars.by_ref().take_while(|ch| *ch != 'm').collect();
            for code in codes.split(';') {
                set(&mut look, code);
            }
            continue;
        }
        cells.push(Cell { ch, ..look.clone() });
    }
    while cells.last().is_some_and(|cell| cell.ch == ' ') {
        cells.pop();
    }
    cells
}

fn set(look: &mut Cell, code: &str) {
    let colour = |name: &str| Some(format!("var(--term-{name})"));
    match code {
        "" | "0" => *look = Cell::plain(' '),
        "1" => look.bold = true,
        "2" => look.color = colour("dim"),
        "22" => {
            look.bold = false;
            look.color = None;
        }
        "31" => look.color = colour("red"),
        "32" => look.color = colour("grn"),
        "33" => look.color = colour("amb"),
        "39" => look.color = None,
        "90" => look.color = colour("faint"),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dim_line_is_drawn_dim_and_its_codes_left_out() {
        let drawn = cells("\x1b[2margonauts: on\x1b[0m done  ");
        assert_eq!(picture::text(&drawn), "argonauts: on done");
        assert_eq!(drawn[0].color.as_deref(), Some("var(--term-dim)"));
        assert_eq!(drawn[13].color, None);
    }
}
