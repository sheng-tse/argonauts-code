//! A drawn buffer as the test kit's grid, so the screen laws and the approved drawings read it
//! as they read a terminal.

use argonauts_testkit::screen::{Cell, Colour, Grid, PLACEHOLDER, Placeholder, Rgb};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// The buffer cell by cell.
pub(crate) fn grid(buf: &Buffer) -> Grid {
    on_screen(buf, (buf.area.width, buf.area.height), 0)
}

/// `buf` on a blank screen `cols` × `rows`, its first row at the screen's row `top`, cut at the
/// screen's edges. The cells a wide glyph covers are left empty, as a terminal leaves them. A
/// buffer holds no pictures: the app sends them beside it, so each placeholder counts as
/// showing its picture.
pub(crate) fn on_screen(buf: &Buffer, (cols, rows): (u16, u16), top: u16) -> Grid {
    let mut grid = Grid::blank(cols, rows);
    let area = buf.area;
    for y in 0..area.height.min(rows.saturating_sub(top)) {
        let mut covered = 0;
        for x in 0..area.width.min(cols) {
            let drawn = &buf[(area.x + x, area.y + y)];
            let ink = colour(drawn.fg);
            let background = colour(drawn.bg);
            let out = grid.cell_mut(top + y, x);
            if covered > 0 {
                covered -= 1;
                *out = Cell {
                    text: String::new(),
                    ink,
                    background,
                    ..Cell::default()
                };
                continue;
            }
            let text = drawn.symbol();
            covered = text.width().saturating_sub(1);
            let has = |modifier| drawn.modifier.contains(modifier);
            *out = Cell {
                text: text.to_owned(),
                ink,
                background,
                bold: has(Modifier::BOLD),
                italic: has(Modifier::ITALIC),
                underline: has(Modifier::UNDERLINED),
                dim: has(Modifier::DIM),
                inverse: has(Modifier::REVERSED),
                wide: covered > 0,
                picture: text.starts_with(PLACEHOLDER).then(|| Placeholder {
                    id: ink.picture_id(),
                    shows: true,
                }),
            };
        }
    }
    grid
}

fn colour(colour: Color) -> Colour {
    match colour {
        Color::Reset => Colour::Own,
        Color::Black => Colour::Indexed(0),
        Color::Red => Colour::Indexed(1),
        Color::Green => Colour::Indexed(2),
        Color::Yellow => Colour::Indexed(3),
        Color::Blue => Colour::Indexed(4),
        Color::Magenta => Colour::Indexed(5),
        Color::Cyan => Colour::Indexed(6),
        Color::Gray => Colour::Indexed(7),
        Color::DarkGray => Colour::Indexed(8),
        Color::LightRed => Colour::Indexed(9),
        Color::LightGreen => Colour::Indexed(10),
        Color::LightYellow => Colour::Indexed(11),
        Color::LightBlue => Colour::Indexed(12),
        Color::LightMagenta => Colour::Indexed(13),
        Color::LightCyan => Colour::Indexed(14),
        Color::White => Colour::Indexed(15),
        Color::Indexed(index) => Colour::Indexed(index),
        Color::Rgb(red, green, blue) => Colour::Rgb(Rgb(red, green, blue)),
    }
}
