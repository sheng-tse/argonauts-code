use std::f64::consts::{FRAC_PI_2, TAU};

use tiny_skia::PathBuilder;

/// A shape to fill, stroke or clip to, built the way a canvas path is: lines, curves, arcs and
/// boxes, each subpath starting where the last one left off unless moved.
#[derive(Debug, Clone, Default)]
pub struct Path {
    builder: PathBuilder,
    /// The pen's position, once the path has one.
    at: Option<(f64, f64)>,
    /// Where the open subpath began, where closing it returns.
    start: (f64, f64),
}

/// The radii of a rounded box's corners: top left, top right, bottom right, bottom left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Corners(pub [f64; 4]);

impl From<f64> for Corners {
    fn from(radius: f64) -> Self {
        Self([radius; 4])
    }
}

impl From<[f64; 4]> for Corners {
    fn from(radii: [f64; 4]) -> Self {
        Self(radii)
    }
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.builder.is_empty()
    }

    /// Starts a new subpath at (x, y).
    pub fn move_to(&mut self, x: f64, y: f64) {
        self.builder.move_to(x as f32, y as f32);
        self.at = Some((x, y));
        self.start = (x, y);
    }

    /// A straight line to (x, y), or the start of a subpath there when the path has none.
    pub fn line_to(&mut self, x: f64, y: f64) {
        if self.at.is_none() {
            return self.move_to(x, y);
        }
        self.builder.line_to(x as f32, y as f32);
        self.at = Some((x, y));
    }

    /// A quadratic curve bent toward (cx, cy), ending at (x, y).
    pub fn quad_to(&mut self, cx: f64, cy: f64, x: f64, y: f64) {
        self.begin_at(cx, cy);
        self.builder
            .quad_to(cx as f32, cy as f32, x as f32, y as f32);
        self.at = Some((x, y));
    }

    /// A cubic curve pulled toward (c1x, c1y) and then (c2x, c2y), ending at (x, y).
    pub fn cubic_to(&mut self, c1x: f64, c1y: f64, c2x: f64, c2y: f64, x: f64, y: f64) {
        self.begin_at(c1x, c1y);
        self.builder.cubic_to(
            c1x as f32, c1y as f32, c2x as f32, c2y as f32, x as f32, y as f32,
        );
        self.at = Some((x, y));
    }

    /// Closes the subpath back to its start, where the next one begins.
    pub fn close(&mut self) {
        if self.at.is_some() {
            self.builder.close();
            self.at = Some(self.start);
        }
    }

    /// Part of a circle round (cx, cy), from angle `from` to angle `to` in radians, clockwise
    /// unless `anticlockwise`. A line joins it to the pen's position first, as a canvas's `arc`
    /// does.
    pub fn arc(&mut self, cx: f64, cy: f64, radius: f64, from: f64, to: f64, anticlockwise: bool) {
        self.ellipse(cx, cy, radius, radius, 0.0, from, to, anticlockwise);
    }

    /// A whole circle as its own subpath, starting from its rightmost point.
    pub fn circle(&mut self, cx: f64, cy: f64, radius: f64) {
        self.move_to(cx + radius, cy);
        self.arc(cx, cy, radius, 0.0, TAU, false);
    }

    /// Part of an ellipse round (cx, cy) with radii `rx` and `ry`, turned by `rotation`, its
    /// angles measured as a canvas's `ellipse` measures them.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(
        &mut self,
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
        rotation: f64,
        from: f64,
        to: f64,
        anticlockwise: bool,
    ) {
        let finite = [cx, cy, rx, ry, rotation, from, to]
            .iter()
            .all(|v| v.is_finite());
        if !finite || rx < 0.0 || ry < 0.0 {
            return;
        }
        let sweep = sweep(from, to, anticlockwise);
        let (sin, cos) = rotation.sin_cos();
        let point = |angle: f64| {
            let (x, y) = (rx * angle.cos(), ry * angle.sin());
            (cx + x * cos - y * sin, cy + x * sin + y * cos)
        };
        let (x, y) = point(from);
        self.line_to(x, y);

        // Each piece spans at most a quarter turn, where a cubic stays within a few
        // ten-thousandths of the radius from the true curve.
        let pieces = (sweep.abs() / FRAC_PI_2).ceil().max(1.0) as usize;
        let step = sweep / pieces as f64;
        let reach = 4.0 / 3.0 * (step / 4.0).tan();
        let tangent = |angle: f64| {
            let (x, y) = (-rx * angle.sin(), ry * angle.cos());
            (x * cos - y * sin, x * sin + y * cos)
        };
        for piece in 0..pieces {
            let a0 = from + step * piece as f64;
            let a1 = a0 + step;
            let (x0, y0) = point(a0);
            let (x1, y1) = point(a1);
            let (t0x, t0y) = tangent(a0);
            let (t1x, t1y) = tangent(a1);
            self.cubic_to(
                x0 + t0x * reach,
                y0 + t0y * reach,
                x1 - t1x * reach,
                y1 - t1y * reach,
                x1,
                y1,
            );
        }
    }

    /// A box as its own closed subpath; the pen is left at its top left corner.
    pub fn rect(&mut self, x: f64, y: f64, width: f64, height: f64) {
        self.move_to(x, y);
        self.line_to(x + width, y);
        self.line_to(x + width, y + height);
        self.line_to(x, y + height);
        self.close();
        self.move_to(x, y);
    }

    /// A box with rounded corners. Radii too big for the box shrink together until they fit,
    /// as a canvas's `roundRect` shrinks them.
    pub fn round_rect(
        &mut self,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        corners: impl Into<Corners>,
    ) {
        let (x, width) = if width < 0.0 {
            (x + width, -width)
        } else {
            (x, width)
        };
        let (y, height) = if height < 0.0 {
            (y + height, -height)
        } else {
            (y, height)
        };
        let Corners(radii) = corners.into();
        let [tl, tr, br, bl] = radii.map(|r| if r.is_finite() { r.max(0.0) } else { 0.0 });
        let fit = [
            width / (tl + tr),
            height / (tr + br),
            width / (br + bl),
            height / (bl + tl),
        ]
        .into_iter()
        .filter(|scale| scale.is_finite())
        .fold(1.0_f64, f64::min);
        let [tl, tr, br, bl] = [tl, tr, br, bl].map(|r| r * fit);

        self.move_to(x + tl, y);
        self.line_to(x + width - tr, y);
        self.corner(x + width - tr, y + tr, tr, -FRAC_PI_2);
        self.line_to(x + width, y + height - br);
        self.corner(x + width - br, y + height - br, br, 0.0);
        self.line_to(x + bl, y + height);
        self.corner(x + bl, y + height - bl, bl, FRAC_PI_2);
        self.line_to(x, y + tl);
        self.corner(x + tl, y + tl, tl, std::f64::consts::PI);
        self.close();
        self.move_to(x, y);
    }

    /// A quarter turn clockwise round (cx, cy) from angle `from`.
    fn corner(&mut self, cx: f64, cy: f64, radius: f64, from: f64) {
        if radius > 0.0 {
            self.arc(cx, cy, radius, from, from + FRAC_PI_2, false);
        }
    }

    /// Starts a subpath at (x, y) when the path has no pen position yet.
    fn begin_at(&mut self, x: f64, y: f64) {
        if self.at.is_none() {
            self.move_to(x, y);
        }
    }

    pub(super) fn skia(&self) -> Option<tiny_skia::Path> {
        self.builder.clone().finish()
    }
}

/// How far an arc from `from` to `to` turns, signed, as a canvas works it out: a full turn or
/// more draws the whole circle, and otherwise the arc goes the asked way round from one angle
/// to the other.
fn sweep(from: f64, to: f64, anticlockwise: bool) -> f64 {
    if !anticlockwise && to - from >= TAU {
        TAU
    } else if anticlockwise && from - to >= TAU {
        -TAU
    } else if !anticlockwise && from > to {
        TAU - (from - to) % TAU
    } else if anticlockwise && from < to {
        -(TAU - (to - from) % TAU)
    } else {
        to - from
    }
}
