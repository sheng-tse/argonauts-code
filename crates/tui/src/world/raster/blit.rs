use tiny_skia::{FillRule, FilterQuality, IntRect, Paint, Pattern, Pixmap, SpreadMode};

use super::brush::Brush;
use super::canvas::Canvas;
use super::draw::Reach;
use super::transform::Transform;

/// How a picture's pixels are read when it lands between whole pixels or at another size, as
/// a canvas's `imageSmoothingEnabled` and `imageSmoothingQuality` choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Smoothing {
    /// The nearest pixel, hard edged: smoothing off.
    Nearest,
    /// The four nearest pixels, mixed by distance: a canvas's default.
    #[default]
    Bilinear,
    /// Sixteen pixels round it, sharper when a picture grows: a canvas's high quality.
    Bicubic,
}

impl Smoothing {
    fn skia(self) -> FilterQuality {
        match self {
            Self::Nearest => FilterQuality::Nearest,
            Self::Bilinear => FilterQuality::Bilinear,
            Self::Bicubic => FilterQuality::Bicubic,
        }
    }
}

impl Canvas {
    /// Draws `picture` with its top left at (x, y), through the brush's transform, as a
    /// canvas's `drawImage(picture, x, y)`.
    pub fn draw(&mut self, picture: &Canvas, x: f64, y: f64, brush: &Brush) {
        let (width, height) = (f64::from(picture.width()), f64::from(picture.height()));
        self.draw_sized(picture, x, y, width, height, brush);
    }

    /// Draws `picture` stretched over the box from (x, y) across `width` and down `height`,
    /// as a canvas's `drawImage(picture, x, y, width, height)`.
    pub fn draw_sized(
        &mut self,
        picture: &Canvas,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        brush: &Brush,
    ) {
        self.blit(&picture.pixmap, [x, y, width, height], brush);
    }

    /// Draws the part of `picture` in the box `from` stretched over the box `to`, each as x,
    /// y, width and height, as a canvas's nine-number `drawImage`. Nothing outside `from` is
    /// read, so a sprite sheet's neighbours don't bleed in.
    pub fn draw_part(&mut self, picture: &Canvas, from: [f64; 4], to: [f64; 4], brush: &Brush) {
        let [x, y, width, height] = from;
        let (left, top) = (x.floor().max(0.0), y.floor().max(0.0));
        let right = (x + width).ceil().min(f64::from(picture.width()));
        let bottom = (y + height).ceil().min(f64::from(picture.height()));
        if !(width > 0.0 && height > 0.0 && right > left && bottom > top) {
            return;
        }
        let whole = IntRect::from_ltrb(left as i32, top as i32, right as i32, bottom as i32);
        let Some(part) = whole.and_then(|rect| picture.pixmap.clone_rect(rect)) else {
            return;
        };
        // The part read in whole pixels, placed so that `from` itself lands on `to`.
        let [tx, ty, tw, th] = to;
        let (sx, sy) = (tw / width, th / height);
        let placed = [
            tx + (left - x) * sx,
            ty + (top - y) * sy,
            (right - left) * sx,
            (bottom - top) * sy,
        ];
        self.blit(&part, placed, brush);
    }

    /// Draws all of `picture` over the box `to`, through the brush.
    fn blit(&mut self, picture: &Pixmap, to: [f64; 4], brush: &Brush) {
        let [x, y, width, height] = to;
        let (pw, ph) = (f64::from(picture.width()), f64::from(picture.height()));
        if !(width.is_finite() && height.is_finite() && width != 0.0 && height != 0.0) {
            return;
        }
        let local = Transform::IDENTITY
            .translate(x, y)
            .scale(width / pw, height / ph);
        let Some(bounds) = tiny_skia::Rect::from_xywh(0.0, 0.0, pw as f32, ph as f32) else {
            return;
        };
        let reach = Reach::placed(bounds, 0.0, brush.transform().then(local));
        let smoothing = brush.smoothing_of();
        self.lay(brush, reach, |pixmap, landing| {
            let transform = landing.transform.pre_concat(local.skia());
            let Some(outline) = tiny_skia::PathBuilder::from_rect(bounds).transform(transform)
            else {
                return;
            };
            let shifted;
            let (picture, placed, quality) = match shift_of(transform) {
                Some((x, y)) if x.fract() == 0.0 && y.fract() == 0.0 => {
                    (picture, transform, FilterQuality::Nearest)
                }
                // tiny-skia reads a picture that is only shifted by its nearest pixels, where
                // a canvas mixes them, so the picture is moved by the part of a pixel first.
                Some((x, y)) if smoothing != Smoothing::Nearest => {
                    shifted = shifted_by(picture, x - x.floor(), y - y.floor());
                    let whole = tiny_skia::Transform::from_translate(x.floor(), y.floor());
                    (&shifted, whole, FilterQuality::Nearest)
                }
                _ => (picture, transform, smoothing.skia()),
            };
            let paint = Paint {
                shader: Pattern::new(
                    picture.as_ref(),
                    SpreadMode::Pad,
                    quality,
                    landing.strength,
                    placed,
                ),
                blend_mode: landing.blend,
                anti_alias: soft_edged(transform, pw, ph),
                ..Paint::default()
            };
            pixmap.fill_path(
                &outline,
                &paint,
                FillRule::Winding,
                tiny_skia::Transform::identity(),
                landing.clip,
            );
        });
    }
}

/// Whether a picture placed by `transform` gets anti-aliased edges. A canvas gives them only
/// to a picture turned off the pixel grid or squeezed under a pixel; one that lands square
/// covers each pixel whose centre it covers, so neighbouring tiles meet without a seam.
fn soft_edged(transform: tiny_skia::Transform, width: f64, height: f64) -> bool {
    let tilted = transform.kx != 0.0 || transform.ky != 0.0;
    let turned_square = transform.sx == 0.0 && transform.sy == 0.0;
    if tilted && !turned_square {
        return true;
    }
    let (across, down) = if turned_square {
        (
            f64::from(transform.ky.abs()) * width,
            f64::from(transform.kx.abs()) * height,
        )
    } else {
        (
            f64::from(transform.sx.abs()) * width,
            f64::from(transform.sy.abs()) * height,
        )
    };
    across < 1.0 || down < 1.0
}

/// How far the transform shifts, when shifting is all it does.
fn shift_of(transform: tiny_skia::Transform) -> Option<(f32, f32)> {
    (!transform.has_scale() && !transform.has_skew()).then_some((transform.tx, transform.ty))
}

/// The premultiplied `picture` moved right by `x` and down by `y`, each under a pixel, its
/// pixels mixed with their neighbours by how far each one lands over them, and its edge
/// pixels carried on past its edges, as a canvas reads a picture. It is a pixel wider and
/// taller, to hold what spills over.
fn shifted_by(picture: &Pixmap, x: f32, y: f32) -> Pixmap {
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let Some(mut out) = Pixmap::new(width as u32 + 1, height as u32 + 1) else {
        return picture.clone();
    };
    let weights = [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y]
        .map(|weight| (weight * 256.0).round() as u32);
    let from = picture.data();
    let read = |column: usize, row: usize, channel: usize| -> u32 {
        let (column, row) = (column.min(width - 1), row.min(height - 1));
        u32::from(from[(row * width + column) * 4 + channel])
    };
    let out_width = width + 1;
    for (index, pixel) in out.data_mut().chunks_exact_mut(4).enumerate() {
        let (column, row) = (index % out_width, index / out_width);
        // Each pixel takes from the one it sits on and the ones to its left and above.
        let left = column.saturating_sub(1);
        let up = row.saturating_sub(1);
        for (channel, value) in pixel.iter_mut().enumerate() {
            let sum = weights[0] * read(column, row, channel)
                + weights[1] * read(left, row, channel)
                + weights[2] * read(column, up, channel)
                + weights[3] * read(left, up, channel);
            *value = ((sum + 128) >> 8).min(255) as u8;
        }
    }
    out
}
