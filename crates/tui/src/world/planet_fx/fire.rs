//! Smoke and fire on the globe: a volcano's plume where something failed this week, and a
//! wildfire's flickering front where work was dropped.

use std::f64::consts::TAU;

use super::{GlobeGround, Kind, Motion, Weather};
use crate::world::raster::noise::{hash3_wrapping, mix, smooth};
use crate::world::raster::{Blend, Brush, Canvas, Colour, Gradient, Path};

const BIG: f64 = 120.0;

/// A volcano. Only one that failed this week smokes: a puff every half second from the crater,
/// rising and drifting downwind as it grows and fades.
#[derive(Debug, Clone)]
pub struct Volcano {
    pub at: (f64, f64),
    /// Its radius on the map, in pixels.
    pub radius: f64,
    pub smoking: bool,
    /// The way the wind blows on the map, a unit vector.
    pub wind: (f64, f64),
}

impl Weather for Volcano {
    fn kind(&self) -> Kind {
        Kind::Volcano
    }

    fn place(&self) -> (f64, f64) {
        self.at
    }

    fn motion(&self) -> Motion {
        Motion::SMOKE
    }

    fn moves_on(&self, radius: f64) -> bool {
        self.smoking && radius >= BIG
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        if !self.smoking {
            return;
        }
        let Some(spot) = ground.at(self.at.0, self.at.1) else {
            return;
        };
        if self.radius * 2.6 * spot.s < Kind::Volcano.reads() * 3.0 {
            return;
        }
        let plume = Plume {
            from: self.at,
            radius: self.radius,
            puffs: 8,
            reach: self.radius * 3.4,
            ash: false,
            wind: self.wind,
        };
        plume.draw(canvas, ground, t, still);
    }
}

/// A wildfire burning this week: its front flickers, each flame at its own pace, and its smoke
/// drifts downwind. The flames are light, so they show by day and glow by night.
#[derive(Debug, Clone)]
pub struct Wildfire {
    /// Points along the burning front, on the map.
    pub front: Vec<(f64, f64)>,
    pub burning: bool,
    pub wind: (f64, f64),
}

impl Weather for Wildfire {
    fn kind(&self) -> Kind {
        Kind::Wildfire
    }

    fn place(&self) -> (f64, f64) {
        self.front
            .get(self.front.len() / 2)
            .copied()
            .unwrap_or_default()
    }

    /// The flames flicker in 1.5 s and the smoke in 3, so the whole starts over every 3.
    fn motion(&self) -> Motion {
        Motion::SMOKE
    }

    fn moves_on(&self, radius: f64) -> bool {
        self.burning && radius >= BIG
    }

    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        if !self.burning || self.front.is_empty() {
            return;
        }
        let middle = self.place();
        let Some(spot) = ground.at(middle.0, middle.1) else {
            return;
        };
        if 28.0 * spot.s < Kind::Wildfire.reads() * 2.0 {
            return;
        }
        let flicker = Motion::FIRE.seconds;
        let last = self.front.len().saturating_sub(1);
        for k in (1..last).step_by(2) {
            let (x, y) = self.front[k];
            let Some(spot) = ground.at(x, y) else {
                continue;
            };
            let kf = k as f64;
            let phase = if still {
                0.5 + 0.3 * (kf * 2.3).sin()
            } else {
                0.5 + 0.5 * ((t / flicker) * TAU * (1.0 + (k % 3) as f64) + kf * 1.7).sin()
            };
            let night = 1.0 - spot.day;
            let reach = (5.0 + 6.0 * phase) * spot.s;
            let alpha = (0.14 + 0.36 * phase) * (0.6 + 0.9 * night) * spot.shows;
            let flame = Gradient::radial(spot.x, spot.y, 0.0, spot.x, spot.y, reach)
                .stop(0.0, Colour::rgba(255, 214, 120, alpha))
                .stop(0.45, Colour::rgba(255, 120, 40, alpha * 0.6))
                .stop(1.0, Colour::rgba(255, 80, 20, 0.0));
            let mut disc = Path::new();
            disc.circle(spot.x, spot.y, reach);
            canvas.fill(&disc, &Brush::new(flame).blend(Blend::Lighter));
        }
        let plume = Plume {
            from: middle,
            radius: 24.0,
            puffs: 10,
            reach: 72.0,
            ash: true,
            wind: self.wind,
        };
        plume.draw(canvas, ground, t, still);
    }
}

/// Smoke rising from a place and drifting downwind, seen from above as a streak blown along
/// the wind, its rise a lean away from the globe's middle.
struct Plume {
    from: (f64, f64),
    radius: f64,
    puffs: u32,
    reach: f64,
    /// A fire's grey ash rather than a volcano's dark smoke turning pale.
    ash: bool,
    wind: (f64, f64),
}

impl Plume {
    fn draw(&self, canvas: &mut Canvas, ground: &GlobeGround, t: f64, still: bool) {
        let loop_seconds = Motion::SMOKE.seconds;
        let (wx, wy) = self.wind;
        let puffs = f64::from(self.puffs);
        for k in 0..self.puffs {
            let kf = f64::from(k);
            let u = if still {
                (kf + 1.0) / (puffs + 1.0)
            } else {
                (t / loop_seconds + kf / puffs).rem_euclid(1.0)
            };
            // The plume fans out as it goes.
            let side =
                (hash3_wrapping(kf, puffs, self.radius.trunc()) - 0.5) * u * self.reach * 0.4;
            let (x, y) = (
                self.from.0 + wx * u * self.reach - wy * side,
                self.from.1 + wy * u * self.reach + wx * side,
            );
            let Some(spot) = ground.at(x, y) else {
                continue;
            };
            let puff = self.radius * (0.22 + 0.6 * u) * spot.s;
            if puff < Kind::Volcano.reads() * 0.5 {
                continue;
            }
            let (ux, uy) = ground.up(&spot, u * self.radius * 1.8);
            let strength = if self.ash {
                0.36 * (1.0 - 0.5 * u)
            } else {
                0.8 * (1.0 - u).powf(0.85)
            } * if still { 1.0 } else { smooth(0.0, 0.08, u) }
                * (1.0 - smooth(0.85, 1.0, u))
                * spot.shows;
            let (px, py) = (spot.x + ux, spot.y + uy);
            let light = spot.cloud_light.clamp(0.04, 1.1);
            // Dark with ash where it leaves the ground, paler as it spreads: smoke, not one more
            // white cloud.
            let first = if self.ash {
                [
                    mix(120.0, 190.0, u),
                    mix(100.0, 186.0, u),
                    mix(86.0, 182.0, u),
                ]
            } else {
                let s = smooth(0.0, 0.55, u);
                [
                    mix(132.0, 230.0, s),
                    mix(122.0, 226.0, s),
                    mix(116.0, 222.0, s),
                ]
            };
            let lit = |c: f64| (c * light).trunc().clamp(0.0, 255.0) as u8;
            let smoke = Gradient::radial(px - puff * 0.25, py - puff * 0.3, 0.0, px, py, puff)
                .stop(
                    0.0,
                    Colour::rgba(lit(first[0]), lit(first[1]), lit(first[2]), strength),
                )
                .stop(
                    0.6,
                    Colour::rgba(lit(150.0), lit(146.0), lit(144.0), strength * 0.7),
                )
                .stop(1.0, Colour::rgba(120, 118, 118, 0.0));
            let mut disc = Path::new();
            disc.circle(px, py, puff);
            canvas.fill(&disc, &Brush::new(smoke));
        }
    }
}
