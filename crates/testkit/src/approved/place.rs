//! Where on its screen a drawing sits: where its `at` line says, or, where that says `?`, the
//! one place the screen shows the drawing's text.

use super::drawing::{Drawing, Paint, Region};
use super::drawn::Drawn;
use super::paint::{blank, solid_text};
use crate::screen::PLACEHOLDER;

/// Where a drawing sits on a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Where its `at` line puts it, or the one place the screen shows its text.
    At(Region),
    /// The screen shows its text nowhere; this is where the fewest cells' text is different.
    Nowhere(Region),
    /// The screen shows its text at more than one place.
    Many { first: Region, more: Vec<Region> },
}

impl Placement {
    /// Where the drawing is compared with the screen.
    pub fn region(&self) -> Region {
        match self {
            Placement::At(region) | Placement::Nowhere(region) => *region,
            Placement::Many { first, .. } => *first,
        }
    }
}

impl Drawing {
    /// Where the drawing sits on `drawn`, a screen of the size it is drawn for. Where its row
    /// or column is `?`, each place it fits is tried, and its text must match at exactly one:
    /// every cell's character but where a logo or a picture covers it, a painted blank matching
    /// anything painted whole in one colour, which the comparison then tells apart. A `?`
    /// cell's character counts, since a drawing typed by hand gives nothing else.
    pub fn place(&self, drawn: &Drawn) -> Placement {
        let span = |at: Option<u16>, room: u16, size: u16| match at {
            Some(at) => at..=at,
            None => 0..=room.saturating_sub(size),
        };
        let rows = span(self.at.row, drawn.rows(), self.height);
        let cols = span(self.at.col, drawn.cols(), self.width);
        let region_at = |top, left| Region {
            top,
            left,
            rows: self.height,
            cols: self.width,
        };
        if let (Some(top), Some(left)) = (self.at.row, self.at.col) {
            return Placement::At(region_at(top, left));
        }
        let mut found: Vec<Region> = Vec::new();
        let mut nearest = (usize::MAX, region_at(*rows.start(), *cols.start()));
        for top in rows {
            for left in cols.clone() {
                let region = region_at(top, left);
                // Once the text is found somewhere, only another exact match matters.
                let enough = if found.is_empty() { nearest.0 } else { 1 };
                let misses = self.misses(drawn, region, enough);
                if misses == 0 {
                    found.push(region);
                } else if misses < nearest.0 {
                    nearest = (misses, region);
                }
            }
        }
        let mut found = found.into_iter();
        match (found.next(), found.next()) {
            (Some(region), None) => Placement::At(region),
            (Some(first), Some(second)) => Placement::Many {
                first,
                more: std::iter::once(second).chain(found).collect(),
            },
            (None, _) => Placement::Nowhere(nearest.1),
        }
    }

    /// The cells at `region` whose text isn't the drawing's, counted up to `enough`.
    fn misses(&self, drawn: &Drawn, region: Region, enough: usize) -> usize {
        let mut misses = 0;
        for row in 0..self.height {
            for col in 0..self.width {
                let Some((wanted, paint)) = self.wanted(row, col) else {
                    continue;
                };
                let got = &drawn.cell(region.top + row, region.left + col).text;
                let shows = match paint {
                    Paint::Logo { .. } | Paint::Picture(_) => continue,
                    Paint::Character => same_character(wanted.text, got),
                    Paint::Blank | Paint::Fill(_) | Paint::Ink { .. } => {
                        same_text(wanted.text, got)
                    }
                };
                if !shows {
                    misses += 1;
                    if misses >= enough {
                        return misses;
                    }
                }
            }
        }
        misses
    }
}

/// Whether a screen's cell shows a painted cell's character: the same one, or both painted
/// whole in one colour.
fn same_text(want: char, got: &str) -> bool {
    let mut buffer = [0; 4];
    let want: &str = want.encode_utf8(&mut buffer);
    !got.starts_with(PLACEHOLDER) && (want == got || (solid_text(want) && solid_text(got)))
}

/// Whether a screen's cell shows a `?` cell's character, whatever its colours.
pub(crate) fn same_character(want: char, got: &str) -> bool {
    let mut buffer = [0; 4];
    let want: &str = want.encode_utf8(&mut buffer);
    !got.starts_with(PLACEHOLDER) && (want == got || (blank(want) && blank(got)))
}
