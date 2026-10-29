//! The far stars behind every level, of two kinds and nothing else. The far field: the same
//! faint stars for everyone, so even a new crew's sky reads as space; it carries no data, so it
//! is kept fainter than any star that does. And your earlier actions: one star each, gathered
//! into a cluster per earlier month, the older the month the dimmer and warmer its stars, with a
//! faint glow under each cluster that is the light of all its stars together.
//!
//! Inside a galaxy or a system the sky keeps the far field alone: there the month clusters would
//! sit over the arms and orbits and read as that maker's own stars.

use std::f64::consts::PI;

use super::model::{Universe, rng_for};
use super::shapes::{DRAWN, Frame};
use crate::world::raster::Canvas;
use crate::world::raster::noise::{Rng, int32, mix};

/// An earlier month's cluster as one picture places it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Cluster {
    /// Months back from the period, 0 for the month just before it.
    pub back: usize,
    pub stars: u32,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    /// A few clumps inside, so it is as uneven as a real star cloud: where each lies from the
    /// middle, and how wide.
    lumps: [(f64, f64, f64); 3],
    turn: f64,
    flat: f64,
    /// From 0 for the newest month to 1 for the oldest.
    age: f64,
}

impl Cluster {
    /// The ellipse inside which the pointer finds it: its middle, its radii and how far it is
    /// turned.
    pub fn outline(&self) -> Option<(f64, f64, f64, f64, f64)> {
        let reach = self.radius * 1.05;
        (reach > 0.0).then_some((self.x, self.y, reach, reach * self.flat, self.turn))
    }
}

/// The far field's stars on a picture of the drawn size; a picture of another size has as many
/// for its area.
const FAR_STARS: f64 = 3600.0;

/// Each earlier month's cluster on a picture this big: spread over the sky, clear of your
/// planet, the galaxies and each other.
pub(super) fn clusters(
    universe: &Universe,
    galaxies: &[Frame],
    width: f64,
    height: f64,
) -> Vec<Cluster> {
    let first = universe.months.first().map_or("", |m| m.month.as_str());
    let mut rng = rng_for(first, 7919);
    let count = universe.months.len();
    let area = (width * height / (DRAWN.0 * DRAWN.1)).sqrt();
    let mut out: Vec<Cluster> = Vec::with_capacity(count);
    for (back, month) in universe.months.iter().enumerate() {
        let radius = (26.0 + 2.1 * f64::from(month.stars).sqrt()) * area;
        let mut best = (width / 2.0, height / 2.0);
        let mut best_score = f64::NEG_INFINITY;
        for _ in 0..60 {
            let x = width * (0.06 + 0.88 * rng.next_f64());
            let y = height * (0.08 + 0.84 * rng.next_f64());
            let mut score =
                -(120.0 - ((x - width / 2.0) * 0.8).hypot(y - height / 2.0)).max(0.0) * 3.0;
            for other in &out {
                let gap = (other.x - x).hypot((other.y - y) * 1.3);
                score -= ((other.radius + radius) * 0.9 - gap).max(0.0) * 2.0;
            }
            for galaxy in galaxies {
                let gap = (galaxy.x - x).hypot((galaxy.y - y) * 1.4);
                score -= (galaxy.r + radius * 0.7 - gap).max(0.0) * 2.5;
            }
            if score > best_score {
                best_score = score;
                best = (x, y);
            }
        }
        let mut lump = || {
            (
                (rng.next_f64() - 0.5) * radius * 0.8,
                (rng.next_f64() - 0.5) * radius * 0.5,
                0.6 + 0.5 * rng.next_f64(),
            )
        };
        let lumps = [lump(), lump(), lump()];
        out.push(Cluster {
            back,
            stars: month.stars,
            x: best.0,
            y: best.1,
            radius,
            lumps,
            turn: rng.next_f64() * PI,
            flat: 0.55 + 0.3 * rng.next_f64(),
            age: if count > 1 {
                back as f64 / (count - 1) as f64
            } else {
                0.0
            },
        });
    }
    out
}

/// The earlier month whose cluster's glow a point lies in, the nearest middle first.
pub(super) fn cluster_at(clusters: &[Cluster], x: f64, y: f64) -> Option<usize> {
    let mut best = None;
    let mut nearest = 1.0;
    for cluster in clusters {
        let (sin, cos) = cluster.turn.sin_cos();
        let (dx, dy) = (x - cluster.x, y - cluster.y);
        let reach = cluster.radius * 1.05;
        let u = (dx * cos + dy * sin) / reach;
        let v = (-dx * sin + dy * cos) / (reach * cluster.flat);
        let e = u * u + v * v;
        if e < nearest {
            nearest = e;
            best = Some(cluster.back);
        }
    }
    best
}

/// A star's colour: most are pale blue-white, some bluer, and a few warm, more of them the older
/// the month.
fn hue(rng: &mut Rng, age: f64) -> [f64; 3] {
    let warm = rng.next_f64();
    let warm_share = 0.12 + 0.5 * age;
    if warm < warm_share {
        [255.0, 214.0, 170.0]
    } else if warm < warm_share + 0.5 {
        [214.0, 222.0, 245.0]
    } else {
        [178.0, 198.0, 255.0]
    }
}

/// The sky as one opaque picture: the deep blue fading toward its edges, the galaxy you are inside
/// as a faint band of `band` where given, each cluster's glow and stars, and the far field.
pub(super) fn sky(clusters: &[Cluster], width: u32, height: u32, band: Option<[u8; 3]>) -> Canvas {
    let (w, h) = (f64::from(width), f64::from(height));
    let (wi, hi) = (width as usize, height as usize);
    let mut rgb = vec![[0.0f32; 3]; wi * hi];
    let glows: Vec<(f64, [f64; 3], f64, f64)> = clusters
        .iter()
        .map(|c| {
            let amp = 0.12 * (1.0 - 0.5 * c.age) * (f64::from(c.stars) / 600.0).sqrt().min(1.0);
            let colour = [
                mix(176.0, 222.0, c.age),
                mix(190.0, 192.0, c.age),
                mix(236.0, 164.0, c.age),
            ];
            let (sin, cos) = c.turn.sin_cos();
            (amp, colour, cos, sin)
        })
        .collect();
    for y in 0..hi {
        let yf = y as f64;
        for x in 0..wi {
            let xf = x as f64;
            let v = ((xf - w / 2.0) / w).hypot((yf - h / 2.0) / h);
            let fade = 1.0 - 0.38 * v;
            let mut c = [23.0 * fade, 24.0 * fade, 37.0 * fade];
            if let Some(band) = band {
                let by = (yf - h * (0.16 + 0.18 * xf / w)) / (h * 0.11);
                let ripple =
                    (xf / 53.0 + (yf / 31.0).sin() * 1.7).sin() * (xf / 23.0 + yf / 41.0).sin();
                let m = (-by * by).exp() * (0.62 + 0.38 * ripple) * 0.075;
                for k in 0..3 {
                    c[k] = mix(c[k], f64::from(band[k]), m);
                }
            }
            for (cluster, (amp, colour, cos, sin)) in clusters.iter().zip(&glows) {
                let (dx, dy) = (xf - cluster.x, yf - cluster.y);
                let u = (dx * cos + dy * sin) / cluster.radius;
                let v = (-dx * sin + dy * cos) / (cluster.radius * cluster.flat);
                let e = u * u + v * v;
                if e < 9.0 {
                    let a = amp * (-1.4 * e).exp();
                    for k in 0..3 {
                        c[k] = mix(c[k], colour[k], a);
                    }
                }
            }
            rgb[y * wi + x] = c.map(|v| v as f32);
        }
    }
    let mut dot = |x: i32, y: i32, colour: [f64; 3], alpha: f64| {
        if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
            return;
        }
        let pixel = &mut rgb[y as usize * wi + x as usize];
        for k in 0..3 {
            pixel[k] = mix(f64::from(pixel[k]), colour[k], alpha) as f32;
        }
    };
    let mut rng = Rng::new(99);
    let area = w * h / (DRAWN.0 * DRAWN.1);
    let far = (FAR_STARS * area).ceil() as usize;
    for _ in 0..far {
        let x = int32(rng.next_f64() * w);
        let y = int32(rng.next_f64() * h);
        let colour = hue(&mut rng, 0.2);
        // Each fainter than the faintest star of yours, which start at 0.22.
        let alpha = 0.05 + 0.15 * rng.next_f64().powf(1.4);
        dot(x, y, colour, alpha);
    }
    for cluster in clusters {
        let (sin, cos) = cluster.turn.sin_cos();
        let fade = 1.0 - 0.5 * cluster.age;
        for _ in 0..cluster.stars {
            // A core round its clumps and a looser halo, each star at a bell-curve distance in a
            // random direction, so it is densest at its heart and thins out with no edge.
            let halo = rng.next_f64() < 0.32;
            let lump = cluster.lumps[((rng.next_f64() * 3.0) as usize).min(2)];
            let spread = cluster.radius * if halo { 0.95 } else { 0.3 * lump.2 };
            let (ox, oy) = if halo { (0.0, 0.0) } else { (lump.0, lump.1) };
            let reach = (-2.0 * (1.0 - 0.999 * rng.next_f64()).ln()).sqrt()
                * if halo { 0.5 } else { 0.42 }
                * spread;
            let angle = rng.next_f64() * PI * 2.0;
            let gx = ox + angle.cos() * reach;
            let gy = oy + angle.sin() * reach * cluster.flat;
            let x = int32(cluster.x + gx * cos - gy * sin);
            let y = int32(cluster.y + gx * sin + gy * cos);
            let bright = rng.next_f64().powi(3);
            let alpha = 0.22 + 0.76 * bright * fade;
            let colour = hue(&mut rng, cluster.age);
            dot(x, y, colour, alpha);
            // The brightest with a soft cross of light.
            if bright > 0.6 {
                for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    dot(x + dx, y + dy, colour, alpha * 0.36);
                }
            }
            if bright > 0.93 {
                for (dx, dy) in [(2, 0), (-2, 0), (0, 2), (0, -2)] {
                    dot(x + dx, y + dy, colour, alpha * 0.14);
                }
            }
        }
    }
    let mut bytes = Vec::with_capacity(wi * hi * 4);
    for pixel in &rgb {
        for channel in pixel {
            bytes.push(channel.round().clamp(0.0, 255.0) as u8);
        }
        bytes.push(255);
    }
    Canvas::from_rgba(width, height, &bytes).unwrap_or_else(|| Canvas::new(width, height))
}
