//! A drawn screen as cells, the same whichever way it was drawn: a ratatui buffer turned cell by
//! cell, the screen a terminal shows, or plain rows of text. vt100's cells are read here alone.

use super::colour::Colour;

/// The kitty protocol's placeholder: a cell holding it shows a piece of the picture its text
/// colour names, at the row and column the diacritics after it number.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// One cell of a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// One glyph and its combining marks, a blank as `" "`. Empty on the right half of a wide
    /// glyph, which the cell before it covers.
    pub text: String,
    pub ink: Colour,
    pub background: Colour,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    /// Drawn with its colours swapped, as a selected row often is.
    pub inverse: bool,
    /// The left half of a wide glyph.
    pub wide: bool,
    /// On a kitty placeholder cell, the picture it shows part of.
    pub picture: Option<Placeholder>,
}

/// The picture a placeholder cell names, and whether its screen holds it, so the cell shows it.
/// A terminal keeps one store of pictures for each screen: a picture sent while the other
/// screen showed leaves its placeholders blank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder {
    pub id: u32,
    pub shows: bool,
}

impl Default for Cell {
    /// A blank in the terminal's own colours.
    fn default() -> Cell {
        Cell::plain(" ")
    }
}

impl Cell {
    /// A cell showing `text` in the terminal's own colours.
    pub fn plain(text: &str) -> Cell {
        Cell {
            text: text.to_owned(),
            ink: Colour::Own,
            background: Colour::Own,
            bold: false,
            italic: false,
            underline: false,
            dim: false,
            inverse: false,
            wide: false,
            picture: None,
        }
    }

    /// Whether the cell holds no glyph, whatever fills it. A covered cell isn't blank: the
    /// glyph to its left is drawn over it.
    pub fn is_blank(&self) -> bool {
        !self.text.is_empty() && self.text.chars().all(|c| c == ' ')
    }

    /// Whether the glyph to its left covers the cell.
    pub fn is_covered(&self) -> bool {
        self.text.is_empty()
    }
}

/// A screen `cols` wide and `rows` tall, cell by cell, rows from the top. Rows and columns
/// count from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    cols: u16,
    rows: u16,
    alternate: bool,
    cells: Vec<Cell>,
}

/// A rectangle of a grid, in cells, as ratatui's `Rect` has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Area {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(self) -> u16 {
        self.x + self.width
    }

    pub fn bottom(self) -> u16 {
        self.y + self.height
    }
}

impl Grid {
    /// A screen of blanks.
    pub fn blank(cols: u16, rows: u16) -> Grid {
        Grid::from_cells(
            cols,
            rows,
            vec![Cell::default(); usize::from(cols) * usize::from(rows)],
        )
    }

    /// From each cell left to right, row after row, a covered cell given as an empty text.
    pub fn from_cells(cols: u16, rows: u16, cells: impl IntoIterator<Item = Cell>) -> Grid {
        let cells: Vec<Cell> = cells.into_iter().collect();
        let len = usize::from(cols) * usize::from(rows);
        assert_eq!(cells.len(), len, "a {cols} × {rows} grid takes {len} cells");
        Grid {
            cols,
            rows,
            alternate: false,
            cells,
        }
    }

    /// From rows of text in the terminal's own colours, laid out by vt100, so a wide glyph
    /// takes the two cells a terminal gives it and a row longer than `cols` is cut.
    pub fn from_text<S: AsRef<str>>(cols: u16, rows: &[S]) -> Grid {
        let height = u16::try_from(rows.len()).expect("a screen's rows fit a u16");
        let mut cells = Vec::with_capacity(usize::from(cols) * rows.len());
        for row in rows {
            let text: String = row.as_ref().chars().filter(|c| !c.is_control()).collect();
            // Tall enough that what runs past the edge wraps below the first row, never
            // scrolling it away.
            let tall = u16::try_from(text.len() / usize::from(cols.max(1)) + 2).unwrap_or(u16::MAX);
            let mut parser = vt100::Parser::new(tall, cols, 0);
            parser.process(text.as_bytes());
            cells.extend(row_cells(parser.screen(), 0, &|_| false));
        }
        Grid::from_cells(cols, height, cells)
    }

    /// The screen a terminal `cols` wide and `rows` tall shows once it has read `bytes`. It
    /// takes pictures as Ghostty does, one store for each screen.
    pub fn from_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Grid {
        crate::pty::read_bytes(cols, rows, bytes)
    }

    /// `screen`'s cells, a placeholder for picture `id` showing it when `shows(id)` says so.
    pub(crate) fn read(screen: &vt100::Screen, shows: &dyn Fn(u32) -> bool) -> Grid {
        let (rows, cols) = screen.size();
        let cells = (0..rows).flat_map(|row| row_cells(screen, row, shows));
        Grid {
            alternate: screen.alternate_screen(),
            ..Grid::from_cells(cols, rows, cells)
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Whether these are the cells of a terminal's alternate screen.
    pub fn alternate(&self) -> bool {
        self.alternate
    }

    /// The whole grid as an area.
    pub fn area(&self) -> Area {
        Area::new(0, 0, self.cols, self.rows)
    }

    /// Panics off the grid, as an index does.
    pub fn cell(&self, row: u16, col: u16) -> &Cell {
        &self.cells[self.at(row, col)]
    }

    pub fn cell_mut(&mut self, row: u16, col: u16) -> &mut Cell {
        let at = self.at(row, col);
        &mut self.cells[at]
    }

    /// The cell at `row` and `col`, or `None` off the grid.
    pub fn get(&self, row: u16, col: u16) -> Option<&Cell> {
        (row < self.rows && col < self.cols).then(|| self.cell(row, col))
    }

    /// The rows from the top, each its cells from the left.
    pub fn cell_rows(&self) -> impl Iterator<Item = &[Cell]> {
        self.cells.chunks(usize::from(self.cols.max(1)))
    }

    /// The cells of `row`, from the left.
    pub fn row(&self, row: u16) -> &[Cell] {
        let start = self.at(row, 0);
        &self.cells[start..start + usize::from(self.cols)]
    }

    fn at(&self, row: u16, col: u16) -> usize {
        assert!(
            row < self.rows && col < self.cols,
            "row {row} col {col} is off a {} × {} screen",
            self.cols,
            self.rows
        );
        usize::from(row) * usize::from(self.cols) + usize::from(col)
    }

    /// The row's text from `from` to `to`, a covered cell adding nothing.
    pub fn text(&self, row: u16, from: u16, to: u16) -> String {
        (from..to.min(self.cols))
            .map(|col| self.cell(row, col).text.as_str())
            .collect()
    }

    /// The row's text inside `area`, its trailing blanks dropped.
    pub fn row_text(&self, area: Area, row: u16) -> String {
        self.text(row, area.x, area.right()).trim_end().to_owned()
    }

    /// Every row of `area` as text, its trailing blanks dropped.
    pub fn lines(&self, area: Area) -> Vec<String> {
        (area.y..area.bottom())
            .map(|row| self.row_text(area, row))
            .collect()
    }

    /// Where `text` starts on the row, by column, each time it shows inside `area`.
    pub fn find_in_row(&self, area: Area, row: u16, text: &str) -> Vec<u16> {
        let mut starts = Vec::new();
        let mut line = String::new();
        let mut at_byte = Vec::new();
        for col in area.x..area.right() {
            at_byte.push((line.len(), col));
            line.push_str(&self.cell(row, col).text);
        }
        for (byte, _) in line.match_indices(text) {
            if let Some(&(_, col)) = at_byte.iter().find(|(start, _)| *start == byte) {
                starts.push(col);
            }
        }
        starts
    }

    /// `area` as text, a row to a line, for a failure to show.
    pub fn as_text(&self, area: Area) -> String {
        self.lines(area).join("\n")
    }
}

/// One row of a vt100 screen as cells.
fn row_cells<'a>(
    screen: &'a vt100::Screen,
    row: u16,
    shows: &'a dyn Fn(u32) -> bool,
) -> impl Iterator<Item = Cell> + 'a {
    (0..screen.size().1).map(move |col| match screen.cell(row, col) {
        Some(cell) => read_cell(cell, shows),
        None => Cell::default(),
    })
}

fn read_cell(cell: &vt100::Cell, shows: &dyn Fn(u32) -> bool) -> Cell {
    let text = if cell.is_wide_continuation() {
        ""
    } else if cell.has_contents() {
        cell.contents()
    } else {
        " "
    };
    let ink = Colour::from(cell.fgcolor());
    let picture = text.starts_with(PLACEHOLDER).then(|| {
        let id = ink.picture_id();
        Placeholder {
            id,
            shows: shows(id),
        }
    });
    Cell {
        text: text.to_owned(),
        ink,
        background: cell.bgcolor().into(),
        bold: cell.bold(),
        italic: cell.italic(),
        underline: cell.underline(),
        dim: cell.dim(),
        inverse: cell.inverse(),
        wide: cell.is_wide(),
        picture,
    }
}

/// The cells `text` takes on a terminal, as vt100 lays it out.
pub(crate) fn text_width(text: &str) -> usize {
    let cols = u16::try_from(text.len() * 2 + 1).unwrap_or(u16::MAX);
    let mut parser = vt100::Parser::new(1, cols, 0);
    parser.process(text.as_bytes());
    usize::from(parser.screen().cursor_position().1)
}
