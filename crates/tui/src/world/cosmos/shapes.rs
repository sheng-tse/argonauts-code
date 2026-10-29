//! Where everything sits. In a galaxy's own plane, radius 1: its biggest model at the core, each
//! other model further out the less its behaviors look like its maker's whole mix, and each
//! action on its model's arm, further from its star the older it is, so this week's work gathers
//! round its star and old work trails along the arms. In the universe: each galaxy round your
//! planet, nearer the faint ring the more its maker's mix is like your whole mix.

use std::collections::BTreeMap;
use std::f64::consts::{PI, TAU};

use argonauts_core::behavior_world::ModelFacts;

use super::model::{Universe, rng_for};

/// The universe's sizes are tuned for a picture this big, 64 by 13 cells of 18 by 42 pixels,
/// and scale with it.
pub(super) const DRAWN: (f64, f64) = (1152.0, 546.0);
/// Your planet's radius in the universe, at the drawn size.
pub(super) const HOME_RADIUS: f64 = 46.0;
/// A galaxy's size is read as if the crew had done this many actions in all, so it keeps its
/// size by share however many actions there were.
pub(super) const SHARE_SCALE: f64 = 1000.0;
/// How steeply a galaxy's arms wind.
const PITCH: f64 = 0.52;

/// A galaxy's own plane, and where its maker's actions lie in it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct GalaxyShape {
    /// How unlike your whole mix its maker's mix is, from 0 for the same.
    pub apart: f64,
    /// How flat it lies: 1 seen face on.
    pub tilt: f64,
    /// How far it is turned in the universe.
    pub turn: f64,
    pub points: Vec<Point>,
}

/// Where a model's star sits in its galaxy's plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StarPlace {
    pub rad: f64,
    pub th: f64,
    pub arm: u8,
    /// The bigger star of a binary, which this one rides round.
    pub companion_of: Option<usize>,
}

/// One action on its model's arm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Point {
    /// Its index among the universe's actions.
    pub action: usize,
    pub star: usize,
    pub rad: f64,
    pub th: f64,
    /// How bright it shows, from 0.45 to 1.
    pub bright: f64,
    /// In the last 7 days.
    pub fresh: bool,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Shapes {
    pub galaxies: Vec<GalaxyShape>,
    pub stars: Vec<StarPlace>,
}

/// A behavior mix: each behavior's share of some actions.
type Mix<'a> = BTreeMap<&'a str, f64>;

fn mix_of<'a>(models: impl Iterator<Item = &'a ModelFacts>) -> Mix<'a> {
    let mut counts: Mix<'a> = BTreeMap::new();
    let mut total = 0.0;
    for model in models {
        for behavior in &model.behaviors {
            *counts.entry(behavior.behavior.as_str()).or_default() += f64::from(behavior.count);
            total += f64::from(behavior.count);
        }
    }
    let total = if total > 0.0 { total } else { 1.0 };
    counts.values_mut().for_each(|share| *share /= total);
    counts
}

/// How alike two mixes are, from 0 to 1.
fn cosine(a: &Mix<'_>, b: &Mix<'_>) -> f64 {
    let dot: f64 = a
        .iter()
        .map(|(k, v)| v * b.get(k).copied().unwrap_or(0.0))
        .sum();
    let norms = a.values().map(|v| v * v).sum::<f64>() * b.values().map(|v| v * v).sum::<f64>();
    dot / if norms > 0.0 { norms.sqrt() } else { 1.0 }
}

impl Shapes {
    pub fn new(universe: &Universe, models: &[ModelFacts]) -> Self {
        let all = mix_of(models.iter());
        let mut stars = vec![
            StarPlace {
                rad: 0.08,
                th: 0.0,
                arm: 0,
                companion_of: None,
            };
            universe.stars.len()
        ];
        let mut galaxies = Vec::with_capacity(universe.makers.len());
        let mut by_star: Vec<Vec<usize>> = vec![Vec::new(); universe.stars.len()];
        for (at, action) in universe.actions.iter().enumerate() {
            if let Some(star) = universe.star_of_model(action.model) {
                by_star[star].push(at);
            }
        }
        for maker in &universe.makers {
            let biggest = maker
                .stars
                .first()
                .map_or("", |s| models[universe.stars[*s].model].model.as_str());
            let mut rng = rng_for(&format!("{}:{biggest}", maker.key), 7);
            let mine = mix_of(
                maker
                    .stars
                    .iter()
                    .map(|s| &models[universe.stars[*s].model]),
            );
            let apart = 1.0 - cosine(&mine, &all);
            let tilt = if maker.dwarf {
                0.75
            } else {
                0.45 + 0.2 * rng.next_f64()
            };
            let turn = (rng.next_f64() - 0.5) * 1.1;
            // The rest in order of how far their work is from the maker's mix, half by that
            // order and half by the difference itself, so no two stars share a place; neighbours
            // take turns by arm.
            let mut rest: Vec<(usize, f64)> = maker
                .stars
                .iter()
                .skip(1)
                .map(|s| {
                    let own = mix_of(std::iter::once(&models[universe.stars[*s].model]));
                    (*s, 1.0 - cosine(&own, &mine))
                })
                .collect();
            rest.sort_by(|a, b| a.1.total_cmp(&b.1));
            let low = rest.first().map_or(0.0, |r| r.1);
            let high = rest.last().map_or(0.0, |r| r.1);
            for (k, (star, own)) in rest.iter().enumerate() {
                let order = if rest.len() > 1 {
                    k as f64 / (rest.len() - 1) as f64
                } else {
                    0.5
                };
                let unlike = if high > low {
                    (own - low) / (high - low)
                } else {
                    0.5
                };
                let shrink = if maker.dwarf { 0.6 } else { 1.0 };
                stars[*star].rad = (0.3 + 0.55 * (0.5 * order + 0.5 * unlike)) * shrink;
                stars[*star].arm = ((k + 1) % 2) as u8;
            }
            for star in &maker.stars {
                let place = &mut stars[*star];
                place.th = if maker.dwarf {
                    rng.next_f64() * TAU
                } else {
                    f64::from(place.arm) * PI + (place.rad / 0.08 + 1.0).ln() / PITCH.tan()
                };
                if let Some(binary) = universe.stars[*star].binary
                    && binary.companion
                {
                    place.companion_of = Some(binary.with);
                }
            }
            let mut points = Vec::new();
            for star in &maker.stars {
                let base = stars[*star]
                    .companion_of
                    .map_or(stars[*star], |big| stars[big]);
                for &at in &by_star[*star] {
                    let action = &universe.actions[at];
                    let age = (f64::from(action.day) / f64::from(universe.days)).clamp(0.0, 1.0);
                    let (rad, th) = if maker.dwarf {
                        let rad = rng.normal().abs() * 0.3 * (0.4 + age);
                        (rad, rng.next_f64() * TAU)
                    } else {
                        let out = if base.rad < 0.15 || rng.next_f64() < 0.5 {
                            1.0
                        } else {
                            -1.0
                        };
                        let rad = (base.rad
                            + out * age * (0.25 + 0.35 * rng.next_f64())
                            + rng.normal() * 0.03)
                            .clamp(0.03, 1.08);
                        let arm = if rng.next_f64() < 0.8 {
                            base.arm
                        } else {
                            1 - base.arm
                        };
                        let th = f64::from(arm) * PI
                            + (rad / 0.08 + 1.0).ln() / PITCH.tan()
                            + rng.normal() * (0.05 + 0.22 * age) / rad.max(0.25);
                        (rad, th)
                    };
                    points.push(Point {
                        action: at,
                        star: *star,
                        rad,
                        th,
                        bright: 0.45 + 0.55 * rng.next_f64(),
                        fresh: action.day < 7,
                        failed: action.failed,
                    });
                }
            }
            galaxies.push(GalaxyShape {
                apart,
                tilt,
                turn,
                points,
            });
        }
        Shapes { galaxies, stars }
    }
}

/// A galaxy as one picture places it: its centre, its radius in pixels and how far it is
/// turned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Frame {
    pub x: f64,
    pub y: f64,
    pub r: f64,
    pub turn: f64,
}

impl Frame {
    /// Where a point of the galaxy's plane lands in the picture.
    pub fn point(&self, tilt: f64, rad: f64, th: f64) -> (f64, f64) {
        let (x, y) = (th.cos() * rad * self.r, th.sin() * rad * self.r * tilt);
        let (sin, cos) = self.turn.sin_cos();
        (self.x + x * cos - y * sin, self.y + x * sin + y * cos)
    }

    /// Half its width and height as its disc shows, for keeping galaxies apart.
    fn extent(&self, tilt: f64) -> (f64, f64) {
        let (sin, cos) = self.turn.sin_cos();
        (
            self.r * 0.85 * cos.hypot(tilt * sin),
            self.r * 0.85 * sin.hypot(tilt * cos).max(0.35),
        )
    }
}

/// How much smaller a picture is than the drawn size, the smaller way.
pub(super) fn scale(width: f64, height: f64) -> f64 {
    (width / DRAWN.0).min(height / DRAWN.1 * 1.4)
}

/// Your planet's radius in the universe on a picture this big.
pub fn home_radius(width: f64, height: f64) -> f64 {
    HOME_RADIUS * scale(width, height)
}

/// The radius a galaxy of this share draws at, at the drawn size.
pub(super) fn galaxy_radius(share: f64, dwarf: bool) -> f64 {
    let actions = share * SHARE_SCALE;
    if dwarf {
        20.0 + 3.0 * actions.sqrt()
    } else {
        12.0 + 6.6 * actions.sqrt()
    }
}

/// Each galaxy's frame in the universe on a picture this big: round your planet, nearer the
/// faint ring the more like your whole mix, then pushed apart from each other and from it.
pub(super) fn universe_layout(
    universe: &Universe,
    shapes: &Shapes,
    width: f64,
    height: f64,
) -> Vec<Frame> {
    let (kx, ky) = (width / DRAWN.0, height / DRAWN.1);
    let (cx, cy) = (width / 2.0, height / 2.0);
    let count = universe.makers.len().max(1) as f64;
    let mut out: Vec<Frame> = universe
        .makers
        .iter()
        .zip(&shapes.galaxies)
        .enumerate()
        .map(|(at, (maker, shape))| {
            let angle = PI + at as f64 * TAU / count + if at > 0 { 0.35 } else { 0.0 };
            let distance = (0.55 + 2.2 * shape.apart).clamp(0.55, 1.0);
            Frame {
                x: cx + angle.cos() * 430.0 * kx * distance,
                y: cy + angle.sin() * 190.0 * ky * distance,
                r: galaxy_radius(maker.share, maker.dwarf) * kx.min(ky * 1.4),
                turn: shape.turn,
            }
        })
        .collect();
    let tilts: Vec<f64> = shapes.galaxies.iter().map(|g| g.tilt).collect();
    let home = HOME_RADIUS;
    for _ in 0..240 {
        for i in 0..out.len() {
            for j in i + 1..out.len() {
                let (a, b) = (out[i], out[j]);
                let ((ax, ay), (bx, by)) = (a.extent(tilts[i]), b.extent(tilts[j]));
                let dx = (b.x - a.x) / (ax + bx + 14.0);
                let dy = (b.y - a.y) / (ay + by + 14.0);
                let d = dx.hypot(dy);
                if d < 1.0 {
                    let push = (1.0 - d) * 6.0;
                    let unit = if d > 0.0 { d } else { 1.0 };
                    let (ux, uy) = (dx / unit, dy / unit);
                    let weight = b.r / (a.r + b.r);
                    out[i].x -= ux * push * weight;
                    out[i].y -= uy * push * weight * 0.5;
                    out[j].x += ux * push * (1.0 - weight);
                    out[j].y += uy * push * (1.0 - weight) * 0.5;
                }
            }
        }
        for (at, frame) in out.iter_mut().enumerate() {
            let (ax, ay) = frame.extent(tilts[at]);
            let dx = (frame.x - cx) / (ax + home + 50.0);
            let dy = (frame.y - cy) / (ay + home + 34.0);
            let d = dx.hypot(dy);
            if d < 1.0 {
                let unit = if d > 0.0 { d } else { 1.0 };
                frame.x += dx / unit * (1.0 - d) * 8.0;
                frame.y += dy / unit * (1.0 - d) * 4.0;
            }
            frame.x = frame.x.clamp(ax + 12.0, (width - ax - 12.0).max(ax + 12.0));
            frame.y = frame
                .y
                .clamp(ay + 10.0, (height - ay - 46.0).max(ay + 10.0));
        }
    }
    out
}

/// A galaxy at its own level: the same galaxy, laid nearly flat across the picture.
pub(super) fn galaxy_frame(shape: &GalaxyShape, dwarf: bool, width: f64, height: f64) -> Frame {
    let level = shape.turn.clamp(-0.3, 0.3);
    let ex = level.cos().hypot(shape.tilt * level.sin());
    let ey = level.sin().hypot(shape.tilt * level.cos());
    let r = ((width / 2.0 - 40.0) / (1.02 * ex)).min((height / 2.0 - 22.0) / (1.02 * ey))
        * if dwarf { 0.62 } else { 1.0 };
    Frame {
        x: width / 2.0,
        y: height / 2.0,
        r,
        turn: level,
    }
}

/// A model's system on a picture this big: its star in the middle, its planets' orbits from
/// the star out to its belt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SystemFrame {
    pub cx: f64,
    pub cy: f64,
    /// The star's radius.
    pub star: f64,
    pub tilt: f64,
    /// The belt's inner and outer orbit.
    pub belt: (f64, f64),
    first: f64,
    last: f64,
    planets: usize,
}

impl SystemFrame {
    /// How flat the system lies.
    pub const TILT: f64 = 0.34;

    pub fn new(actions: u32, planets: usize, belt: bool, width: f64, height: f64) -> Self {
        let star = 26.0 + 8.0 * (1.0 + f64::from(actions) / 8.0).log2();
        let tilt = Self::TILT;
        let (cx, cy) = (width / 2.0, height / 2.0 + 6.0);
        let full = (width / 2.0 - 34.0).min((height / 2.0 - 30.0) / tilt);
        let outer = full * (0.5 + 0.5 * (planets as f64 / 4.0).clamp(0.0, 1.0));
        let ring = if belt {
            (outer - 54.0 * outer / full, outer)
        } else {
            (outer, outer)
        };
        let first = star * 1.35 + 34.0;
        let last = if belt { ring.0 - 26.0 } else { outer - 20.0 };
        SystemFrame {
            cx,
            cy,
            star,
            tilt,
            belt: ring,
            first,
            last,
            planets,
        }
    }

    /// The same system with its middle moved by (dx, dy), for drawing part of it on its own.
    pub fn moved(&self, dx: f64, dy: f64) -> Self {
        SystemFrame {
            cx: self.cx + dx,
            cy: self.cy + dy,
            ..*self
        }
    }

    /// The `k`th planet's orbit, nearest the most used.
    pub fn orbit(&self, k: usize) -> f64 {
        if self.planets <= 1 {
            self.first + 30.0
        } else {
            self.first + (self.last - self.first) * k as f64 / (self.planets - 1) as f64
        }
    }

    /// Where an orbit of this radius is at angle `th`.
    pub fn at(&self, radius: f64, th: f64) -> (f64, f64) {
        (
            self.cx + th.cos() * radius,
            self.cy + th.sin() * radius * self.tilt,
        )
    }
}
