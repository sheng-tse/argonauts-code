//! The land's kinds as drawn on the ground. Each lies flat on the land and keeps to it, its
//! edge on the coast's own ragged line, so none spills out over the sea as a pale halo.

use std::f64::consts::{PI, TAU};

use super::LandMark;
use crate::world::map::fields::{Ground, byte};
use crate::world::map::land::coast_at;
use crate::world::raster::noise::{Rng, fbm3, mix, smooth};
use crate::world::raster::{Brush, Canvas, Colour, Gradient, Path, Pen, Transform};

/// What a land kind is drawn onto.
pub enum Onto<'a> {
    /// The map's ground marks, keeping to this ground's land.
    Map(&'a Ground, &'a mut Canvas),
    /// A picture of its own, placed by the transform, with no coast to keep to.
    Picture(&'a mut Canvas, Transform),
}

/// A drawing's canvas and the transform that places the map's own pixels on it, so its paths
/// are given in the map's pixels.
struct Patch<'a> {
    canvas: &'a mut Canvas,
    place: Transform,
}

impl Patch<'_> {
    fn brush(&self, ink: impl Into<crate::world::raster::Ink>) -> Brush {
        Brush::new(ink).at(self.place)
    }

    fn fill(&mut self, path: &Path, ink: impl Into<crate::world::raster::Ink>) {
        let brush = self.brush(ink);
        self.canvas.fill(path, &brush);
    }

    fn stroke(&mut self, path: &Path, pen: &Pen, ink: impl Into<crate::world::raster::Ink>) {
        let brush = self.brush(ink);
        self.canvas.stroke(path, pen, &brush);
    }
}

/// On the map, draws `draw` within `reach` px of (x, y) in a small canvas of its own, then
/// fades it out where the land gives way to the sea and lays it on the ground marks. On a
/// picture of its own it draws as it is.
fn on_land(onto: Onto, x: f64, y: f64, reach: f64, draw: impl FnOnce(&mut Patch)) {
    let (ground, canvas) = match onto {
        Onto::Map(ground, canvas) => (ground, canvas),
        Onto::Picture(canvas, place) => return draw(&mut Patch { canvas, place }),
    };
    let (left, top) = ((x - reach).floor(), (y - reach).floor());
    let side = (2.0 * reach).ceil() as u32 + 2;
    let mut own = Canvas::new(side, side);
    draw(&mut Patch {
        canvas: &mut own,
        place: Transform::IDENTITY.translate(-left, -top),
    });
    let mut rgba = own.to_rgba();
    for j in 0..side as usize {
        for i in 0..side as usize {
            let at = (j * side as usize + i) * 4 + 3;
            if rgba[at] > 0 {
                let inside = coast_at(ground, left + i as f64 + 0.5, top + j as f64 + 0.5);
                rgba[at] = byte(f64::from(rgba[at]) * smooth(-0.5, 1.5, inside));
            }
        }
    }
    own.put_rgba(&rgba);
    canvas.draw(&own, left, top, &Brush::new(Colour::WHITE));
}

/// A round shape with a wobbly edge, the same for the same seed.
pub fn blob(
    x: f64,
    y: f64,
    radius: f64,
    seed: f64,
    wobble: f64,
    points: u32,
    frequency: f64,
) -> Path {
    let mut path = Path::new();
    for k in 0..=points {
        let a = f64::from(k) / f64::from(points) * TAU;
        let r =
            radius * (1.0 + wobble * fbm3(a.cos() * frequency, a.sin() * frequency, seed, 11.0, 3));
        let (px, py) = (x + a.cos() * r, y + a.sin() * r);
        if k == 0 {
            path.move_to(px, py);
        } else {
            path.line_to(px, py);
        }
    }
    path.close();
    path
}

/// A straight line bent by noise, never at its ends.
pub fn meander(x0: f64, y0: f64, x1: f64, y1: f64, seed: u32) -> Vec<(f64, f64)> {
    let length = (x1 - x0).hypot(y1 - y0);
    let steps = ((length / 4.0).ceil() as usize).max(8);
    let (nx, ny) = (-(y1 - y0) / length, (x1 - x0) / length);
    let amplitude = (0.13 * length).min(16.0);
    (0..=steps)
        .map(|s| {
            let t = s as f64 / steps as f64;
            let off = amplitude
                * (PI * t).sin()
                * (fbm3(t * length / 38.0, f64::from(seed % 97), 1.3, 9.0, 3) * 1.6);
            (mix(x0, x1, t) + nx * off, mix(y0, y1, t) + ny * off)
        })
        .collect()
}

fn polyline(points: &[(f64, f64)]) -> Path {
    let mut path = Path::new();
    for (k, &(x, y)) in points.iter().enumerate() {
        if k == 0 {
            path.move_to(x, y);
        } else {
            path.line_to(x, y);
        }
    }
    path
}

fn ellipse(x: f64, y: f64, rx: f64, ry: f64, rotation: f64) -> Path {
    let mut path = Path::new();
    path.ellipse(x, y, rx, ry, rotation, 0.0, 7.0, false);
    path
}

fn circle(x: f64, y: f64, r: f64) -> Path {
    let mut path = Path::new();
    path.circle(x, y, r);
    path
}

/// A swamp, drawn as wetland looks from the air: dark wet sedge, darker and browner than any
/// forest, threaded with bright channels and pools that mirror the sky, reeds at their edges
/// and a few dead snags. Bright water on dark ground is what reads as wet.
pub fn swamp(onto: Onto, mark: &LandMark) {
    let (x0, y0, r) = (mark.x, mark.y, mark.radius);
    let label = mark.behavior as u32;
    on_land(onto, x0, y0, r * 1.4, |patch| {
        let mut rng = Rng::new(label.wrapping_mul(53).wrapping_add(11));
        let mut next = || rng.next_f64();
        let sedge = Gradient::radial(x0, y0, r * 0.45, x0, y0, r * 1.3)
            .stop(0.0, Colour::rgba(54, 62, 36, 0.97))
            .stop(0.72, Colour::rgba(60, 68, 38, 0.92))
            .stop(1.0, Colour::rgba(66, 74, 42, 0.0));
        patch.fill(
            &blob(x0, y0, r * 1.3, f64::from(label) + 3.0, 0.3, 28, 1.3),
            sedge,
        );
        // Tussocks: paler sedge in clumps.
        for _ in 0..30 {
            let a = next() * TAU;
            let d = next().sqrt() * r * 1.05;
            let colour = Colour::rgba(
                (96.0 + 20.0 * next()) as u8,
                (100.0 + 16.0 * next()) as u8,
                (52.0 + 10.0 * next()) as u8,
                0.6,
            );
            let (rx, ry, turn) = (1.2 + next() * 1.6, 0.9 + next(), next() * 3.0);
            patch.fill(
                &ellipse(
                    x0 + a.cos() * d * 1.1,
                    y0 + a.sin() * d * 0.85,
                    rx,
                    ry,
                    turn,
                ),
                colour,
            );
        }
        // Channels winding through it, lit by the sky.
        for c in 0..3 {
            let a0 = next() * TAU;
            let a1 = a0 + PI * (0.6 + 0.6 * next());
            let (sx, sy) = (x0 + a0.cos() * r * 0.95, y0 + a0.sin() * r * 0.75);
            let (ex, ey) = (x0 + a1.cos() * r * 0.95, y0 + a1.sin() * r * 0.75);
            let points: Vec<(f64, f64)> = (0..=14)
                .map(|s| {
                    let t = f64::from(s) / 14.0;
                    let wobble = (t * PI * (2.5 + f64::from(c))).sin() * r * 0.16 * (PI * t).sin();
                    (
                        mix(sx, ex, t) - (ey - sy) / r * wobble,
                        mix(sy, ey, t) + (ex - sx) / r * wobble,
                    )
                })
                .collect();
            let channel = polyline(&points);
            patch.stroke(
                &channel,
                &Pen::new(3.0).round(),
                Colour::rgba(18, 34, 40, 0.9),
            );
            patch.stroke(
                &channel,
                &Pen::new(1.5).round(),
                Colour::rgba(120, 166, 184, 0.95),
            );
        }
        // The still pools, mirroring the sky.
        for k in 0..18 {
            let a = next() * TAU;
            let d = next().sqrt() * r * 0.85;
            let (px, py) = (x0 + a.cos() * d * 1.15, y0 + a.sin() * d * 0.85);
            let pr = 1.4 + next() * 2.6;
            let seed = f64::from(label * 7 + k);
            patch.fill(
                &blob(px, py, pr + 0.7, seed, 0.35, 12, 1.3),
                Colour::rgba(20, 36, 40, 0.9),
            );
            patch.fill(
                &blob(px, py, pr, seed, 0.35, 12, 1.3),
                Colour::rgb(92, 140, 162),
            );
            patch.fill(
                &ellipse(px - pr * 0.25, py - pr * 0.3, pr * 0.5, pr * 0.22, -0.3),
                Colour::rgba(214, 236, 242, 0.9),
            );
            let mut reeds = Path::new();
            reeds.arc(px, py, pr + 0.9, PI * 0.1, PI * 0.8, false);
            patch.stroke(
                &reeds,
                &Pen::new(0.9).round(),
                Colour::rgba(40, 48, 24, 0.95),
            );
        }
        // Dead snags.
        for _ in 0..5 {
            let sx = x0 + (next() - 0.5) * r * 1.6;
            let sy = y0 + (next() - 0.5) * r * 1.1;
            let snag = polyline(&[(sx, sy + 1.5), (sx + 0.4, sy - 3.0)]);
            patch.stroke(&snag, &Pen::new(1.0).round(), Colour::rgb(40, 32, 26));
        }
    });
}

/// A basin: the flat valley floor at a behavior's foot that agents reach from many others. A
/// pale alluvial plain ringed by the darker foot of the slopes round it, and from the side of
/// each behavior they come from, a stream from the floor's own rim to a pool in the middle,
/// where the streams meet. `from` holds each of those behaviors with the middle of its dots.
pub fn basin(onto: Onto, mark: &LandMark, from: &[(usize, (f64, f64))]) {
    let (x0, y0, r) = (mark.x, mark.y, mark.radius);
    let label = mark.behavior;
    on_land(onto, x0, y0, r * 1.5, |patch| {
        let seed = label as f64 + 7.0;
        let floor = Gradient::radial(x0, y0, 2.0, x0, y0, r * 1.3)
            .stop(0.0, Colour::rgba(178, 186, 112, 0.96))
            .stop(0.7, Colour::rgba(162, 174, 100, 0.9))
            .stop(1.0, Colour::rgba(122, 142, 78, 0.0));
        patch.fill(&blob(x0, y0, r * 1.3, seed, 0.22, 28, 1.3), floor);
        patch.stroke(
            &blob(x0, y0, r * 1.06, seed, 0.22, 28, 1.3),
            &Pen::new(1.6),
            Colour::rgba(66, 80, 42, 0.75),
        );
        for &(source, (fx, fy)) in from {
            let a = (fy - y0).atan2(fx - x0);
            let (sx, sy) = (x0 + a.cos() * r * 1.2, y0 + a.sin() * r * 1.2);
            let seed = (source * 31 + label) as u32;
            let stream = polyline(&meander(
                sx,
                sy,
                x0 + a.cos() * 2.0,
                y0 + a.sin() * 2.0,
                seed,
            ));
            patch.stroke(&stream, &Pen::new(3.0).round(), Colour::rgb(30, 70, 110));
            patch.stroke(&stream, &Pen::new(1.6).round(), Colour::rgb(112, 178, 212));
        }
        // The pool where they meet.
        patch.fill(&ellipse(x0, y0, 4.0, 2.9, 0.0), Colour::rgb(30, 70, 110));
        patch.fill(&ellipse(x0, y0, 3.0, 2.1, 0.0), Colour::rgb(112, 178, 212));
        patch.fill(
            &ellipse(x0 - 0.8, y0 - 0.6, 1.3, 0.5, -0.3),
            Colour::rgba(232, 246, 250, 0.9),
        );
    });
}

/// An oasis: a green pool ringed by palms on a patch of grass, in sand. The pool is lit
/// turquoise and the palms are dark stars, so it reads at the map's own size.
pub fn oasis(onto: Onto, x: f64, y: f64, label: usize) {
    on_land(onto, x, y, 22.0, |patch| {
        let grass = Gradient::radial(x, y, 5.0, x, y, 19.0)
            .stop(0.0, Colour::rgba(104, 146, 60, 0.96))
            .stop(0.7, Colour::rgba(118, 148, 68, 0.85))
            .stop(1.0, Colour::rgba(150, 160, 90, 0.0));
        patch.fill(&circle(x, y, 19.0), grass);
        let pool = blob(x, y + 0.5, 7.4, label as f64 + 3.0, 0.25, 16, 1.3);
        patch.fill(&pool, Colour::rgb(46, 168, 170));
        patch.stroke(&pool, &Pen::new(1.0), Colour::rgba(232, 238, 204, 0.85));
        patch.fill(
            &ellipse(x - 1.6, y - 1.1, 2.2, 0.8, -0.3),
            Colour::rgba(230, 250, 250, 0.9),
        );
        let mut rng = Rng::new((label as u32).wrapping_mul(5).wrapping_add(1));
        // Palms: a dark star of fronds over the shade they cast.
        for k in 0..11 {
            let a = f64::from(k) / 11.0 * TAU + rng.next_f64() * 0.4;
            let rr = 10.6 + rng.next_f64() * 2.0;
            let (px, py) = (x + a.cos() * rr, y + a.sin() * rr * 0.86);
            patch.fill(
                &ellipse(px + 1.4, py + 1.2, 2.6, 1.5, 0.0),
                Colour::rgba(20, 24, 16, 0.4),
            );
            for q in 0..5 {
                let b = f64::from(q) / 5.0 * TAU + a;
                let frond = polyline(&[(px, py), (px + b.cos() * 2.9, py + b.sin() * 2.4)]);
                patch.stroke(&frond, &Pen::new(1.3), Colour::rgb(26, 70, 30));
            }
        }
    });
}
