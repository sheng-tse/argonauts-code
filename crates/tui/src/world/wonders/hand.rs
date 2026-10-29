//! The hand that draws a wonder's parts, in units of its size: `u` right from its
//! middle and `v` up from its ground line. One hand draws every part in one of four passes: the
//! shade it casts, its dark rim, its lit body, and the glow of its fires. Each part is shaded
//! by the way it faces: the viewer stands south-west of each wonder, so faces turned left catch
//! a north-west light and faces turned right sit in shade.

use crate::world::map::fields::{Rgb, mix_rgb};
use crate::world::raster::{Brush, Canvas, Cap, Colour, Gradient, Join, Path, Pen};

/// The dark rim round every part, and the cool shade the dark side leans to.
pub const EDGE: Rgb = [18.0, 19.0, 30.0];
const COOL: Rgb = [32.0, 40.0, 74.0];

/// Which pass the hand draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// The shade each part casts, skewed away from the light.
    Cast,
    /// The dark rim round every part.
    Rim,
    /// The lit parts.
    Body,
    /// The light of a wonder's fires, laid on brightening.
    Glow,
}

/// A normal on the screen: right, up, and toward the viewer.
pub type Normal = [f64; 3];

/// The normals the parts are shaded by.
pub mod facing {
    use super::Normal;
    pub const FRONT: Normal = [0.0, 0.0, 1.0];
    pub const LEFT: Normal = [-0.72, 0.0, 0.7];
    pub const RIGHT: Normal = [0.72, 0.0, 0.7];
    pub const TOP: Normal = [0.0, 1.0, 0.35];
    pub const UP_LEFT: Normal = [-0.55, 0.55, 0.62];
    pub const UP_RIGHT: Normal = [0.55, 0.55, 0.62];
    pub const LIT_LEFT: Normal = [-0.4, 0.15, 0.9];
    pub const SHADE_RIGHT: Normal = [0.45, 0.0, 0.89];
}

/// One step of an outline, in units of the wonder's size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Move(f64, f64),
    Line(f64, f64),
    Quad(f64, f64, f64, f64),
    Close,
}

/// A part shaded across: from one unit point to another, by normals evenly spaced.
pub type Across = ((f64, f64), (f64, f64), &'static [Normal]);

/// How a part is drawn, beyond its shape, colour and normal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// It casts no shade, or has no rim.
    pub no_cast: bool,
    pub no_rim: bool,
    /// Its colour as given, unshaded.
    pub raw: bool,
    pub alpha: f64,
    /// Shaded across from one point to another by the normals given, evenly spaced.
    pub across: Option<Across>,
    /// A line's least width in pixels, and its ends.
    pub min: Option<f64>,
    pub cap: Cap,
    /// A ball squashed down by this much.
    pub squash: f64,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            no_cast: false,
            no_rim: false,
            raw: false,
            alpha: 1.0,
            across: None,
            min: None,
            cap: Cap::Round,
            squash: 1.0,
        }
    }
}

impl Look {
    /// A flat detail: no shade and no rim.
    pub fn flat() -> Self {
        Self {
            no_cast: true,
            no_rim: true,
            ..Self::default()
        }
    }

    /// A flat detail in its own colour, at `alpha`.
    pub fn raw(alpha: f64) -> Self {
        Self {
            raw: true,
            alpha,
            ..Self::flat()
        }
    }

    pub fn no_cast(mut self) -> Self {
        self.no_cast = true;
        self
    }

    pub fn no_rim(mut self) -> Self {
        self.no_rim = true;
        self
    }

    pub fn unshaded(mut self) -> Self {
        self.raw = true;
        self
    }

    pub fn min(mut self, min: f64) -> Self {
        self.min = Some(min);
        self
    }

    pub fn cap(mut self, cap: Cap) -> Self {
        self.cap = cap;
        self
    }

    pub fn squash(mut self, squash: f64) -> Self {
        self.squash = squash;
        self
    }

    pub fn across(mut self, from: (f64, f64), to: (f64, f64), normals: &'static [Normal]) -> Self {
        self.across = Some((from, to, normals));
        self
    }
}

/// What a box's corners came to, for drawing along its edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxCorners {
    pub left: (f64, f64),
    pub front: (f64, f64),
    pub right: (f64, f64),
}

/// The hand: where the wonder's ground point lies on the canvas, how big it is, the light, and
/// the pass it draws.
pub struct Hand<'a> {
    canvas: &'a mut Canvas,
    ground: (f64, f64),
    pub size: f64,
    pub pass: Pass,
    /// The light along the wonder's own right, up and toward-the-viewer axes.
    light: [f64; 3],
    /// How far the shade is skewed for each unit of height.
    skew: (f64, f64),
    rim: f64,
    blend_lighter: bool,
}

impl<'a> Hand<'a> {
    /// A hand drawing on `canvas`, the wonder's ground point at `ground`, lit from `light` in the
    /// map's frame: x right, y down the screen, z up out of it.
    pub fn new(
        canvas: &'a mut Canvas,
        ground: (f64, f64),
        size: f64,
        light: [f64; 3],
        pass: Pass,
    ) -> Self {
        let length = (light[0] * light[0] + light[1] * light[1] + light[2] * light[2]).sqrt();
        let length = if length == 0.0 { 1.0 } else { length };
        let l = [light[0] / length, light[1] / length, light[2] / length];
        let half = std::f64::consts::FRAC_1_SQRT_2;
        let along = [l[0] * half + l[1] * half, l[2], -l[0] * half + l[1] * half];
        let lz = l[2].max(0.25);
        Self {
            canvas,
            ground,
            size,
            pass,
            light: along,
            skew: (-l[0] / lz * 0.5, -l[1] / lz * 0.25),
            rim: (size * 0.042).max(1.15),
            blend_lighter: pass == Pass::Glow,
        }
    }

    /// Where a unit point lands on the canvas; in the shade pass, cast along the ground.
    pub fn point(&self, u: f64, v: f64) -> (f64, f64) {
        let (gx, gy, s) = (self.ground.0, self.ground.1, self.size);
        if self.pass == Pass::Cast {
            let h = v.max(0.0);
            (
                gx + (u + h * self.skew.0) * s,
                gy + (h * self.skew.1 - v.min(0.0)) * s,
            )
        } else {
            (gx + u * s, gy - v * s)
        }
    }

    /// A colour shaded by the way its part faces.
    pub fn shade(&self, rgb: Rgb, normal: Option<Normal>) -> Rgb {
        let d = normal.map_or(0.0, |n| {
            let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            let length = if length == 0.0 { 1.0 } else { length };
            (n[0] * self.light[0] + n[1] * self.light[1] + n[2] * self.light[2]) / length
        });
        let lam = 0.5 + 0.5 * d;
        let k = 0.42 + 0.7 * lam;
        mix_rgb(
            [rgb[0] * k, rgb[1] * k, rgb[2] * k],
            COOL,
            (1.0 - lam) * 0.22,
        )
    }

    /// The light along the wonder's right and up axes, for a ball's highlight.
    pub fn light_across(&self) -> (f64, f64) {
        (self.light[0], self.light[1])
    }

    fn skip(&self, look: &Look) -> bool {
        match self.pass {
            Pass::Glow => true,
            Pass::Cast => look.no_cast,
            Pass::Rim => look.no_rim,
            Pass::Body => false,
        }
    }

    fn trace(&self, steps: &[Step]) -> Path {
        let mut path = Path::new();
        for step in steps {
            match *step {
                Step::Move(u, v) => {
                    let (x, y) = self.point(u, v);
                    path.move_to(x, y);
                }
                Step::Line(u, v) => {
                    let (x, y) = self.point(u, v);
                    path.line_to(x, y);
                }
                Step::Quad(a, b, u, v) => {
                    let (cx, cy) = self.point(a, b);
                    let (x, y) = self.point(u, v);
                    path.quad_to(cx, cy, x, y);
                }
                Step::Close => path.close(),
            }
        }
        path
    }

    fn brush(&self, colour: Colour) -> Brush {
        let brush = Brush::new(colour);
        if self.blend_lighter {
            brush.blend(crate::world::raster::Blend::Lighter)
        } else {
            brush
        }
    }

    /// A filled shape, shaded flat by its normal or across by a run of normals.
    pub fn shape(&mut self, steps: &[Step], rgb: Rgb, normal: Option<Normal>, look: Look) {
        if self.skip(&look) {
            return;
        }
        let path = self.trace(steps);
        match self.pass {
            Pass::Cast => self.canvas.fill(&path, &self.brush(Colour::BLACK)),
            Pass::Rim => {
                let edge = clamped_colour(EDGE, 1.0);
                self.canvas.fill(&path, &self.brush(edge));
                let pen = Pen::new(self.rim * 2.0).join(Join::Round);
                self.canvas.stroke(&path, &pen, &self.brush(edge));
            }
            Pass::Body => {
                let ink = match look.across {
                    Some((from, to, normals)) => {
                        let (a, b) = (self.point(from.0, from.1), self.point(to.0, to.1));
                        let last = (normals.len().max(2) - 1) as f64;
                        let mut gradient = Gradient::linear(a.0, a.1, b.0, b.1);
                        for (i, normal) in normals.iter().enumerate() {
                            let colour = clamped_colour(self.shade(rgb, Some(*normal)), look.alpha);
                            gradient = gradient.stop(i as f64 / last, colour);
                        }
                        Brush::new(gradient)
                    }
                    None => {
                        let colour = if look.raw {
                            rgb
                        } else {
                            self.shade(rgb, normal)
                        };
                        self.brush(clamped_colour(colour, look.alpha))
                    }
                };
                self.canvas.fill(&path, &ink);
            }
            Pass::Glow => {}
        }
    }

    /// A filled polygon through unit points.
    pub fn poly(&mut self, points: &[(f64, f64)], rgb: Rgb, normal: Option<Normal>, look: Look) {
        let mut steps: Vec<Step> = points
            .iter()
            .enumerate()
            .map(|(i, &(u, v))| {
                if i == 0 {
                    Step::Move(u, v)
                } else {
                    Step::Line(u, v)
                }
            })
            .collect();
        steps.push(Step::Close);
        self.shape(&steps, rgb, normal, look);
    }

    fn stroke_path(
        &mut self,
        path: &Path,
        width: f64,
        rgb: Rgb,
        normal: Option<Normal>,
        look: Look,
        cap: Cap,
    ) {
        let px = (width * self.size).max(look.min.unwrap_or(0.8));
        match self.pass {
            Pass::Cast => {
                let pen = Pen::new(px).cap(cap).join(Join::Round);
                self.canvas.stroke(path, &pen, &self.brush(Colour::BLACK));
            }
            Pass::Rim => {
                let pen = Pen::new(px + self.rim * 2.0).cap(cap).join(Join::Round);
                self.canvas
                    .stroke(path, &pen, &self.brush(clamped_colour(EDGE, 1.0)));
            }
            Pass::Body => {
                let colour = if look.raw {
                    rgb
                } else {
                    self.shade(rgb, normal)
                };
                let pen = Pen::new(px).cap(cap).join(Join::Round);
                self.canvas
                    .stroke(path, &pen, &self.brush(clamped_colour(colour, look.alpha)));
            }
            Pass::Glow => {}
        }
    }

    /// A stroked line through unit points, `width` sizes wide and at least its least width.
    pub fn line(
        &mut self,
        points: &[(f64, f64)],
        width: f64,
        rgb: Rgb,
        normal: Option<Normal>,
        look: Look,
    ) {
        if self.skip(&look) {
            return;
        }
        let mut path = Path::new();
        for (i, &(u, v)) in points.iter().enumerate() {
            let (x, y) = self.point(u, v);
            if i == 0 {
                path.move_to(x, y);
            } else {
                path.line_to(x, y);
            }
        }
        self.stroke_path(&path, width, rgb, normal, look, look.cap);
    }

    /// A stroked outline, always with round ends.
    pub fn curve(
        &mut self,
        steps: &[Step],
        width: f64,
        rgb: Rgb,
        normal: Option<Normal>,
        look: Look,
    ) {
        if self.skip(&look) {
            return;
        }
        let path = self.trace(steps);
        self.stroke_path(&path, width, rgb, normal, look, Cap::Round);
    }

    fn ellipse_path(&self, u: f64, v: f64, rx: f64, ry: f64) -> Path {
        let (x, y) = self.point(u, v);
        let mut path = Path::new();
        path.ellipse(
            x,
            y,
            (rx * self.size).max(0.3),
            (ry * self.size).max(0.3),
            0.0,
            0.0,
            std::f64::consts::TAU,
            false,
        );
        path
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(
        &mut self,
        u: f64,
        v: f64,
        rx: f64,
        ry: f64,
        rgb: Rgb,
        normal: Option<Normal>,
        look: Look,
    ) {
        if self.skip(&look) {
            return;
        }
        let path = self.ellipse_path(u, v, rx, ry);
        match self.pass {
            Pass::Cast => self.canvas.fill(&path, &self.brush(Colour::BLACK)),
            Pass::Rim => {
                let edge = clamped_colour(EDGE, 1.0);
                self.canvas.fill(&path, &self.brush(edge));
                self.canvas
                    .stroke(&path, &Pen::new(self.rim * 2.0), &self.brush(edge));
            }
            Pass::Body => {
                let colour = if look.raw {
                    rgb
                } else {
                    self.shade(rgb, normal)
                };
                self.canvas
                    .fill(&path, &self.brush(clamped_colour(colour, look.alpha)));
            }
            Pass::Glow => {}
        }
    }

    /// A round thing lit from the light's side: a tree's crown, a boulder, a head.
    pub fn ball(&mut self, u: f64, v: f64, r: f64, rgb: Rgb, look: Look) {
        if self.skip(&look) {
            return;
        }
        if self.pass != Pass::Body {
            self.ellipse(u, v, r, r * look.squash, rgb, None, look);
            return;
        }
        let (cx, cy) = self.point(u, v);
        let radius = r * self.size;
        let (lx, ly) = self.light_across();
        let (hx, hy) = (cx + lx * radius * 0.45, cy - ly * radius * 0.45);
        let gradient = Gradient::radial(hx, hy, radius * 0.08, cx, cy, radius * 1.05)
            .stop(
                0.0,
                clamped_colour(self.shade(rgb, Some([lx, ly, 0.6])), look.alpha),
            )
            .stop(
                0.55,
                clamped_colour(self.shade(rgb, Some([0.0, 0.0, 1.0])), look.alpha),
            )
            .stop(
                1.0,
                clamped_colour(self.shade(rgb, Some([-lx, -ly, 0.3])), look.alpha),
            );
        let mut path = Path::new();
        path.ellipse(
            cx,
            cy,
            radius,
            radius * look.squash,
            0.0,
            0.0,
            std::f64::consts::TAU,
            false,
        );
        self.canvas.fill(&path, &Brush::new(gradient));
    }

    /// A standing cylinder from `v0` up to `v1`: a column, a tower's round tier; with a cap of
    /// `cap` unless it has none.
    #[allow(clippy::too_many_arguments)]
    pub fn cylinder(
        &mut self,
        u: f64,
        v0: f64,
        v1: f64,
        r: f64,
        rgb: Rgb,
        cap: Option<Rgb>,
        look: Look,
    ) {
        if self.skip(&look) {
            return;
        }
        let e = r * 0.32;
        let steps = [
            Step::Move(u - r, v0),
            Step::Line(u - r, v1),
            Step::Quad(u, v1 + e * 2.0, u + r, v1),
            Step::Line(u + r, v0),
            Step::Quad(u, v0 - e * 2.0, u - r, v0),
            Step::Close,
        ];
        if self.pass != Pass::Body {
            self.shape(&steps, rgb, None, look);
            return;
        }
        const ROUND: [Normal; 6] = [
            [-1.0, 0.0, 0.223_606_797_749_979],
            [-0.6, 0.0, 0.8],
            [-0.15, 0.0, 0.988_685_996_664_197_9],
            [0.35, 0.0, 0.936_749_699_759_8],
            [0.8, 0.0, 0.6],
            [1.0, 0.0, 0.223_606_797_749_979],
        ];
        self.shape(
            &steps,
            rgb,
            None,
            look.across((u - r, v0), (u + r, v0), &ROUND),
        );
        if let Some(cap) = cap {
            self.ellipse(u, v1, r, e, cap, Some(facing::TOP), look);
        }
    }

    /// A box seen from the south-west, its left face, its right face and, unless `top` is
    /// `None`, its top in that colour; `depth` of its width deep.
    #[allow(clippy::too_many_arguments)]
    pub fn cuboid(
        &mut self,
        cu: f64,
        v0: f64,
        w: f64,
        h: f64,
        rgb: Rgb,
        top: Option<Rgb>,
        depth: f64,
        look: Look,
    ) -> BoxCorners {
        let dp = w * depth;
        let (left, front, right, back) = (
            (cu - w / 2.0, v0),
            (cu, v0 - dp),
            (cu + w / 2.0, v0),
            (cu, v0 + dp),
        );
        let up = |p: (f64, f64)| (p.0, p.1 + h);
        self.poly(
            &[left, front, up(front), up(left)],
            rgb,
            Some(facing::LEFT),
            look,
        );
        self.poly(
            &[front, right, up(right), up(front)],
            rgb,
            Some(facing::RIGHT),
            look,
        );
        if let Some(top) = top {
            self.poly(
                &[up(left), up(front), up(right), up(back)],
                top,
                Some(facing::TOP),
                look,
            );
        }
        BoxCorners { left, front, right }
    }

    /// A ripple on the water: flat, no shade, no rim.
    pub fn ripple(&mut self, u: f64, v: f64, rx: f64, alpha: f64) {
        if self.pass != Pass::Body {
            return;
        }
        let (x, y) = self.point(u, v);
        let s = self.size;
        let mut path = Path::new();
        let pi = std::f64::consts::PI;
        path.ellipse(x, y, rx * s, rx * s * 0.22, 0.0, pi * 0.1, pi * 0.9, true);
        let foam = super::art::colours::FOAM;
        let pen = Pen::new((s * 0.018).max(0.8));
        self.canvas
            .stroke(&path, &pen, &Brush::new(clamped_colour(foam, alpha)));
    }

    /// The light of a fire, laid on brightening in the glow pass.
    pub fn glow(&mut self, u: f64, v: f64, r: f64, rgb: Rgb, alpha: f64) {
        if self.pass != Pass::Glow {
            return;
        }
        let (x, y) = self.point(u, v);
        let radius = r * self.size;
        let gradient = Gradient::radial(x, y, 0.0, x, y, radius)
            .stop(0.0, clamped_colour(rgb, alpha))
            .stop(1.0, clamped_colour(rgb, 0.0));
        let mut path = Path::new();
        path.circle(x, y, radius);
        self.canvas
            .fill(&path, &self.brush(Colour::WHITE).ink(gradient));
    }
}

/// A colour as a canvas takes it from a style: each channel held to 0..=255 and cut to a whole
/// number.
pub fn clamped_colour(rgb: Rgb, alpha: f64) -> Colour {
    let channel = |v: f64| v.clamp(0.0, 255.0).trunc() as u8;
    Colour::rgba(channel(rgb[0]), channel(rgb[1]), channel(rgb[2]), alpha)
}
