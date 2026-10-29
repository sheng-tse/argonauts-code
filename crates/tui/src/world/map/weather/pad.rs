//! A canvas over part of the map, drawn on in the map's own pixels: the whole map for a layer,
//! or a small box round one mark for a sprite.

use crate::world::map::fields::{HEIGHT, WIDTH};
use crate::world::raster::{Brush, Canvas, Ink, Path, Pen, Transform};

/// A canvas whose top left lies at (`left`, `top`) on the map.
#[derive(Debug, Clone)]
pub struct Pad {
    canvas: Canvas,
    left: f64,
    top: f64,
}

impl Pad {
    /// A pad over the whole map.
    pub fn whole() -> Self {
        Self::new(0.0, 0.0, WIDTH as u32, HEIGHT as u32)
    }

    pub fn new(left: f64, top: f64, width: u32, height: u32) -> Self {
        Self {
            canvas: Canvas::new(width.max(1), height.max(1)),
            left,
            top,
        }
    }

    pub fn corner(&self) -> (f64, f64) {
        (self.left, self.top)
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    pub fn into_canvas(self) -> Canvas {
        self.canvas
    }

    /// Where the map's pixels land on this pad, then `inner` within them.
    pub fn place(&self, inner: Transform) -> Transform {
        Transform::IDENTITY
            .translate(-self.left, -self.top)
            .then(inner)
    }

    /// A brush laying `ink` in the map's own pixels.
    pub fn brush(&self, ink: impl Into<Ink>) -> Brush {
        Brush::new(ink).at(self.place(Transform::IDENTITY))
    }

    pub fn fill(&mut self, path: &Path, brush: &Brush) {
        self.canvas.fill(path, brush);
    }

    pub fn stroke(&mut self, path: &Path, pen: &Pen, brush: &Brush) {
        self.canvas.stroke(path, pen, brush);
    }

    pub fn fill_rect(&mut self, x: f64, y: f64, width: f64, height: f64, brush: &Brush) {
        self.canvas.fill_rect(x, y, width, height, brush);
    }

    /// Draws `picture` with its top left at (x, y) on the map, through `brush`'s transform
    /// within the map's pixels.
    pub fn draw(&mut self, picture: &Canvas, x: f64, y: f64, brush: &Brush) {
        self.canvas.draw(picture, x, y, brush);
    }

    /// Lays this pad onto a canvas over the whole map, where it lies.
    pub fn onto(&self, canvas: &mut Canvas) {
        canvas.draw(
            &self.canvas,
            self.left,
            self.top,
            &Brush::new(crate::world::raster::Colour::WHITE),
        );
    }
}
