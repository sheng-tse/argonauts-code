//! What every level draws with: soft glows, a maker's or model's badge, the ring round what the
//! pointer found, and a star's look at the universe's size, each kept once it is made.

use std::collections::HashMap;
use std::f64::consts::{PI, TAU};

use argonauts_core::look::logos::Logo;

use super::model::{Star, StarKind};
use crate::base::kit::logo::{LogoSize, logo_png};
use crate::base::theme::Brightness;
use crate::world::Image;
use crate::world::raster::noise::{fbm3_shifted, smooth};
use crate::world::raster::{Blend, Brush, Canvas, Colour, Gradient, Path, Pen, Transform};

pub(super) type Rgb = [f64; 3];

pub(super) const WHITE: Rgb = [255.0, 255.0, 255.0];
/// The app's accent, which rings what the pointer found.
const ACCENT: Colour = Colour::rgb(255, 150, 108);

pub(super) fn rgb(colour: [u8; 3]) -> Rgb {
    colour.map(f64::from)
}

/// `a` mixed `t` of the way toward `b`.
pub(super) fn mixc(a: Rgb, b: Rgb, t: f64) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// A colour at `alpha`, its channels cut to whole numbers as a stylesheet reads them.
pub(super) fn ink(colour: Rgb, alpha: f64) -> Colour {
    Colour::of(colour, alpha)
}

/// A star's glow at the universe's size, from its actions: as wide as they are many.
pub(super) fn core_radius(actions: u32) -> f64 {
    1.6 + 1.1 * (1.0 + f64::from(actions)).log2()
}

/// A star's size at the universe level, and how much a red giant swells.
pub(super) const AT_UNIVERSE: f64 = 0.62;
pub(super) const GIANT: f64 = 1.8;

/// Pictures made once and drawn many times.
#[derive(Default)]
pub(super) struct Paint {
    glows: HashMap<([u8; 3], u64), Canvas>,
    nebulas: HashMap<usize, Canvas>,
    granules: HashMap<usize, Canvas>,
    logos: HashMap<Logo, Option<Canvas>>,
}

impl Paint {
    /// A soft round glow of `colour`, white-hot at its heart and gone at its edge; `soft` is
    /// how far out its body reaches.
    fn glow(&mut self, colour: Rgb, soft: f64) -> &Canvas {
        let key = (colour.map(|c| c.clamp(0.0, 255.0) as u8), soft.to_bits());
        self.glows.entry(key).or_insert_with(|| {
            let c = key.0.map(f64::from);
            let mut canvas = Canvas::new(64, 64);
            let gradient = Gradient::radial(32.0, 32.0, 0.0, 32.0, 32.0, 32.0)
                .stop(0.0, ink(mixc(c, WHITE, 0.35), 1.0))
                .stop(soft * 0.6, ink(c, 0.42))
                .stop(1.0, ink(c, 0.0));
            canvas.fill_rect(0.0, 0.0, 64.0, 64.0, &Brush::new(gradient));
            canvas
        })
    }

    /// Lays a glow of radius `r` at (x, y).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_glow(
        &mut self,
        canvas: &mut Canvas,
        colour: Rgb,
        (x, y): (f64, f64),
        r: f64,
        alpha: f64,
        soft: f64,
        blend: Blend,
    ) {
        if alpha <= 0.005 || r <= 0.3 {
            return;
        }
        let glow = self.glow(colour, soft);
        let brush = Brush::default().alpha(alpha).blend(blend);
        canvas.draw_sized(glow, x - r, y - r, 2.0 * r, 2.0 * r, &brush);
    }

    /// A logo's picture, where one ships.
    pub fn logo(&mut self, logo: &Logo) -> Option<&Canvas> {
        self.logos
            .entry(logo.clone())
            .or_insert_with(|| {
                let png = logo_png(logo, LogoSize::Px64, Brightness::Dark)?;
                Canvas::from_image(&Image::from_png(png)?)
            })
            .as_ref()
    }

    /// A maker's or a model's sign: its logo on a dark tile two cells wide, or its colour dot
    /// where it has no logo.
    pub fn badge(
        &mut self,
        canvas: &mut Canvas,
        logo: &Logo,
        colour: Rgb,
        (x, y): (f64, f64),
        alpha: f64,
    ) {
        let size = 38.0;
        let half = size / 2.0;
        let mut tile = Path::new();
        tile.round_rect(x - half, y - half, size, size, size * 0.21);
        canvas.fill(
            &tile,
            &Brush::new(Colour::rgba(16, 18, 30, 0.74)).alpha(alpha),
        );
        canvas.stroke(
            &tile,
            &Pen::new(1.2),
            &Brush::new(ink(colour, 0.55)).alpha(alpha),
        );
        match self.logo(logo) {
            Some(picture) => {
                let side = half * 1.48;
                let brush = Brush::default().alpha(alpha);
                canvas.draw_sized(picture, x - side / 2.0, y - side / 2.0, side, side, &brush);
            }
            None => {
                let mut dot = Path::new();
                dot.circle(x, y, half * 0.42);
                canvas.fill(&dot, &Brush::new(ink(colour, 1.0)).alpha(alpha));
                let edge = Brush::new(Colour::rgba(255, 255, 255, 0.7)).alpha(alpha);
                canvas.stroke(&dot, &Pen::new(1.5), &edge);
            }
        }
    }

    /// A red giant's mottled surface, the same for the same seed.
    pub fn granules(&mut self, seed: usize) -> &Canvas {
        self.granules.entry(seed).or_insert_with(|| granules(seed))
    }

    /// A protostar's cloud: dust lit from inside, turned by `angle`.
    #[allow(clippy::too_many_arguments)]
    pub fn nebula(
        &mut self,
        canvas: &mut Canvas,
        star: &Star,
        seed: usize,
        (x, y): (f64, f64),
        r: f64,
        angle: f64,
        alpha: f64,
    ) {
        let colour = rgb(star.rgb);
        let cloud = self.nebulas.entry(seed).or_insert_with(|| {
            let size = 96usize;
            let mut bytes = vec![0u8; size * size * 4];
            for j in 0..size {
                for i in 0..size {
                    let u = (i as f64 + 0.5) / size as f64 * 2.0 - 1.0;
                    let v = (j as f64 + 0.5) / size as f64 * 2.0 - 1.0;
                    let rr = u.hypot(v);
                    if rr > 1.0 {
                        continue;
                    }
                    let th = v.atan2(u);
                    let swirl = fbm3_shifted(
                        (th + rr * 3.0).cos() * rr * 2.4,
                        (th + rr * 3.0).sin() * rr * 2.4,
                        seed as f64,
                        61.0,
                        4,
                    );
                    let density = smooth(1.0, 0.15, rr) * (0.55 + 0.6 * swirl);
                    let lit = (-rr * 3.2).exp();
                    let tint = mixc(
                        mixc([60.0, 28.0, 40.0], colour, 0.35),
                        [255.0, 150.0, 110.0],
                        lit,
                    );
                    let at = (j * size + i) * 4;
                    for k in 0..3 {
                        bytes[at + k] = tint[k].clamp(0.0, 255.0) as u8;
                    }
                    bytes[at + 3] = (density * 200.0).clamp(0.0, 230.0) as u8;
                }
            }
            Canvas::from_rgba(size as u32, size as u32, &bytes).unwrap_or_else(|| Canvas::new(1, 1))
        });
        let brush = Brush::default()
            .alpha(alpha)
            .at(Transform::IDENTITY.translate(x, y).rotate(angle));
        canvas.draw_sized(cloud, -r, -r, 2.0 * r, 2.0 * r, &brush);
    }
}

/// The ring round what the pointer found, in the app's accent.
pub(super) fn ring(canvas: &mut Canvas, (x, y): (f64, f64), r: f64, alpha: f64) {
    let mut path = Path::new();
    path.circle(x, y, r);
    canvas.stroke(
        &path,
        &Pen::new(1.5),
        &Brush::new(ACCENT.with_alpha(0.95)).alpha(alpha),
    );
}

/// A star drawn as the universe shows it, onto a canvas already set to add light. Main
/// sequence: its own colour, a white-hot core, a glow as wide as its actions. A protostar: a red
/// knot in its dark cloud. A red giant: swollen, dimmer, warmer. A white dwarf: a hard white point
/// inside the shell it shed, as wide as the days since its newer model arrived. A black hole: dark,
/// with a thin bright ring and a disc of its colour. Gives the radius the pointer finds.
#[allow(clippy::too_many_arguments)]
pub(super) fn universe_star(
    paint: &mut Paint,
    canvas: &mut Canvas,
    star: &Star,
    seed: usize,
    at: (f64, f64),
    k: f64,
    pulse: f64,
    spin: f64,
    days: u32,
) -> f64 {
    let (x, y) = at;
    let rc = core_radius(star.actions) * k;
    let c = rgb(star.rgb);
    let bright = if star.quiet() { 0.6 } else { 1.0 };
    let add = Blend::Lighter;
    match star.kind {
        StarKind::Protostar => {
            paint.nebula(canvas, star, seed, at, rc * 4.2, spin, 1.0);
            paint.draw_glow(
                canvas,
                [255.0, 120.0, 80.0],
                at,
                rc * 2.2,
                0.85 * (0.8 + 0.2 * pulse),
                0.4,
                add,
            );
            let mut knot = Path::new();
            knot.circle(x, y, (rc * 0.28).max(0.9));
            canvas.fill(
                &knot,
                &Brush::new(ink([255.0, 196.0, 150.0], 1.0)).blend(add),
            );
            rc * 2.0
        }
        StarKind::WhiteDwarf => {
            let came = star.newer.map_or(0, |(_, came)| came);
            let since = (f64::from(came) / f64::from(days.max(1))).clamp(0.0, 1.0);
            let shell = rc * (1.4 + 1.8 * since);
            let tint = mixc(c, [120.0, 220.0, 255.0], 0.55);
            let gradient = Gradient::radial(x, y, shell * 0.55, x, y, shell * 1.15)
                .stop(0.0, ink(tint, 0.0))
                .stop(0.6, ink(tint, 0.22))
                .stop(0.8, ink(mixc(tint, [255.0, 140.0, 160.0], 0.4), 0.12))
                .stop(1.0, ink(tint, 0.0));
            let mut disc = Path::new();
            disc.circle(x, y, shell * 1.15);
            canvas.fill(&disc, &Brush::new(gradient).blend(add));
            let glow = mixc(c, [220.0, 235.0, 255.0], 0.6);
            paint.draw_glow(
                canvas,
                glow,
                at,
                rc * 2.2,
                0.7 * bright * (0.85 + 0.15 * pulse),
                0.4,
                add,
            );
            let mut point = Path::new();
            point.circle(x, y, (rc * 0.3).max(1.0));
            canvas.fill(
                &point,
                &Brush::new(ink([248.0, 250.0, 255.0], bright)).blend(add),
            );
            shell
        }
        StarKind::BlackHole => {
            black_hole(paint, canvas, star, at, rc, 1.0, add);
            rc * 1.6
        }
        StarKind::Star | StarKind::RedGiant => {
            let giant = star.kind == StarKind::RedGiant;
            let cc = if giant {
                mixc(c, [255.0, 120.0, 70.0], 0.45)
            } else {
                c
            };
            let rr = if giant { rc * GIANT } else { rc };
            let halo = rr * if giant { 3.2 } else { 4.6 } * (1.0 + 0.16 * pulse);
            let strength = if giant { 0.5 } else { 0.75 } * bright * (0.82 + 0.18 * pulse);
            paint.draw_glow(
                canvas,
                cc,
                at,
                halo,
                strength,
                if giant { 0.7 } else { 0.4 },
                add,
            );
            let heat = if giant { (0.45, 0.15) } else { (0.82, 0.45) };
            let gradient = Gradient::radial(x - rr * 0.15, y - rr * 0.15, 0.0, x, y, rr * 0.62)
                .stop(0.0, ink(mixc(cc, WHITE, heat.0), bright))
                .stop(0.7, ink(mixc(cc, WHITE, heat.1), bright))
                .stop(1.0, ink(cc, if giant { 0.5 * bright } else { 0.0 }));
            let mut body = Path::new();
            body.circle(x, y, rr * 0.62);
            canvas.fill(&body, &Brush::new(gradient).blend(add));
            rr
        }
    }
}

/// A black hole: the back of its disc, the dark hole, the back of the disc bent over the top by
/// its pull, then the front of the disc across it.
pub(super) fn black_hole(
    paint: &mut Paint,
    canvas: &mut Canvas,
    star: &Star,
    at: (f64, f64),
    rc: f64,
    alpha: f64,
    blend: Blend,
) {
    let (x, y) = at;
    let c = rgb(star.rgb);
    let r0 = rc * 0.7;
    let hot = mixc(c, [255.0, 236.0, 210.0], 0.55);
    let disc = |canvas: &mut Canvas, from: f64, to: f64| {
        let flat = Transform::IDENTITY.translate(x, y).scale(1.0, 0.26);
        let gradient = Gradient::radial(0.0, 0.0, r0 * 1.15, 0.0, 0.0, r0 * 3.6)
            .stop(0.0, ink(hot, 0.95 * alpha))
            .stop(0.3, ink(c, 0.55 * alpha))
            .stop(1.0, ink(c, 0.0));
        let mut half = Path::new();
        half.arc(0.0, 0.0, r0 * 3.6, from, to, false);
        half.arc(0.0, 0.0, r0 * 1.1, to, from, true);
        half.close();
        canvas.fill(&half, &Brush::new(gradient).at(flat).blend(blend));
    };
    paint.draw_glow(canvas, c, at, rc * 3.4, 0.22 * alpha, 0.4, blend);
    disc(canvas, PI, TAU);
    let mut hole = Path::new();
    hole.circle(x, y, r0);
    canvas.fill(&hole, &Brush::new(Colour::rgba(3, 3, 6, alpha)));
    let mut bent = Path::new();
    bent.arc(0.0, 0.0, r0 * 1.22, PI * 1.04, PI * 1.96, false);
    let squash = Transform::IDENTITY.translate(x, y).scale(1.0, 0.8);
    let pen = Pen::new((r0 * 0.16).max(1.0));
    canvas.stroke(
        &bent,
        &pen,
        &Brush::new(ink(hot, 0.75 * alpha)).at(squash).blend(blend),
    );
    let mut edge = Path::new();
    edge.circle(x, y, r0 * 1.03);
    let pen = Pen::new((r0 * 0.04).max(0.8));
    canvas.stroke(
        &edge,
        &pen,
        &Brush::new(ink(hot, 0.55 * alpha)).blend(blend),
    );
    disc(canvas, 0.0, PI);
}

/// A red giant's surface boiling in convection cells.
fn granules(seed: usize) -> Canvas {
    let size = 96usize;
    let mut bytes = vec![0u8; size * size * 4];
    for j in 0..size {
        for i in 0..size {
            let u = (i as f64 + 0.5) / size as f64 * 2.0 - 1.0;
            let v = (j as f64 + 0.5) / size as f64 * 2.0 - 1.0;
            if u.hypot(v) > 1.0 {
                continue;
            }
            let n = fbm3_shifted(u * 3.2, v * 3.2, seed as f64, 71.0, 3);
            let at = (j * size + i) * 4;
            bytes[at..at + 3].copy_from_slice(&[60, 10, 4]);
            bytes[at + 3] = ((n - 0.48) * 420.0).clamp(0.0, 80.0) as u8;
        }
    }
    Canvas::from_rgba(size as u32, size as u32, &bytes).unwrap_or_else(|| Canvas::new(1, 1))
}
