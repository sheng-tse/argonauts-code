//! The pictures under the talk's words: Messages' bubbles, the small pills of tapbacks, quotes
//! and typing, and a task's inset. Each is a shape in pixels of a cell 18 wide and 42 tall,
//! measured from the left edge of the screen and the top of the row it starts on, so a row that
//! scrolls carries its picture with it. [`super::picture`] draws them at the terminal's own cell
//! size.
//!
//! The bubble keeps Messages' proportions at the app's cell size: the words keep the grid, and
//! the bubble reaches 28 px past them on each side and 18 px above and below, so it takes part
//! of the row over its words and of the row under them, which hold nothing else but a tapback.

use crate::theme::Brightness;

/// A cell's width and height, in the pixels the shapes are measured in.
pub const CELL: (i32, i32) = (18, 42);

/// How far a bubble reaches past its words, sideways and up and down.
pub const PAD: (i32, i32) = (28, 18);

/// A bubble's corner radius; a one-row bubble is as round as it is tall.
pub const RADIUS: i32 = 40;

/// What a shape is filled with. The colours are Messages' own, in the theme's light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fill {
    /// A seat's bubble, Messages' grey.
    Seat,
    /// Your bubble, Messages' blue.
    You,
    /// A tapback's pill, a step lighter than a seat's bubble.
    Chip,
    /// The faded copy of a seat's words a reply answers.
    Quote,
    /// The faded copy of your words a reply answers.
    YourQuote,
    /// A task's inset, a step darker than the bubble.
    Inset,
    /// The thin line from a reply's quote down into its bubble.
    Thread,
    /// A held message's bubble, faded, since no seat has read it.
    Held,
}

/// A ring drawn round a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ring {
    /// A picked message or task, in the picked colour.
    Picked,
    /// The screen's own colour, between a tapback and the bubble it sits on.
    Screen,
}

/// Which way a bubble's tail curls: toward the seat's avatar on the left, or toward you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A rounded rectangle, with its tail on the last bubble of a run.
    Bubble { tail: Option<Side> },
    /// A stadium; with `dot`, a tapback's small dot of a tail toward its message.
    Pill { dot: bool },
    /// The typing bubble: a pill with Messages' two small circles of a tail.
    Typing,
    /// A rectangle rounded by `radius`.
    Inset { radius: i32 },
    /// A thin straight rectangle.
    Rule,
}

/// One shape, on the row of its item it starts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shape {
    /// The item's row it is measured from.
    pub row: usize,
    pub kind: Kind,
    pub x0: i32,
    pub x1: i32,
    pub y0: i32,
    pub y1: i32,
    pub fill: Fill,
    pub ring: Option<Ring>,
}

impl Shape {
    pub fn new(row: usize, kind: Kind, (x0, x1): (i32, i32), (y0, y1): (i32, i32)) -> Self {
        Self {
            row,
            kind,
            x0,
            x1,
            y0,
            y1,
            fill: Fill::Seat,
            ring: None,
        }
    }

    pub fn filled(mut self, fill: Fill) -> Self {
        self.fill = fill;
        self
    }

    pub fn ringed(mut self, ring: Option<Ring>) -> Self {
        self.ring = ring;
        self
    }

    /// The same shape measured from `rows` rows further down, as an item's rows sit under a
    /// leading blank.
    pub fn moved(mut self, rows: usize) -> Self {
        self.row += rows;
        self
    }

    /// The pixels it can touch, its ring and tail included, from the top left of its row:
    /// x0, y0, x1, y1.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        let margin = match self.ring {
            Some(_) => 6,
            None => 0,
        };
        let (left, below) = match self.kind {
            Kind::Bubble { tail: Some(_) } => (14, 0),
            Kind::Typing => (16, 10),
            Kind::Pill { dot: true } => (0, 6),
            _ => (0, 0),
        };
        (
            self.x0 - left - margin,
            self.y0 - margin,
            self.x1 + left + margin,
            self.y1 + below + margin,
        )
    }
}

/// The column a cell starts at, in pixels.
pub fn x_of(cells: usize) -> i32 {
    i32::try_from(cells).unwrap_or(i32::MAX / CELL.0) * CELL.0
}

/// The row a cell starts at, in pixels.
pub fn y_of(cells: usize) -> i32 {
    i32::try_from(cells).unwrap_or(i32::MAX / CELL.1) * CELL.1
}

/// A fill's colour on a dark or a light screen: Messages' grey and blue, with the tapback's
/// pill a step lighter and a task's inset a step darker than the bubble it sits in.
pub fn colour(fill: Fill, brightness: Brightness) -> (u8, u8, u8) {
    let dark = brightness == Brightness::Dark;
    match (fill, dark) {
        (Fill::Seat, true) => (0x36, 0x3a, 0x56),
        (Fill::Seat, false) => (0xe9, 0xe9, 0xeb),
        (Fill::You, _) => (0x0a, 0x84, 0xff),
        (Fill::Chip, true) => (0x45, 0x4b, 0x6e),
        (Fill::Chip, false) => (0xd8, 0xd8, 0xdc),
        (Fill::Quote, true) => (0x2b, 0x2f, 0x45),
        (Fill::Quote, false) => (0xf2, 0xf2, 0xf4),
        (Fill::YourQuote, true) => (0x1d, 0x3f, 0x73),
        (Fill::YourQuote, false) => (0xcf, 0xe4, 0xff),
        (Fill::Inset, true) => (0x2a, 0x2e, 0x45),
        (Fill::Inset, false) => (0xf6, 0xf6, 0xf8),
        (Fill::Thread, true) => (0x99, 0x99, 0x99),
        (Fill::Thread, false) => (0x8a, 0x8a, 0x8a),
        (Fill::Held, true) => (0x2b, 0x2f, 0x45),
        (Fill::Held, false) => (0xf2, 0xf2, 0xf4),
    }
}

/// A picked message's ring, and a picked block bubble's fill.
pub fn picked_colour(brightness: Brightness) -> (u8, u8, u8) {
    match brightness {
        Brightness::Dark => (0xb1, 0xb9, 0xf9),
        Brightness::Light => (0x52, 0x5a, 0x8a),
    }
}

/// A picked block bubble's own fill, a step lighter than a seat's.
pub fn picked_fill(brightness: Brightness) -> (u8, u8, u8) {
    match brightness {
        Brightness::Dark => (0x52, 0x5a, 0x8a),
        Brightness::Light => (0xc9, 0xcd, 0xf0),
    }
}
