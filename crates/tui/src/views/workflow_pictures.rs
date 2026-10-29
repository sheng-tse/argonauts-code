//! The pictures the workflow view is drawn to, and what a test needs to put a drawing beside
//! one: every cell's character, its colour, and whether it is bold. A picture is two files in
//! `tests/golden`, the cells as text and the same cells with their colours as spans.

use std::ops::Range;

use argonauts_testkit::picture as page;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

use crate::theme::{Role, Theme};

#[derive(Debug, Clone, PartialEq)]
struct Cell {
    ch: char,
    /// `None` for the terminal's own foreground.
    colour: Option<Color>,
    bold: bool,
}

pub(crate) struct Picture {
    name: &'static str,
    rows: Vec<Vec<Cell>>,
}

/// The picture `$name`, read from its two files.
macro_rules! picture {
    ($name:literal) => {
        $crate::views::workflow_pictures::Picture::read(
            $name,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/golden/",
                $name,
                ".txt"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/golden/",
                $name,
                ".html"
            )),
        )
    };
}
pub(crate) use picture;

impl Picture {
    pub(crate) fn read(name: &'static str, text: &str, colours: &str) -> Self {
        let theme = dark();
        let rows = cells(colours, &theme);
        let drawn: Vec<String> = rows.iter().map(|row| line_text(row)).collect();
        let written: Vec<String> = text
            .trim_end()
            .lines()
            .map(|line| line.trim_end().to_owned())
            .collect();
        assert_eq!(drawn, written, "{name}: its two files disagree");
        Self { name, rows }
    }

    /// The same picture under `name`, for a screen with no picture of its own that a test
    /// draws from this one, changed: what it draws is then written apart from this one's.
    pub(crate) fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// The picture's rows as text, without the blanks at their ends.
    pub(crate) fn text(&self, rows: Range<usize>) -> Vec<String> {
        self.rows[rows].iter().map(|row| line_text(row)).collect()
    }

    /// Puts `to` where row `row` says `from`, each new cell looking as the first one it
    /// replaces. The row keeps its width: the widest run of blanks after it takes or gives the
    /// difference, so the columns between keep their places.
    pub(crate) fn reword(&mut self, row: usize, from: &str, to: &str) {
        let cells = &mut self.rows[row];
        let text: Vec<char> = cells.iter().map(|cell| cell.ch).collect();
        let from: Vec<char> = from.chars().collect();
        let at = text
            .windows(from.len())
            .position(|window| window == from.as_slice())
            .unwrap_or_else(|| panic!("{}: row {row} doesn't say {from:?}", self.name));
        let look = cells[at].clone();
        let new: Vec<Cell> = to.chars().map(|ch| Cell { ch, ..look.clone() }).collect();
        let grew = new.len() as isize - from.len() as isize;
        cells.splice(at..at + from.len(), new);
        let after = at + to.chars().count();
        let mut gap: Option<(usize, usize)> = None;
        let mut i = after;
        while i < cells.len() {
            let run = cells[i..].iter().take_while(|cell| cell.ch == ' ').count();
            if run >= 2 && gap.is_none_or(|(_, widest)| run > widest) {
                gap = Some((i, run));
            }
            i += run.max(1);
        }
        let Some((gap, _)) = gap else {
            return;
        };
        if grew > 0 {
            cells.drain(gap..gap + grew as usize);
        } else {
            let blank = Cell {
                ch: ' ',
                colour: None,
                bold: false,
            };
            let more = std::iter::repeat_n(blank, grew.unsigned_abs());
            cells.splice(gap..gap, more);
        }
    }

    /// Says a fallback the way its row reads now: `in place of GPT 5.6 Luna` where the picture
    /// says `for GPT 5.6 Luna`, and on a row that answered, `verdict` first, in its own colour.
    pub(crate) fn in_place_of(&mut self, row: usize, model: &str, verdict: Option<&str>) {
        let Some(verdict) = verdict else {
            self.reword(
                row,
                &format!("for {model}"),
                &format!("in place of {model}"),
            );
            return;
        };
        let from = format!("for {model} · {verdict}");
        let text: Vec<char> = self.rows[row].iter().map(|cell| cell.ch).collect();
        let wanted: Vec<char> = from.chars().collect();
        let at = text
            .windows(wanted.len())
            .position(|window| window == wanted.as_slice())
            .unwrap_or_else(|| panic!("{}: row {row} doesn't say {from:?}", self.name));
        let looks = |offset: usize| self.rows[row][at + offset].clone();
        let (cells, verdict_cells) = (wanted.len(), verdict.chars().count());
        let (amber, dim, said) = (looks(0), looks(cells - verdict_cells - 2), looks(cells - 1));
        self.reword(row, &from, &format!("{verdict} · in place of {model}"));
        for (i, cell) in self.rows[row][at..].iter_mut().enumerate() {
            let look = if i < verdict_cells {
                &said
            } else if i < verdict_cells + 3 {
                &dim
            } else if i < verdict_cells + 3 + "in place of ".len() + model.chars().count() {
                &amber
            } else {
                break;
            };
            cell.colour = look.colour;
            cell.bold = look.bold;
        }
    }

    /// Gives `label` on row `row` a column `width` cells wide, the label and the blanks after
    /// it, for a picture that lays every phase out by the widest label of the whole workflow
    /// where the view lays each phase out by its own. The row keeps its width as
    /// [`Self::reword`] keeps it.
    pub(crate) fn label_column(&mut self, row: usize, label: &str, width: usize) {
        let text: String = self.rows[row].iter().map(|cell| cell.ch).collect();
        let at = text
            .find(label)
            .unwrap_or_else(|| panic!("{}: row {row} doesn't say {label:?}", self.name));
        let blanks = text[at + label.len()..]
            .chars()
            .take_while(|ch| *ch == ' ')
            .count();
        let from = format!("{label}{}", " ".repeat(blanks));
        self.reword(row, &from, &format!("{label:<width$}"));
    }

    /// Puts a blank line before each of `titles`, the sections of an open agent's view, which
    /// Claude Code keeps apart where the picture drew them together. The view's lines from the
    /// first row from `from` on that holds each title move down one, in the view's pane only,
    /// and its box grows a row at the bottom, so the list beside it stays put. Returns how many
    /// went in.
    pub(crate) fn space_sections(&mut self, from: usize, titles: &[&str]) -> usize {
        let blank = Cell {
            ch: ' ',
            colour: None,
            bold: false,
        };
        let mut at = from;
        for title in titles {
            let wanted: Vec<char> = format!("│ {title}").chars().collect();
            let found = (at..self.rows.len()).find_map(|row| {
                let text: Vec<char> = self.rows[row].iter().map(|cell| cell.ch).collect();
                let column = text
                    .windows(wanted.len())
                    .position(|window| window == wanted.as_slice())?;
                let end = text.iter().rposition(|ch| *ch == '│')?;
                Some((row, column + 1, end))
            });
            let (row, start, end) = found
                .unwrap_or_else(|| panic!("{}: no row from {at} on says {title:?}", self.name));
            let bottom = (row..self.rows.len())
                .find(|row| self.rows[*row].iter().any(|cell| cell.ch == '╰'))
                .unwrap_or_else(|| panic!("{}: no box under row {row}", self.name));
            let mut grown = self.rows[bottom - 1].clone();
            for cell in grown.iter_mut().filter(|cell| cell.ch != '│') {
                *cell = blank.clone();
            }
            self.rows.insert(bottom, grown);
            for line in (row + 1..=bottom).rev() {
                let above: Vec<Cell> = self.rows[line - 1][start..end].to_vec();
                self.rows[line][start..end].clone_from_slice(&above);
            }
            for cell in &mut self.rows[row][start..end] {
                *cell = blank.clone();
            }
            at = row + 2;
        }
        titles.len()
    }

    /// Puts each box's frame in the colour the view draws it in, the text's own, where the
    /// picture drew it faint.
    pub(crate) fn bright_frames(&mut self) {
        let faint = dark().style(Role::Faint).fg;
        for row in &mut self.rows {
            if !row.iter().any(|cell| matches!(cell.ch, '╭' | '╰' | '│')) {
                continue;
            }
            for cell in row.iter_mut() {
                let frame = matches!(cell.ch, '╭' | '╮' | '╰' | '╯' | '│' | '┬' | '┴' | '─');
                if frame && cell.colour == faint {
                    cell.colour = None;
                }
            }
        }
    }

    /// Puts `blanks` blank cells after `text` on row `row`, the rest of the row moving right.
    pub(crate) fn widen(&mut self, row: usize, text: &str, blanks: usize) {
        let wanted: Vec<char> = text.chars().collect();
        let cells = &mut self.rows[row];
        let at = cells
            .windows(wanted.len())
            .position(|window| window.iter().map(|cell| cell.ch).eq(wanted.iter().copied()))
            .unwrap_or_else(|| panic!("{}: row {row} doesn't say {text:?}", self.name))
            + wanted.len();
        let blank = Cell {
            ch: ' ',
            colour: None,
            bold: false,
        };
        cells.splice(at..at, std::iter::repeat_n(blank, blanks));
    }

    /// Draws every `from` in the picture as `to`, in the same colour: a glyph the view draws as
    /// Claude Code does where the picture drew another.
    pub(crate) fn respell(&mut self, from: char, to: char) {
        for cell in self.rows.iter_mut().flatten() {
            if cell.ch == from {
                cell.ch = to;
            }
        }
    }

    /// Writes `text` over the picture's cells from `column` of `row`, in `colour`.
    pub(crate) fn write(&mut self, row: usize, column: usize, text: &str, colour: Option<Color>) {
        let cells = &mut self.rows[row];
        for (at, ch) in text.chars().enumerate() {
            cells[column + at] = Cell {
                ch,
                colour,
                bold: false,
            };
        }
    }

    /// Gives each of `names` in row `row` the colour the picture draws it in on another row,
    /// as a status line colours the models its rows name.
    pub(crate) fn colour_as_its_row(&mut self, row: usize, names: &[&str]) {
        for name in names {
            let name: Vec<char> = name.chars().collect();
            let at = |cells: &[Cell]| {
                cells
                    .windows(name.len())
                    .position(|window| window.iter().map(|cell| cell.ch).eq(name.iter().copied()))
            };
            let colour = (0..self.rows.len())
                .filter(|other| *other != row)
                .find_map(|other| at(&self.rows[other]).map(|x| self.rows[other][x].colour))
                .unwrap_or_else(|| panic!("{}: no other row names {name:?}", self.name));
            let x = at(&self.rows[row])
                .unwrap_or_else(|| panic!("{}: row {row} doesn't name {name:?}", self.name));
            for cell in &mut self.rows[row][x..x + name.len()] {
                cell.colour = colour;
            }
        }
    }

    /// Gives one cell of the picture another colour.
    pub(crate) fn recolour(&mut self, row: usize, column: usize, colour: Option<Color>) {
        self.rows[row][column].colour = colour;
    }

    /// Blanks one cell of the picture.
    pub(crate) fn blank(&mut self, row: usize, column: usize) {
        self.rows[row][column] = Cell {
            ch: ' ',
            colour: None,
            bold: false,
        };
    }

    /// Fails unless `buf` from its row `top` down draws the picture's `rows`: the same
    /// characters, and under each one the same colour and weight.
    pub(crate) fn assert_drawn(&self, rows: Range<usize>, buf: &Buffer, top: u16) {
        let theme = dark();
        let drawn: Vec<String> = (0..rows.len())
            .map(|row| buffer_row(buf, top + row as u16))
            .collect();
        // A picture checked a part at a time is written a part at a time, each named by its
        // rows, so one part never takes another's place.
        let part = format!("{}-rows-{}-{}", self.name, rows.start, rows.end);
        crate::picture_dump::rows(&part, &drawn);
        let width = usize::from(buf.area.width);
        let at = (0, usize::from(top));
        crate::picture_dump::buffer_page(&part, buf, at, (width, rows.len()), &theme);
        let wanted = self.text(rows.clone());
        assert_eq!(
            drawn,
            wanted,
            "{}\ndrawn:\n{}\npicture:\n{}",
            self.name,
            drawn.join("\n"),
            wanted.join("\n")
        );
        for (at, row) in rows.enumerate() {
            self.assert_looks(row, 0..self.rows[row].len(), buf, top + at as u16, &theme);
        }
    }

    /// Fails unless `buf`'s row `y` draws the picture's `row` over `columns`: the same
    /// characters, and under each one the same colour and weight. For a row the screen
    /// draws only the start of.
    pub(crate) fn assert_cells(&self, row: usize, columns: Range<usize>, buf: &Buffer, y: u16) {
        let drawn: String = columns
            .clone()
            .map(|x| buf[(x as u16, y)].symbol())
            .collect();
        let wanted: String = self.rows[row][columns.clone()]
            .iter()
            .map(|cell| cell.ch)
            .collect();
        assert_eq!(drawn, wanted, "{} row {row}", self.name);
        self.assert_looks(row, columns, buf, y, &dark());
    }

    /// The colour and weight under each character of `columns`.
    fn assert_looks(&self, row: usize, columns: Range<usize>, buf: &Buffer, y: u16, theme: &Theme) {
        let plain = [Role::Text, Role::Bright].map(|role| theme.style(role).fg);
        for x in columns {
            let want = &self.rows[row][x];
            if want.ch.is_whitespace() {
                continue;
            }
            let cell = &buf[(x as u16, y)];
            let fg: Option<Color> = Some(cell.fg);
            let colour = match fg {
                Some(Color::Reset) => None,
                fg if plain.contains(&fg) => None,
                fg => fg,
            };
            let got = Cell {
                ch: want.ch,
                colour,
                bold: cell.modifier.contains(Modifier::BOLD),
            };
            assert_eq!(&got, want, "{} row {row} column {x}", self.name);
        }
    }
}

/// The theme the pictures are drawn in.
pub(crate) fn dark() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = crate::theme::Glyphs::UNICODE;
    theme
}

/// `lines` drawn from the top of a buffer `width` cells wide.
pub(crate) fn buffer_of(lines: &[Line<'_>], width: u16) -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, lines.len() as u16));
    for (y, line) in lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, width);
    }
    buf
}

fn buffer_row(buf: &Buffer, y: u16) -> String {
    let area = buf.area;
    let mut line = String::new();
    for x in area.x..area.right() {
        line.push_str(buf[(x, y)].symbol());
    }
    line.trim_end().to_owned()
}

fn line_text(row: &[Cell]) -> String {
    let text: String = row.iter().map(|cell| cell.ch).collect();
    text.trim_end().to_owned()
}

/// The picture's cells, read off the spans of its coloured file.
fn cells(colours: &str, theme: &Theme) -> Vec<Vec<Cell>> {
    let mut rows: Vec<Vec<Cell>> = page::grid(colours)
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| Cell {
                    ch: cell.ch,
                    colour: cell
                        .color
                        .as_deref()
                        .and_then(|value| colour_of(value, theme)),
                    bold: cell.bold,
                })
                .collect()
        })
        .collect();
    while rows.last().is_some_and(Vec::is_empty) {
        rows.pop();
    }
    rows
}

/// A cell's colour: one of the page's named colours as the theme draws it, or the colour its
/// style spells out.
fn colour_of(value: &str, theme: &Theme) -> Option<Color> {
    let named = [
        ("dim", Role::Dim),
        ("faint", Role::Faint),
        ("grn", Role::Success),
        ("red", Role::Danger),
        ("amb", Role::Warn),
        ("acc", Role::Accent),
        ("org", Role::AgentOrange),
    ];
    if let Some(name) = value
        .strip_prefix("var(--term-")
        .and_then(|name| name.strip_suffix(')'))
    {
        if name == "fg" {
            return None;
        }
        let role = named
            .into_iter()
            .find(|(known, _)| *known == name)
            .map(|(_, role)| role)
            .unwrap_or_else(|| panic!("a colour the pictures don't name: {name}"));
        return theme.style(role).fg;
    }
    let hex = value.strip_prefix('#').expect("a colour");
    let rgb = u32::from_str_radix(hex, 16).expect("a colour's digits");
    let [_, r, g, b] = rgb.to_be_bytes();
    let colour = Color::Rgb(r, g, b);
    // The bright text colour, written as its value, draws as the terminal's own does.
    (theme.style(Role::Bright).fg != Some(colour)).then_some(colour)
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    const TEXT: &str = include_str!("../../tests/golden/captain_done.txt");
    const COLOURS: &str = include_str!("../../tests/golden/captain_done.html");

    /// The picture's own cells in a buffer, each in its colour and weight.
    fn painted(picture: &Picture) -> Buffer {
        let width = picture.rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut buf = Buffer::empty(Rect::new(0, 0, width as u16, picture.rows.len() as u16));
        for (y, row) in picture.rows.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let drawn = &mut buf[(x as u16, y as u16)];
                drawn.set_char(cell.ch);
                drawn.fg = cell.colour.unwrap_or(Color::Reset);
                if cell.bold {
                    drawn.modifier.insert(Modifier::BOLD);
                }
            }
        }
        buf
    }

    fn fails(text: &str, colours: &str, buf: &Buffer) -> bool {
        let picture = Picture::read("changed", text, colours);
        let rows = 0..picture.rows.len();
        catch_unwind(AssertUnwindSafe(|| picture.assert_drawn(rows, buf, 0))).is_err()
    }

    #[test]
    fn one_changed_cell_fails_the_comparison() {
        let buf = painted(&Picture::read("captain_done", TEXT, COLOURS));
        assert!(!fails(TEXT, COLOURS, &buf));
        let (row, column) = page::find(&page::grid(COLOURS), |cell| {
            cell.ch.is_alphanumeric() && cell.color.as_deref() == Some("var(--term-dim)")
        });
        let changed = |edit: &dyn Fn(&mut page::Cell)| {
            page::edited(COLOURS, |grid| edit(&mut grid[row][column]))
        };
        let recoloured = changed(&|cell| cell.color = Some("var(--term-red)".to_owned()));
        assert!(fails(TEXT, &recoloured, &buf), "a colour changed");
        assert!(
            fails(TEXT, &changed(&|cell| cell.bold ^= true), &buf),
            "a weight changed"
        );
        let mut rows: Vec<String> = TEXT.lines().map(str::to_owned).collect();
        let mut chars: Vec<char> = rows[row].chars().collect();
        chars[column] = '#';
        rows[row] = chars.into_iter().collect();
        let respelled = changed(&|cell| cell.ch = '#');
        assert!(
            fails(&rows.join("\n"), &respelled, &buf),
            "a character changed"
        );
    }
}
