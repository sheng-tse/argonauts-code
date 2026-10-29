//! The strict comparison of a screen with its drawing: every cell of the drawing, where it
//! sits on the screen, its text and every colour as the terminal paints it, and a report a
//! person can read.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};

use super::drawing::{Drawing, Paint, Wanted};
use super::drawn::Drawn;
use super::paint::{Painted, Palette, blank, light_ink, logo_half, logo_of, picture_words, solid};
use super::place::{Placement, same_character};
use crate::screen::Rgb;

/// A cell of the screen that isn't as its drawing has it, at the drawing's own row and column,
/// counted from its first cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub row: u16,
    pub col: u16,
    pub aspect: Aspect,
    /// What the drawing has, in words.
    pub drawing: String,
    /// What the screen has, in words.
    pub screen: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aspect {
    /// Where the drawing sits: its text found nowhere on the screen, or at more than one place.
    Place,
    Text,
    TextColour,
    Background,
    Bold,
    Italic,
    Picture,
}

impl fmt::Display for Aspect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Aspect::Place => "place",
            Aspect::Text => "text",
            Aspect::TextColour => "text colour",
            Aspect::Background => "background",
            Aspect::Bold => "bold",
            Aspect::Italic => "italic",
            Aspect::Picture => "picture",
        })
    }
}

/// Every cell of the drawing the screen draws otherwise, where it sits on the screen
/// ([`Drawing::place`]), but where `excused`, as `palette` paints both on the drawing's
/// terminal. Panics unless the screen is the size the drawing is drawn for, which
/// [`super::check`] tells apart first.
pub fn differences(
    drawing: &Drawing,
    drawn: &Drawn,
    palette: &Palette,
    excused: &dyn Fn(u16, u16) -> bool,
) -> Vec<Difference> {
    differences_at(drawing, &drawing.place(drawn), drawn, palette, excused)
}

/// [`differences`] with the drawing at `placement`. Its text found at more than one place is
/// one difference; found nowhere, that and every cell that is different where it comes
/// closest.
pub fn differences_at(
    drawing: &Drawing,
    placement: &Placement,
    drawn: &Drawn,
    palette: &Palette,
    excused: &dyn Fn(u16, u16) -> bool,
) -> Vec<Difference> {
    assert_eq!(
        (drawn.cols(), drawn.rows()),
        (drawing.cols, drawing.rows),
        "the screen isn't the size the drawing is drawn for"
    );
    let palette = palette.on(drawing.terminal);
    let mut found = Vec::new();
    let misplaced = |screen: String| Difference {
        row: 0,
        col: 0,
        aspect: Aspect::Place,
        drawing: "its text at one place on the screen".to_owned(),
        screen,
    };
    match placement {
        Placement::At(_) => {}
        Placement::Nowhere(region) => found.push(misplaced(format!(
            "its text nowhere; it comes closest at {region}"
        ))),
        Placement::Many { first, more } => {
            let places = std::iter::once(first).chain(more).count();
            found.push(misplaced(format!("its text at {places} places")));
            return found;
        }
    }
    let region = placement.region();
    for row in 0..drawing.height {
        for col in 0..drawing.width {
            if excused(row, col) {
                continue;
            }
            let Some((wanted, paint)) = drawing.wanted(row, col) else {
                continue;
            };
            let got = drawn
                .cell(region.top + row, region.left + col)
                .painted(&palette);
            let cell = Cell {
                drawing,
                palette: &palette,
                row,
                col,
            };
            cell.compare(wanted, paint, &got, &mut found);
        }
    }
    found
}

/// One cell of a drawing being compared, at its own row and column.
struct Cell<'a> {
    drawing: &'a Drawing,
    palette: &'a Palette,
    row: u16,
    col: u16,
}

impl Cell<'_> {
    fn compare(
        &self,
        wanted: Wanted,
        paint: &Paint,
        got: &Painted<'_>,
        found: &mut Vec<Difference>,
    ) {
        let mut mismatch = |aspect, drawing: String, screen: String| {
            found.push(Difference {
                row: self.row,
                col: self.col,
                aspect,
                drawing,
                screen,
            });
        };
        let mut buffer = [0; 4];
        let text: &str = wanted.text.encode_utf8(&mut buffer);
        let (ink, fill, bold, italic) = match *paint {
            Paint::Logo { ref stem, number } => {
                let half = self.drawing.logo_half(self.row, self.col);
                // A one-colour logo takes its ink for light screens only on a light terminal.
                let light = self.drawing.terminal.is_light();
                let shows = got.picture.is_some_and(|picture| {
                    picture.shows
                        && logo_of(picture.id) == Some((stem.as_str(), number))
                        && (light || !light_ink(picture.id))
                });
                if !shows || got.text != logo_half(half) {
                    mismatch(Aspect::Picture, logo_words(stem, number, half), words(got));
                }
                return;
            }
            Paint::Picture(ref name) => {
                let shows = got
                    .picture
                    .is_some_and(|picture| picture.shows && logo_of(picture.id).is_none());
                if !shows {
                    mismatch(Aspect::Picture, format!("the picture {name}"), words(got));
                }
                return;
            }
            Paint::Character => {
                if !same_character(wanted.text, got.text) {
                    mismatch(Aspect::Text, quoted(text), words(got));
                }
                return;
            }
            Paint::Blank => (self.palette.text, self.palette.background, false, false),
            Paint::Fill(fill) => (self.palette.text, fill, false, false),
            Paint::Ink {
                ink,
                fill,
                bold,
                italic,
            } => (ink, fill.unwrap_or(self.palette.background), bold, italic),
        };
        if got.picture.is_some() {
            mismatch(Aspect::Text, quoted(text), words(got));
            return;
        }
        match (solid(text, ink, fill), solid(got.text, got.ink, got.fill)) {
            (Some(want), Some(shown)) => {
                if want != shown {
                    mismatch(
                        Aspect::Background,
                        format!("the whole cell {}", self.colour_words(want)),
                        format!("the whole cell {shown}"),
                    );
                }
            }
            (None, None) if text == got.text => {
                if ink != got.ink {
                    mismatch(
                        Aspect::TextColour,
                        self.colour_words(ink),
                        got.ink.to_string(),
                    );
                }
                if fill != got.fill {
                    mismatch(
                        Aspect::Background,
                        self.colour_words(fill),
                        got.fill.to_string(),
                    );
                }
                let flags = [
                    (Aspect::Bold, bold, got.bold),
                    (Aspect::Italic, italic, got.italic),
                ];
                for (aspect, want, has) in flags {
                    if want != has {
                        mismatch(aspect, yes_no(want), yes_no(has));
                    }
                }
            }
            _ => mismatch(Aspect::Text, quoted(text), words(got)),
        }
    }

    /// A colour as the drawing names it, and as the terminal's own where it is.
    fn colour_words(&self, rgb: Rgb) -> String {
        let mut words = rgb.to_string();
        if let Some(name) = self.drawing.name_of(rgb) {
            let _ = write!(words, " {name}");
        }
        if rgb == self.palette.background {
            words.push_str(", the terminal's own background");
        } else if rgb == self.palette.text {
            words.push_str(", the terminal's own text colour");
        }
        words
    }
}

fn logo_words(stem: &str, number: Option<u8>, half: usize) -> String {
    let side = if half == 0 { "left" } else { "right" };
    match number {
        None => format!("the {stem} logo's {side} half"),
        Some(number) => format!("the {stem} logo numbered {number}, {side} half"),
    }
}

fn quoted(text: &str) -> String {
    if blank(text) {
        "a blank".to_owned()
    } else {
        format!("{text:?}")
    }
}

fn words(got: &Painted<'_>) -> String {
    match got.picture {
        Some(picture) => picture_words(picture, got.text),
        None => quoted(got.text),
    }
}

fn yes_no(on: bool) -> String {
    if on { "on" } else { "off" }.to_owned()
}

/// The longest a report lists, so a screen drawn all wrong stays readable.
const MOST_RUNS: usize = 40;
const MOST_ROWS: usize = 12;
const MOST_PLACES: usize = 6;

/// `differences` in words, the drawing at `placement`: where it sits, each run of cells in a
/// row that are different in the same way, then each row as the drawing has it and as the
/// screen shows it, the different cells marked under them.
pub fn report(
    title: &str,
    drawing: &Drawing,
    drawn: &Drawn,
    placement: &Placement,
    differences: &[Difference],
) -> String {
    let cells: BTreeSet<(u16, u16)> = differences
        .iter()
        .filter(|d| d.aspect != Aspect::Place)
        .map(|d| (d.row, d.col))
        .collect();
    let screen = format!("{} × {} screen", drawing.cols, drawing.rows);
    let mut text = match placement {
        Placement::At(region) => format!(
            "{title}: {} cells are different from the drawing, at {region} of the {screen}.\n",
            cells.len()
        ),
        Placement::Nowhere(region) => format!(
            "{title}: the drawing's text is nowhere on the {screen}. Where it comes closest, at \
             {region}, {} cells are different.\n",
            cells.len()
        ),
        Placement::Many { first, more } => {
            let places: Vec<String> = std::iter::once(first)
                .chain(more)
                .take(MOST_PLACES)
                .map(ToString::to_string)
                .collect();
            let more = (more.len() + 1).saturating_sub(MOST_PLACES);
            let rest = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            return format!(
                "{title}: the drawing's text is at {} places on the {screen}, where it must be \
                 at one: {}{rest}.\n",
                places.len() + more,
                places.join("; ")
            );
        }
    };
    let region = placement.region();
    if (region.top, region.left) != (0, 0) && !cells.is_empty() {
        text.push_str("  Rows and columns count from the drawing's first cell.\n");
    }
    let runs = runs(differences);
    for run in runs.iter().take(MOST_RUNS) {
        let cols = if run.first == run.last {
            format!("col {}", run.first)
        } else {
            format!("cols {}-{}", run.first, run.last)
        };
        let _ = writeln!(
            text,
            "  row {}, {cols}: {}: the drawing has {}, the screen {}",
            run.row, run.aspect, run.drawing, run.screen
        );
    }
    if runs.len() > MOST_RUNS {
        let _ = writeln!(text, "  and {} more", runs.len() - MOST_RUNS);
    }
    let rows: BTreeSet<u16> = cells.iter().map(|(row, _)| *row).collect();
    for row in rows.iter().take(MOST_ROWS) {
        let marks: String = (0..drawing.width)
            .map(|col| {
                if cells.contains(&(*row, col)) {
                    '^'
                } else {
                    ' '
                }
            })
            .collect();
        let _ = write!(
            text,
            "\n  row {row}\n    drawing │{}\n    screen  │{}\n            │{}\n",
            drawing.line(*row).unwrap_or_default(),
            drawn.text_of(region.top + row, region.left, region.cols),
            marks.trim_end()
        );
    }
    if rows.len() > MOST_ROWS {
        let _ = writeln!(text, "\n  and {} more rows", rows.len() - MOST_ROWS);
    }
    text
}

/// Cells side by side in a row that are different in the same way.
struct Run<'a> {
    row: u16,
    first: u16,
    last: u16,
    aspect: Aspect,
    drawing: &'a str,
    screen: &'a str,
}

fn runs(differences: &[Difference]) -> Vec<Run<'_>> {
    let mut sorted: Vec<&Difference> = differences
        .iter()
        .filter(|d| d.aspect != Aspect::Place)
        .collect();
    sorted.sort_by_key(|d| (d.row, d.aspect as u8, d.col));
    let mut runs: Vec<Run<'_>> = Vec::new();
    for d in sorted {
        if let Some(run) = runs.last_mut()
            && run.row == d.row
            && run.aspect == d.aspect
            && run.last + 1 == d.col
            && run.drawing == d.drawing
            && run.screen == d.screen
        {
            run.last = d.col;
            continue;
        }
        runs.push(Run {
            row: d.row,
            first: d.col,
            last: d.col,
            aspect: d.aspect,
            drawing: &d.drawing,
            screen: &d.screen,
        });
    }
    runs.sort_by_key(|run| (run.row, run.first));
    runs
}
