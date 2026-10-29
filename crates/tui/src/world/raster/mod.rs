//! The drawing engine: paths, curves, gradients, blur and sprite blits into RGBA pictures.
//!
//! It follows the usual 2D canvas rules: angles in radians running clockwise on a screen whose
//! y grows downwards,
//! transforms that apply to what is drawn after them, colours blended without gamma, and
//! every edge anti-aliased.
//!
//! A [`Canvas`] holds the pixels. A [`Path`] is the shape, a [`Pen`] turns it into a line,
//! and a [`Brush`] says how a draw lands: its colour or [`Gradient`], where it goes, how
//! strongly, and how it mixes with what is there ([`Blend`]). A canvas draws onto another
//! as a sprite, turned and sized by the brush's [`Transform`]. [`noise`] gives the seeded
//! randomness the world grows from, and a [`Grid`] holds the fields it is read through.
//!
//! | On a 2D canvas | Here |
//! |---|---|
//! | `fillStyle`, then `fill()` or `fillRect` | [`Canvas::fill`], [`Canvas::fill_rect`] with [`Brush::new`] |
//! | `strokeStyle`, `lineWidth`, `lineCap`, `lineJoin`, `setLineDash`, `lineDashOffset` | [`Canvas::stroke`] with a [`Pen`] |
//! | `createLinearGradient`, `createRadialGradient`, `createConicGradient` | [`Gradient`] |
//! | `globalAlpha`, `globalCompositeOperation` | [`Brush::alpha`], [`Brush::blend`] |
//! | `filter = "brightness(k) blur(s px)"` | [`Brush::brightness`], [`Brush::blur`] |
//! | the blurred colour cast under a draw | [`Brush::glow`] |
//! | `translate`, `rotate`, `scale`, `setTransform` | [`Brush::at`] with a [`Transform`] |
//! | `save()`, `clip()`, draws, `restore()` | [`Canvas::with_clip`] |
//! | `drawImage` with three, five or nine numbers | [`Canvas::draw`], [`Canvas::draw_sized`], [`Canvas::draw_part`] |
//! | `imageSmoothingEnabled`, `imageSmoothingQuality` | [`Brush::smoothing`] |
//! | `getImageData`, `putImageData`, `clearRect` | [`Canvas::to_rgba`], [`Canvas::put_rgba`], [`Canvas::clear`] |
//!
//! ```
//! use argonauts_tui::world::raster::{Brush, Canvas, Colour, Path, Pen};
//!
//! let mut canvas = Canvas::new(64, 32);
//! let mut dot = Path::new();
//! dot.circle(16.0, 16.0, 6.0);
//! canvas.fill(&dot, &Brush::new(Colour::rgb(122, 162, 247)));
//!
//! let mut line = Path::new();
//! line.move_to(28.0, 16.0);
//! line.line_to(60.0, 16.0);
//! canvas.stroke(&line, &Pen::new(2.0).round(), &Brush::new(Colour::WHITE).alpha(0.5));
//!
//! assert_eq!(canvas.pixel(16, 16), [122, 162, 247, 255]);
//! let png = canvas.to_png();
//! # assert!(!png.is_empty());
//! ```

mod blend;
mod blit;
mod brush;
mod canvas;
mod colour;
mod draw;
mod filter;
mod grid;
mod ink;
pub mod noise;
mod path;
mod pen;
mod transform;

#[cfg(test)]
mod tests;

pub use blend::Blend;
pub use blit::Smoothing;
pub use brush::Brush;
pub use canvas::Canvas;
pub use colour::Colour;
pub use grid::Grid;
pub use ink::{Gradient, Ink};
pub use path::{Corners, Path};
pub use pen::{Cap, Join, Pen};
pub use transform::Transform;
