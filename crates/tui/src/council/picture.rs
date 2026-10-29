//! An item's shapes drawn into one picture at the terminal's own cell size: Messages' bubbles
//! with their tails, tapback pills ringed in the screen's colour, a task's inset and a picked
//! message's ring. Edges are smooth: each pixel is sampled four by four.

use argonauts_core::names::MessageId;

use super::shapes::{CELL, Kind, RADIUS, Ring, Shape, Side, colour, picked_colour};
use crate::mascot::png::{Rgba, encode};
use crate::theme::Brightness;

/// Samples a pixel takes on each side.
const SAMPLES: usize = 4;

/// How far a ring reaches past its shape, in the shapes' pixels.
const PICKED_RING: f32 = 3.0;
const SCREEN_RING: f32 = 4.0;

/// One item's picture and where it goes: from the top left of the cell at `col`, on the item's
/// `row` from its first, `offset` pixels in. The picture starts at a cell's corner, so the
/// offset is nought unless a terminal asks to nudge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub png: Vec<u8>,
    pub col: u16,
    pub row: u16,
    pub offset: (u16, u16),
    pub size: (u16, u16),
    /// The same message picked, with its ring, for a screen that puts its pictures again
    /// while its rows can't change.
    pub picked: Option<(MessageId, Box<Picture>)>,
}

/// Draws `shapes`, measured in the 18 × 42 cells they are laid out in, for a terminal whose
/// cells are `cell` pixels. `None` for no shapes.
pub fn draw(shapes: &[Shape], cell: (u16, u16), brightness: Brightness) -> Option<Picture> {
    let first = shapes.first()?;
    let place = |shape: &Shape| {
        let (x0, y0, x1, y1) = shape.bounds();
        let top = super::shapes::y_of(shape.row);
        (x0, y0 + top, x1, y1 + top)
    };
    let (mut x0, mut y0, mut x1, mut y1) = place(first);
    for shape in &shapes[1..] {
        let (a, b, c, d) = place(shape);
        x0 = x0.min(a);
        y0 = y0.min(b);
        x1 = x1.max(c);
        y1 = y1.max(d);
    }
    x0 = x0.max(0);
    y0 = y0.max(0);
    let scale = (
        f32::from(cell.0) / CELL.0 as f32,
        f32::from(cell.1) / CELL.1 as f32,
    );
    let col = x0 / CELL.0;
    let row = y0 / CELL.1;
    let origin = (col * CELL.0, row * CELL.1);
    let width = ((x1 - origin.0) as f32 * scale.0).ceil().max(1.0) as usize;
    let height = ((y1 - origin.1) as f32 * scale.1).ceil().max(1.0) as usize;
    let mut canvas = Rgba::transparent(width, height);
    for shape in shapes {
        let top = super::shapes::y_of(shape.row) - origin.1;
        let at = Placed {
            shape: *shape,
            dx: -origin.0 as f32,
            dy: top as f32,
            scale,
        };
        match shape.ring {
            Some(Ring::Screen) => paint(&mut canvas, &at, SCREEN_RING, None),
            Some(Ring::Picked) => paint(
                &mut canvas,
                &at,
                PICKED_RING,
                Some(picked_colour(brightness)),
            ),
            None => {}
        }
        paint(&mut canvas, &at, 0.0, Some(colour(shape.fill, brightness)));
    }
    let as_u16 = |value: i32| u16::try_from(value.max(0)).unwrap_or(u16::MAX);
    let px = |value: usize| u16::try_from(value).unwrap_or(u16::MAX);
    Some(Picture {
        png: encode(&canvas),
        col: as_u16(col),
        row: as_u16(row),
        offset: (0, 0),
        size: (px(width), px(height)),
        picked: None,
    })
}

/// A shape on the canvas: moved by `dx`, `dy` in its own pixels, then scaled.
struct Placed {
    shape: Shape,
    dx: f32,
    dy: f32,
    scale: (f32, f32),
}

/// Paints the shape grown by `grow` in `fill`, or with `None` clears what it covers, as a
/// tapback's ring shows the screen between it and its bubble.
fn paint(canvas: &mut Rgba, at: &Placed, grow: f32, fill: Option<(u8, u8, u8)>) {
    let (x0, y0, x1, y1) = at.shape.bounds();
    let reach = grow + 2.0;
    let left = (((x0 as f32 + at.dx - reach) * at.scale.0).floor().max(0.0)) as usize;
    let top = (((y0 as f32 + at.dy - reach) * at.scale.1).floor().max(0.0)) as usize;
    let right = (((x1 as f32 + at.dx + reach) * at.scale.0).ceil() as usize).min(canvas.width);
    let bottom = (((y1 as f32 + at.dy + reach) * at.scale.1).ceil() as usize).min(canvas.height);
    for py in top..bottom {
        for px in left..right {
            let mut hits = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / at.scale.0 - at.dx;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / at.scale.1 - at.dy;
                    if covers(&at.shape, x, y, grow) {
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                continue;
            }
            let cover = hits as f32 / (SAMPLES * SAMPLES) as f32;
            let i = (py * canvas.width + px) * 4;
            let pixel = &mut canvas.pixels[i..i + 4];
            match fill {
                Some((r, g, b)) => over(pixel, [r, g, b], cover),
                None => pixel[3] = (f32::from(pixel[3]) * (1.0 - cover)).round() as u8,
            }
        }
    }
}

/// `colour` at `cover` over what the pixel holds.
fn over(pixel: &mut [u8], colour: [u8; 3], cover: f32) {
    let below = f32::from(pixel[3]) / 255.0;
    let alpha = cover + below * (1.0 - cover);
    if alpha <= 0.0 {
        return;
    }
    for c in 0..3 {
        let mixed =
            (f32::from(colour[c]) * cover + f32::from(pixel[c]) * below * (1.0 - cover)) / alpha;
        pixel[c] = mixed.round() as u8;
    }
    pixel[3] = (alpha * 255.0).round() as u8;
}

/// Whether the point (in the shape's own pixels, from its row's top) lies in the shape grown by
/// `grow`.
fn covers(shape: &Shape, x: f32, y: f32, grow: f32) -> bool {
    let (x0, x1, y0, y1) = (
        shape.x0 as f32 - grow,
        shape.x1 as f32 + grow,
        shape.y0 as f32 - grow,
        shape.y1 as f32 + grow,
    );
    match shape.kind {
        Kind::Bubble { tail } => {
            let radius = (RADIUS as f32 + grow).min((y1 - y0) / 2.0);
            rounded(x, y, (x0, x1, y0, y1), radius)
                || tail.is_some_and(|side| in_tail(x, y, side, shape, grow))
        }
        Kind::Pill { dot } => {
            rounded(x, y, (x0, x1, y0, y1), (y1 - y0) / 2.0)
                || dot
                    && in_ellipse(
                        x,
                        y,
                        (shape.x0 as f32 + 10.0, shape.y1 as f32),
                        (6.0 + grow, 6.0 + grow),
                    )
        }
        Kind::Typing => {
            let (sx0, sy1) = (shape.x0 as f32, shape.y1 as f32);
            rounded(x, y, (x0, x1, y0, y1), (y1 - y0) / 2.0)
                || in_ellipse(x, y, (sx0 + 1.0, sy1 - 5.0), (7.0 + grow, 7.0 + grow))
                || in_ellipse(x, y, (sx0 - 10.5, sy1 + 5.5), (3.5 + grow, 3.5 + grow))
        }
        Kind::Inset { radius } => rounded(x, y, (x0, x1, y0, y1), radius as f32 + grow),
        Kind::Rule => x >= x0 && x < x1 && y >= y0 && y < y1,
    }
}

fn in_ellipse(x: f32, y: f32, (cx, cy): (f32, f32), (rx, ry): (f32, f32)) -> bool {
    let (dx, dy) = ((x - cx) / rx, (y - cy) / ry);
    dx * dx + dy * dy <= 1.0
}

fn rounded(x: f32, y: f32, (x0, x1, y0, y1): (f32, f32, f32, f32), radius: f32) -> bool {
    if x < x0 || x >= x1 || y < y0 || y >= y1 {
        return false;
    }
    let cx = x.clamp(x0 + radius, x1 - radius);
    let cy = y.clamp(y0 + radius, y1 - radius);
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= radius * radius
}

/// A box with one bottom corner rounded by an ellipse, as CSS rounds one corner.
fn corner_box(
    x: f32,
    y: f32,
    (x0, x1, y0, y1): (f32, f32, f32, f32),
    (rx, ry): (f32, f32),
    right: bool,
) -> bool {
    if x < x0 || x >= x1 || y < y0 || y >= y1 {
        return false;
    }
    let corner_x = if right { x1 - rx } else { x0 + rx };
    let in_corner = y > y1 - ry && if right { x > corner_x } else { x < corner_x };
    !in_corner || in_ellipse(x, y, (corner_x, y1 - ry), (rx, ry))
}

/// The tail of a bubble: a box of the bubble's colour with one rounded corner, less a box with a
/// rounder one, curling out of the bottom corner toward the sender.
fn in_tail(x: f32, y: f32, side: Side, shape: &Shape, grow: f32) -> bool {
    let (x0, x1, y1) = (shape.x0 as f32, shape.x1 as f32, shape.y1 as f32 + grow);
    match side {
        Side::Left => {
            corner_box(
                x,
                y,
                (x0 - 12.6 - grow, x0 + 23.4, y1 - 36.0, y1),
                (28.8, 25.2),
                true,
            ) && !corner_box(
                x,
                y,
                (x0 - 22.5 - grow, x0 - grow, y1 - 36.0, y1),
                (18.0, 18.0),
                true,
            )
        }
        Side::Right => {
            corner_box(
                x,
                y,
                (x1 - 23.4, x1 + 12.6 + grow, y1 - 36.0, y1),
                (28.8, 25.2),
                false,
            ) && !corner_box(
                x,
                y,
                (x1 + grow, x1 + 22.5 + grow, y1 - 36.0, y1),
                (18.0, 18.0),
                false,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::council::shapes::{Fill, PAD, x_of, y_of};
    use crate::mascot::png::decode;

    fn bubble(tail: Option<Side>) -> Shape {
        Shape::new(
            0,
            Kind::Bubble { tail },
            (x_of(5) - PAD.0, x_of(40) + PAD.0),
            (CELL.1 - PAD.1, y_of(3) + PAD.1),
        )
    }

    fn pixel(image: &Rgba, x: usize, y: usize) -> [u8; 4] {
        let i = (y * image.width + x) * 4;
        image.pixels[i..i + 4].try_into().expect("four bytes")
    }

    #[test]
    fn a_bubble_is_filled_inside_round_at_its_corners_and_clear_outside() {
        let picture = draw(&[bubble(None)], (18, 42), Brightness::Dark).expect("a picture");
        let image = decode(&picture.png).expect("a png");
        let (r, g, b) = colour(Fill::Seat, Brightness::Dark);
        // The middle of the bubble.
        assert_eq!(
            pixel(&image, image.width / 2, image.height / 2),
            [r, g, b, 255]
        );
        // A corner, cut round.
        assert_eq!(pixel(&image, 0, 0)[3], 0);
        // It starts on the row above the words, 24 px down, and reaches 18 px under them.
        assert_eq!(picture.row, 0);
        assert_eq!(picture.col, 3);
        assert_eq!(
            image.height,
            (y_of(3) + PAD.1 - (CELL.1 - PAD.1)) as usize + 24
        );
    }

    #[test]
    fn a_tail_curls_out_of_the_bottom_corner_toward_its_side() {
        let shape = bubble(Some(Side::Left));
        let with = draw(&[shape], (18, 42), Brightness::Dark).expect("a picture");
        let without = draw(&[bubble(None)], (18, 42), Brightness::Dark).expect("a picture");
        let with = decode(&with.png).expect("a png");
        let without = decode(&without.png).expect("a png");
        // The tail reaches left of the bubble's edge, near its bottom.
        let opaque = |image: &Rgba| image.pixels.chunks(4).filter(|p| p[3] > 0).count();
        assert!(opaque(&with) > opaque(&without));
    }

    #[test]
    fn a_picture_scales_to_the_terminals_cells() {
        let small = draw(&[bubble(None)], (18, 42), Brightness::Dark).expect("a picture");
        let large = draw(&[bubble(None)], (36, 84), Brightness::Dark).expect("a picture");
        assert_eq!(large.size.0, small.size.0 * 2);
        assert_eq!(large.size.1, small.size.1 * 2);
    }

    #[test]
    fn a_tapbacks_ring_clears_the_bubble_round_it() {
        let pill = Shape::new(0, Kind::Pill { dot: true }, (x_of(30), x_of(38)), (3, 39))
            .filled(Fill::Chip)
            .ringed(Some(Ring::Screen));
        let picture = draw(&[bubble(None), pill], (18, 42), Brightness::Dark).expect("a picture");
        let image = decode(&picture.png).expect("a png");
        // Just under the pill's bottom, where the bubble would be, the screen shows through.
        let x = (x_of(34) - x_of(3)) as usize;
        let y = 39 + 2;
        assert!(pixel(&image, x, y)[3] < 255, "{:?}", pixel(&image, x, y));
    }
}
