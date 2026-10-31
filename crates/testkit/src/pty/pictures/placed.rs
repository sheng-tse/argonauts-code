//! Where a picture placed straight on the screen shows, as kitty and Ghostty place one: from the
//! cell the cursor is on, over `c` columns and `r` rows where the placement gives them, or over
//! the cells its pixels reach from its offset in that first cell, at the size the terminal's
//! cells are. One given only `c` or only `r` keeps its picture's shape. A placement below zero
//! sits under the text, so a cell holding a glyph shows the glyph; one below
//! [`UNDER_BACKGROUNDS`] sits under coloured cells too. The cursor stays where it was, whatever
//! `C` says.

use crate::screen::{Colour, Grid, Placeholder};

/// The z below which a picture sits under coloured cells as well as text.
const UNDER_BACKGROUNDS: i32 = -1_073_741_824;

/// A placement on the screen, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Placed {
    /// The first cell's row, which goes above the screen as rows scroll off its top.
    row: i32,
    col: u16,
    rows: u16,
    cols: u16,
    z: i32,
    /// Placed later sits over placed earlier at the same z.
    order: u64,
}

/// What a placement says of its size and depth, from its keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Shape {
    /// `c` and `r`: the cells it is scaled to fill, 0 where not given.
    pub(super) cells: (u32, u32),
    /// `X` and `Y`: pixels right and down from the first cell's top left.
    pub(super) offset: (u32, u32),
    /// `x` and `y`: the corner of the part of the picture that shows, or the first and last
    /// ids a deletion by range (`d=r`) takes.
    pub(super) from: (u32, u32),
    /// `w` and `h`: the size of that part, 0 for the rest of the picture.
    pub(super) size: (u32, u32),
    pub(super) z: i32,
}

impl Placed {
    /// Where `shape` places a picture `pixels` big, where they are known, from `cursor`, row
    /// then column, on a terminal whose cells are `cell` pixels.
    pub(super) fn new(
        shape: Shape,
        pixels: Option<(u32, u32)>,
        cursor: (u16, u16),
        cell: (u16, u16),
        order: u64,
    ) -> Self {
        let (cols, rows) = cells(shape, pixels, cell);
        Self {
            row: i32::from(cursor.0),
            col: cursor.1,
            rows,
            cols,
            z: shape.z,
            order,
        }
    }

    /// The text of the rows from the first to `bottom` went up `count` rows.
    pub(super) fn scroll(&mut self, bottom: u16, count: u16) {
        if self.row <= i32::from(bottom) {
            self.row -= i32::from(count);
        }
    }
}

/// The columns and rows a placement covers, at least one of each.
fn cells(shape: Shape, pixels: Option<(u32, u32)>, cell: (u16, u16)) -> (u16, u16) {
    let (cell_width, cell_height) = (u64::from(cell.0.max(1)), u64::from(cell.1.max(1)));
    // The part of the picture that shows, in its own pixels.
    let part = pixels.map(|(width, height)| {
        let side = |size: u32, whole: u32, from: u32| {
            if size > 0 {
                size
            } else {
                whole.saturating_sub(from)
            }
        };
        (
            u64::from(side(shape.size.0, width, shape.from.0)).max(1),
            u64::from(side(shape.size.1, height, shape.from.1)).max(1),
        )
    });
    let (cols, rows) = (u64::from(shape.cells.0), u64::from(shape.cells.1));
    let (cols, rows) = match (cols, rows, part) {
        (1.., 1.., _) => (cols, rows),
        (1.., 0, Some((width, height))) => (
            cols,
            (cols * cell_width * height).div_ceil(width * cell_height),
        ),
        (0, 1.., Some((width, height))) => (
            (rows * cell_height * width).div_ceil(height * cell_width),
            rows,
        ),
        (0, 0, Some((width, height))) => (
            (u64::from(shape.offset.0) + width).div_ceil(cell_width),
            (u64::from(shape.offset.1) + height).div_ceil(cell_height),
        ),
        // A picture whose size isn't known covers its first cell, as far as can be told.
        _ => (cols, rows),
    };
    let fit = |count: u64| u16::try_from(count.max(1)).unwrap_or(u16::MAX);
    (fit(cols), fit(rows))
}

/// Marks each cell of `grid` that a placement in `placed` shows over, with the picture's id: the
/// highest one whose picture isn't under the cell's glyph or colour.
pub(super) fn cover(grid: &mut Grid, placed: impl Iterator<Item = (u32, Placed)>) {
    let mut placed: Vec<(u32, Placed)> = placed.collect();
    placed.sort_by_key(|(_, placed)| (placed.z, placed.order));
    for (id, placed) in placed {
        let first_row = placed.row.max(0);
        let last_row = (placed.row + i32::from(placed.rows)).min(i32::from(grid.rows()));
        let last_col = placed.col.saturating_add(placed.cols).min(grid.cols());
        for row in first_row..last_row {
            let Ok(row) = u16::try_from(row) else {
                continue;
            };
            for col in placed.col..last_col {
                let cell = grid.cell_mut(row, col);
                let under_glyph = placed.z < 0 && !cell.is_blank();
                let under_colour = placed.z < UNDER_BACKGROUNDS && cell.background != Colour::Own;
                if !under_glyph && !under_colour {
                    cell.picture = Some(Placeholder { id, shows: true });
                }
            }
        }
    }
}

/// The width and height a PNG's header gives, from the first bytes of its file.
pub(super) fn png_size(head: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    let rest = head.strip_prefix(SIGNATURE)?;
    let header = rest.get(4..16)?;
    if &header[..4] != b"IHDR" {
        return None;
    }
    let number = |bytes: &[u8]| Some(u32::from_be_bytes(bytes.try_into().ok()?));
    Some((number(&header[4..8])?, number(&header[8..12])?))
}
