//! The block version of each mascot, for terminals that draw no pictures: 3 rows of 9 cells of
//! quadrant characters, each with a foreground and a background colour, as `cells.json` holds
//! them. `assets/mascots/cells.py` writes that file from each mascot's grid of quadrants.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use ratatui::text::Span;
use serde::Deserialize;

use super::{Look, Mascot, ROWS, WIDTH};
use crate::base::theme::{ColorLevel, Theme, nearest_ansi16, nearest_ansi256};

const CELLS: &str = include_str!("../../../assets/mascots/cells.json");

/// Body quadrants take the light as it sweeps; eyes, dark parts and gold don't.
const BODY: &str = "A";
/// Columns the light covers at once.
const BAND: i32 = 2;

#[derive(Debug, Deserialize)]
pub(super) struct CellsFile {
    /// Each look's colour for each letter, as `#rrggbb`.
    pub palettes: BTreeMap<String, BTreeMap<String, String>>,
    /// The light that sweeps across the body in each ultra look.
    pub shimmer: BTreeMap<String, String>,
    pub mascots: BTreeMap<String, MascotCells>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MascotCells {
    pub story: String,
    /// The quadrants the cells are drawn from, which the tests check them against.
    #[cfg(test)]
    pub grid: Vec<String>,
    pub cells: Vec<Vec<Cell>>,
    pub ultra_cells: Vec<Vec<Cell>>,
}

/// A character, then the letters of its foreground and background colours; no background is
/// the terminal's own.
pub(super) type Cell = (String, Option<String>, Option<String>);

pub(super) fn file() -> &'static CellsFile {
    static FILE: OnceLock<CellsFile> = OnceLock::new();
    FILE.get_or_init(|| serde_json::from_str(CELLS).expect("cells.json is the generator's own"))
}

/// The mascot's three rows in `look`, each [`WIDTH`] cells. `frame` counts the ultra looks'
/// frames, one every 50 ms, through which the light sweeps across the body from the left; the
/// first and the last ones have none.
pub fn rows(mascot: Mascot, look: Look, frame: usize, theme: &Theme) -> [Vec<Span<'static>>; ROWS] {
    let file = file();
    let cells = &file.mascots[mascot.name()];
    let grid = if look.is_ultra() {
        &cells.ultra_cells
    } else {
        &cells.cells
    };
    let palette = &file.palettes[look.name()];
    let shimmer = file.shimmer.get(look.name());
    // The light's right edge, which enters from the left of the first column.
    let frames = mascot.frames().max(1);
    let front = i32::try_from(frame % frames * 12 / frames).unwrap_or(i32::MAX) - 1;
    let lit = |column: usize| {
        let column = i32::try_from(column).unwrap_or(i32::MAX);
        (front - BAND < column) && (column <= front)
    };
    let colour = |letter: &Option<String>, column: usize| -> Option<Color> {
        let letter = letter.as_deref()?;
        let hex = match shimmer {
            Some(light) if letter == BODY && lit(column) => light,
            _ => palette.get(letter)?,
        };
        paint(theme, hex)
    };
    std::array::from_fn(|row| {
        grid[row]
            .iter()
            .take(WIDTH)
            .enumerate()
            .map(|(column, (text, fg, bg))| {
                let mut style = Style::new();
                if let Some(fg) = colour(fg, column) {
                    style = style.fg(fg);
                }
                if let Some(bg) = colour(bg, column) {
                    style = style.bg(bg);
                }
                Span::styled(text.clone(), style)
            })
            .collect()
    })
}

/// `#rrggbb` as this terminal can show it; nothing without colours, where the shapes still
/// read.
fn paint(theme: &Theme, hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#')?;
    let channel = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    let rgb = (channel(0)?, channel(2)?, channel(4)?);
    match theme.level {
        ColorLevel::TrueColor => Some(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        ColorLevel::Ansi256 => Some(Color::Indexed(nearest_ansi256(rgb))),
        ColorLevel::Ansi16 => Some(nearest_ansi16(rgb)),
        ColorLevel::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generator's rule for turning a grid into cells, so a hand edit of `cells.json` that
    /// the grid doesn't draw is caught.
    fn cells_of(grid: &[String]) -> Vec<Vec<Cell>> {
        const AMBIGUOUS: [char; 4] = ['▀', '▄', '█', '▌'];
        let quadrants = |mask: u8| -> char {
            [
                ' ', '▗', '▖', '▄', '▝', '▐', '▞', '▟', '▘', '▚', '▌', '▙', '▀', '▜', '▛', '█',
            ][usize::from(mask)]
        };
        let rows: Vec<Vec<char>> = grid.iter().map(|row| row.chars().collect()).collect();
        (0..3)
            .map(|cy| {
                (0..9)
                    .map(|cx| {
                        let quad = [
                            rows[2 * cy][2 * cx],
                            rows[2 * cy][2 * cx + 1],
                            rows[2 * cy + 1][2 * cx],
                            rows[2 * cy + 1][2 * cx + 1],
                        ];
                        let mask = |letter: char| -> u8 {
                            quad.iter()
                                .zip([8u8, 4, 2, 1])
                                .filter(|(q, _)| **q == letter)
                                .map(|(_, bit)| bit)
                                .sum()
                        };
                        let mut letters: Vec<char> = quad.to_vec();
                        letters.sort_unstable();
                        letters.dedup();
                        let name = |c: char| Some(c.to_string());
                        match letters.as_slice() {
                            ['.'] => (" ".to_owned(), None, None),
                            ['.', fg] => (quadrants(mask(*fg)).to_string(), name(*fg), None),
                            [one] => ("█".to_owned(), name(*one), None),
                            [a, b] => {
                                let mut options = [(*a, *b), (*b, *a)];
                                options.sort_by_key(|(fg, _)| {
                                    let glyph = quadrants(mask(*fg));
                                    let count = quad.iter().filter(|q| **q == *fg).count();
                                    (AMBIGUOUS.contains(&glyph), count)
                                });
                                let (fg, bg) = options[0];
                                (quadrants(mask(fg)).to_string(), name(fg), name(bg))
                            }
                            mixed => panic!("cell {cx},{cy} mixes {mixed:?}"),
                        }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_cells_are_the_ones_each_grid_draws() {
        for (name, mascot) in &file().mascots {
            assert_eq!(cells_of(&mascot.grid), mascot.cells, "{name}");
            assert_eq!(mascot.cells.len(), ROWS, "{name}");
            assert!(mascot.cells.iter().all(|row| row.len() == WIDTH), "{name}");
            // The ultra cells add only the gold mark.
            let gold = |cells: &Vec<Vec<Cell>>| {
                cells
                    .iter()
                    .flatten()
                    .filter(|(_, fg, bg)| [fg, bg].iter().any(|c| c.as_deref() == Some("G")))
                    .count()
            };
            assert_eq!(gold(&mascot.cells), 0, "{name}");
            assert!(gold(&mascot.ultra_cells) > 0, "{name}");
        }
    }

    #[test]
    fn every_look_colours_every_letter() {
        let file = file();
        for look in Look::ALL {
            let palette = &file.palettes[look.name()];
            for letter in ["A", "D", "K", "G"] {
                assert!(palette.contains_key(letter), "{look:?} {letter}");
            }
            assert_eq!(file.shimmer.contains_key(look.name()), look.is_ultra());
        }
    }

    #[test]
    fn the_light_crosses_the_body_once_a_cycle() {
        let theme = Theme::dark();
        let lit_columns = |frame: usize| -> Vec<usize> {
            let rows = rows(Mascot::Medusa, Look::Ultracode, frame, &theme);
            let lit = Some(Color::Rgb(0xd7, 0xc2, 0xff));
            (0..WIDTH)
                .filter(|column| rows.iter().any(|row| row[*column].style.fg == lit))
                .collect()
        };
        // The medusa's 36 frames a cycle.
        assert_eq!(lit_columns(0), Vec::<usize>::new());
        assert_eq!(lit_columns(35), Vec::<usize>::new());
        assert_eq!(lit_columns(6), [1]);
        assert_eq!(lit_columns(18), [4, 5]);
        assert_eq!(lit_columns(36), lit_columns(0), "the next cycle");
        let swept: std::collections::BTreeSet<usize> = (0..36).flat_map(lit_columns).collect();
        assert_eq!(
            swept.into_iter().collect::<Vec<_>>(),
            (1..8).collect::<Vec<_>>()
        );
        // Without colours the shapes stay and nothing is painted.
        let plain = Theme::dark().with_level(ColorLevel::None);
        let rows = rows(Mascot::Argonaut, Look::Normal, 0, &plain);
        assert!(rows.iter().flatten().all(|span| span.style == Style::new()));
    }
}
