//! Water that moves on the globe: light running down rivers and along currents the way the
//! agents moved, and the tsunami's one great wave rolling in to its coast.

use std::f64::consts::TAU;
use std::sync::Arc;

use super::{GlobeGround, Kind, Motion, Spot, Weather};
use crate::world::raster::noise::{hash3_wrapping, mix, smooth};
use crate::world::raster::{Brush, Canvas, Colour, Join, Path, Pen};

const BIG: f64 = 120.0;

/// How fast a river's or a current's light runs, in the map's pixels a second, and how far
/// apart its flecks are.
const RIVER_RUN: (f64, f64) = (12.0, 18.0);
const CURRENT_RUN: (f64, f64) = (16.0, 24.0);

/// Light running down a river or along a current: short bright flecks as wide as the water, at
/// the map's speed and spacing in the map's pixels, so they run on the globe as on the map. The
/// flecks are sunlight on the water, so the night side shows none, and nor does a held picture,
/// where the map's own arrows say the way.
#[derive(Debug, Clone)]
pub struct Flow {
    pub river: bool,
    /// The water's line on the map, the way the agents moved.
    pub path: Vec<(f64, f64)>,
    /// How wide the water is on the map, in pixels; a river widens to it toward its mouth.
    pub width: f64,
    /// For a current, whether each point of its line is at sea: its flecks keep to the sea.
    pub at_sea: Option<Vec<bool>>,
}

impl Flow {
    fn lengths(&self) -> Vec<f64> {
        let mut lengths = vec![0.0];
        for pair in self.path.windows(2) {
            let last = lengths.last().copied().unwrap_or(0.0);
            lengths.push(last + (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1));
        }
        lengths
    }
}

impl Weather for Flow {
    fn kind(&self) -> Kind {
        Kind::Flow
    }

    fn place(&self) -> (f64, f64) {
        self.path
            .get(self.path.len() / 2)
            .copied()
            .unwrap_or_default()
    }

    fn motion(&self) -> Motion {
        if self.river {
            Motion::RIVER
        } else {
            Motion::CURRENT
        }
    }

    fn moves_on(&self, radius: f64) -> bool {
        radius >= BIG
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        if still || self.path.len() < 2 {
            return;
        }
        let lengths = self.lengths();
        let total = lengths.last().copied().unwrap_or(0.0);
        if total < 12.0 {
            return;
        }
        let (speed, gap) = if self.river { RIVER_RUN } else { CURRENT_RUN };
        let dash = if self.river { 2.6 } else { 4.5 };
        let colour = if self.river {
            Colour::rgba(214, 238, 252, 0.85)
        } else {
            Colour::rgba(206, 238, 250, 0.8)
        };
        let mut along = (t * speed).rem_euclid(gap);
        let mut at = 1;
        while along < total {
            while at < lengths.len() - 1 && lengths[at] < along {
                at += 1;
            }
            let fleck = along;
            along += gap;
            if let Some(sea) = &self.at_sea
                && !(sea.get(at).copied().unwrap_or(false)
                    && sea.get(at - 1).copied().unwrap_or(false))
            {
                continue;
            }
            let (a, b) = (self.path[at - 1], self.path[at]);
            let segment = match lengths[at] - lengths[at - 1] {
                length if length > 0.0 => length,
                _ => 1.0,
            };
            let u = (fleck - lengths[at - 1]) / segment;
            let through = fleck / total;
            let (x, y) = (mix(a.0, b.0, u), mix(a.1, b.1, u));
            let (dx, dy) = ((b.0 - a.0) / segment, (b.1 - a.1) / segment);
            let Some(spot) = ground.at(x, y) else {
                continue;
            };
            let width = if self.river {
                mix(0.7, self.width, through.powf(0.8)).max(0.9) * 0.6
            } else {
                self.width * 0.85
            } * spot.s;
            if width < Kind::Flow.reads() {
                continue;
            }
            let (ex, ey) = spot.along(dx * dash, dy * dash);
            let alpha = 0.75
                * smooth(0.0, 0.07, through)
                * smooth(1.0, 0.93, through)
                * spot.shows
                * spot.day;
            let mut line = Path::new();
            line.move_to(spot.x - ex, spot.y - ey);
            line.line_to(spot.x + ex, spot.y + ey);
            canvas.stroke(
                &line,
                &Pen::new(width).round(),
                &Brush::new(colour).alpha(alpha),
            );
        }
    }
}

/// One stretch of a current over the open sea: its middle line, the length along it to each
/// point, its sideways normal at each, how far it tapers in at its ends, and its band's width.
#[derive(Debug, Clone, PartialEq)]
pub struct Stretch {
    pub points: Vec<(f64, f64)>,
    pub lengths: Vec<f64>,
    pub normals: Vec<(f64, f64)>,
    pub taper: Vec<f64>,
    pub band: f64,
}

impl Stretch {
    /// A stretch along `points`, its band `band` px wide: the lengths and normals worked out,
    /// and the ends tapering in over 18 px.
    pub fn along(points: Vec<(f64, f64)>, band: f64) -> Self {
        let mut lengths = vec![0.0];
        for pair in points.windows(2) {
            let last = lengths.last().copied().unwrap_or(0.0);
            lengths.push(last + (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1));
        }
        let n = points.len();
        let normals = (0..n)
            .map(|i| {
                let (a, b) = (points[i.saturating_sub(1)], points[(i + 1).min(n - 1)]);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let length = match dx.hypot(dy) {
                    l if l > 0.0 => l,
                    _ => 1.0,
                };
                (-dy / length, dx / length)
            })
            .collect();
        let total = lengths.last().copied().unwrap_or(0.0);
        let taper = lengths
            .iter()
            .map(|at| smooth(0.0, 18.0, *at) * smooth(0.0, 18.0, total - at))
            .collect();
        Self {
            points,
            lengths,
            normals,
            taper,
            band,
        }
    }

    pub fn total(&self) -> f64 {
        self.lengths.last().copied().unwrap_or(0.0)
    }

    /// The point `along` the stretch, `track` of the band's width off its middle, how far it
    /// has tapered there, and the way the stretch runs.
    fn at(&self, along: f64, track: f64) -> (f64, f64, f64, f64, f64) {
        let mut i = 1;
        while i < self.lengths.len() - 1 && self.lengths[i] < along {
            i += 1;
        }
        let (a, b) = (self.points[i - 1], self.points[i]);
        let span = match self.lengths[i] - self.lengths[i - 1] {
            s if s > 0.0 => s,
            _ => 1.0,
        };
        let u = ((along - self.lengths[i - 1]) / span).clamp(0.0, 1.0);
        let taper = mix(self.taper[i - 1], self.taper[i], u);
        let off = self.band * track * taper;
        let nx = mix(self.normals[i - 1].0, self.normals[i].0, u);
        let ny = mix(self.normals[i - 1].1, self.normals[i].1, u);
        let length = match (b.0 - a.0).hypot(b.1 - a.1) {
            l if l > 0.0 => l,
            _ => 1.0,
        };
        (
            mix(a.0, b.0, u) + nx * off,
            mix(a.1, b.1, u) + ny * off,
            taper,
            (b.0 - a.0) / length,
            (b.1 - a.1) / length,
        )
    }
}

/// A current as the map draws it over the open sea: a band with five tracks of streaks running
/// the way the agents went, 16 px a second; held still, chevrons along its middle say the way.
#[derive(Debug, Clone)]
pub struct Current {
    pub stretches: Vec<Stretch>,
    /// Its place in nature's list and the behavior it runs to, which set each track's phase.
    pub seed: u32,
    pub to: u32,
}

impl Weather for Current {
    fn kind(&self) -> Kind {
        Kind::Flow
    }

    fn place(&self) -> (f64, f64) {
        self.stretches
            .first()
            .and_then(|stretch| stretch.points.get(stretch.points.len() / 2))
            .copied()
            .unwrap_or_default()
    }

    fn motion(&self) -> Motion {
        Motion::CURRENT
    }

    fn moves_on(&self, radius: f64) -> bool {
        radius >= BIG
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        let (speed, gap) = CURRENT_RUN;
        for stretch in &self.stretches {
            if stretch.points.len() < 2 {
                continue;
            }
            let total = stretch.total();
            if still {
                let mut along = 22.0;
                while along < total - 14.0 {
                    let (x, y, taper, ux, uy) = stretch.at(along, 0.0);
                    along += 34.0;
                    let k = stretch.band * 0.3 * taper;
                    let (Some(mid), Some(a), Some(b)) = (
                        ground.at(x, y),
                        ground.at(x - ux * k - uy * k, y - uy * k + ux * k),
                        ground.at(x - ux * k + uy * k, y - uy * k - ux * k),
                    ) else {
                        continue;
                    };
                    let mut chevron = Path::new();
                    chevron.move_to(a.x, a.y);
                    chevron.line_to(mid.x, mid.y);
                    chevron.line_to(b.x, b.y);
                    let brush =
                        Brush::new(Colour::rgb(206, 238, 248)).alpha(0.75 * mid.shows * mid.day);
                    canvas.stroke(&chevron, &Pen::new(1.4 * mid.s).round(), &brush);
                }
                continue;
            }
            // Five tracks, each its own phase, so the flecks never line up in rows.
            for (j, offset) in [-0.36, -0.12, 0.12, 0.36, 0.0].into_iter().enumerate() {
                let jf = j as f64;
                let phase = hash3_wrapping(jf, f64::from(self.seed), f64::from(self.to)) * gap;
                let mut along = (t * speed + phase).rem_euclid(gap) + jf * 5.0;
                while along < total {
                    let (x, y, taper, ux, uy) = stretch.at(along, offset);
                    let streak =
                        2.5 + 1.5 * hash3_wrapping((along / gap).floor(), jf, f64::from(self.seed));
                    along += gap;
                    if taper < 0.2 {
                        continue;
                    }
                    let alpha = 0.7 * taper * (1.0 - f64::abs(offset));
                    streak_at(canvas, ground.at(x, y), (ux, uy), streak, alpha);
                }
            }
        }
    }
}

/// One streak of a current's track at a spot, `k` of the map's pixels each way along it.
fn streak_at(canvas: &mut Canvas, spot: Option<Spot>, way: (f64, f64), k: f64, alpha: f64) {
    let Some(spot) = spot else {
        return;
    };
    let width = 1.2;
    if width * spot.s < Kind::Flow.reads() {
        return;
    }
    let (ex, ey) = spot.along(way.0 * k, way.1 * k);
    let mut line = Path::new();
    line.move_to(spot.x - ex, spot.y - ey);
    line.line_to(spot.x + ex, spot.y + ey);
    let brush = Brush::new(Colour::rgb(220, 244, 252)).alpha(alpha * spot.shows * spot.day);
    canvas.stroke(&line, &Pen::new(width * spot.s).round(), &brush);
}

/// The tsunami from above: one great wave, born out at sea and rolling in to its behavior's
/// coast every 3 seconds, as wide as the agents that took the behavior up together were many:
/// its broken white crest, the dark trough behind, foam lines in its wake, the sea drawn back
/// pale ahead of it, and white surge where it reaches the land.
#[derive(Clone)]
pub struct Tsunami {
    /// Where the wave is born and where it reaches the coast, on the map.
    pub from: (f64, f64),
    pub to: (f64, f64),
    /// The way it rolls, a unit vector.
    pub toward: (f64, f64),
    /// How wide its front is, on the map.
    pub size: f64,
    pub seed: u32,
    /// Points just off the coast it reaches, with how far each is from the run's middle, 0 to
    /// 1; `None` where no shore was found.
    pub coast: Vec<Option<(f64, f64, f64)>>,
    /// Whether a pixel of the map is at sea.
    pub at_sea: Arc<dyn Fn(f64, f64) -> bool + Send + Sync>,
}

impl std::fmt::Debug for Tsunami {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tsunami")
            .field("from", &self.from)
            .field("to", &self.to)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Tsunami {
    /// Where the wave is `u` of the way in, and how strongly it shows: born and dying softly.
    fn wave_at(&self, u: f64) -> (f64, f64, f64) {
        (
            mix(self.from.0, self.to.0, u),
            mix(self.from.1, self.to.1, u),
            smooth(0.0, 0.1, u) * (1.0 - smooth(0.94, 1.0, u)),
        )
    }
}

impl Weather for Tsunami {
    fn kind(&self) -> Kind {
        Kind::Tsunami
    }

    fn place(&self) -> (f64, f64) {
        self.to
    }

    fn motion(&self) -> Motion {
        Motion::TSUNAMI
    }

    fn moves_on(&self, radius: f64) -> bool {
        radius >= BIG
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        let u = if still {
            0.72
        } else {
            (t / Motion::TSUNAMI.seconds).rem_euclid(1.0)
        };
        let (wx, wy, life) = self.wave_at(u);
        if life < 0.02 {
            return;
        }
        let Some(here) = ground.at(wx, wy) else {
            return;
        };
        if self.size * here.s < Kind::Tsunami.reads() * 2.0 {
            return;
        }
        let scale = here.s;
        let light = here.cloud_light.clamp(0.05, 1.1);
        let tone = |c: [f64; 3], alpha: f64| {
            let lit = |v: f64| (v * light).trunc().clamp(0.0, 255.0) as u8;
            Colour::rgba(lit(c[0]), lit(c[1]), lit(c[2]), alpha)
        };
        let (ux, uy) = self.toward;
        let (px, py) = (-uy, ux);
        // A curve across the run at a point, bowed by `bow` toward the coast.
        let curve = |cx: f64, cy: f64, half: f64, bow: f64| -> Vec<Spot> {
            (0..=16)
                .filter_map(|i| {
                    let v = -1.0 + f64::from(i) / 8.0;
                    let b = bow * (1.0 - v * v);
                    ground.at(cx + px * half * v - ux * b, cy + py * half * v - uy * b)
                })
                .collect()
        };
        let alpha = life * here.shows;
        let stroke = |canvas: &mut Canvas, spots: &[Spot], colour: Colour, width: f64| {
            if spots.len() < 2 {
                return;
            }
            let mut line = Path::new();
            for (index, spot) in spots.iter().enumerate() {
                if index == 0 {
                    line.move_to(spot.x, spot.y);
                } else {
                    line.line_to(spot.x, spot.y);
                }
            }
            let pen = Pen::new(width).round().join(Join::Round);
            canvas.stroke(&line, &pen, &Brush::new(colour).alpha(alpha));
        };
        let back = smooth(0.3, 0.75, u);
        if back > 0.0 {
            for &(x, y, off) in self.coast.iter().flatten() {
                let Some(spot) = ground.at(x, y) else {
                    continue;
                };
                let mut disc = Path::new();
                disc.circle(spot.x, spot.y, 3.6 * spot.s);
                let colour = tone([160.0, 172.0, 142.0], 0.6 * back * (1.0 - 0.6 * off));
                canvas.fill(&disc, &Brush::new(colour).alpha(alpha));
            }
        }
        // The foam lines it leaves behind.
        for k in 1..=3 {
            let kf = f64::from(k);
            let (bx, by) = (wx + ux * (kf * 9.0 + 4.0), wy + uy * (kf * 9.0 + 4.0));
            if (self.at_sea)(bx, by) {
                let foam = curve(bx, by, self.size * (0.62 - 0.1 * kf), -3.0);
                let colour = tone([214.0, 236.0, 246.0], 0.55 - 0.13 * kf);
                stroke(canvas, &foam, colour, 1.3 * scale);
            }
        }
        let half = self.size * 0.8;
        let trough = curve(wx + ux * 4.0, wy + uy * 4.0, half, 6.0);
        stroke(canvas, &trough, Colour::rgba(4, 16, 30, 0.6), 6.0 * scale);
        let body = curve(wx, wy, half, 6.0);
        stroke(canvas, &body, tone([46.0, 118.0, 146.0], 0.95), 5.0 * scale);
        let crest = curve(wx - ux * 2.5, wy - uy * 2.5, half, 6.0);
        stroke(
            canvas,
            &crest,
            tone([246.0, 251.0, 253.0], 1.0),
            2.6 * scale,
        );
        let surge = if still { 0.0 } else { smooth(0.82, 0.96, u) };
        if surge <= 0.0 {
            return;
        }
        let seed = f64::from(self.seed);
        for (i, point) in self.coast.iter().enumerate() {
            let Some((x, y, off)) = *point else {
                continue;
            };
            if off > 0.65 {
                continue;
            }
            let Some(spot) = ground.at(x - ux * 3.0, y - uy * 3.0) else {
                continue;
            };
            let fi = i as f64;
            for k in 0..3 {
                let kf = f64::from(k);
                let mut drop = Path::new();
                drop.arc(
                    spot.x + (hash3_wrapping(fi, kf, seed) - 0.5) * 5.0 * scale,
                    spot.y + (hash3_wrapping(kf, fi, seed + 5.0) - 0.5) * 4.0 * scale,
                    (1.0 + 1.6 * hash3_wrapping(fi, kf + 7.0, seed) * surge) * scale,
                    0.0,
                    TAU,
                    false,
                );
                let colour = tone([244.0, 250.0, 252.0], 0.92);
                canvas.fill(&drop, &Brush::new(colour).alpha(alpha));
            }
        }
    }
}
