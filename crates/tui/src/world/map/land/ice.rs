//! The ice at the poles, the same on the map and on the planet, since the map is the planet
//! unrolled. The north cap reaches further the more days since the crew's last action, the south
//! ice the more quiet days in the period, and an iceberg floats off the south ice for each quiet
//! stretch. Both stay clear of the land, which reaches about 60° north and south.

use std::f64::consts::{PI, TAU};

use crate::world::map::fields::{Ground, Look, Rgb, mix_rgb};
use crate::world::raster::noise::{fbm3, hash3, smooth, vnoise3};

/// An edge no latitude reaches, for a cap that isn't there.
const NO_EDGE: f64 = 9.0;

/// How far the ice reaches from each pole, and the icebergs off the south ice.
#[derive(Debug, Clone, PartialEq)]
pub struct Polar {
    /// Degrees of the north cap from the pole, after 3 idle days; `None` before.
    pub cap_north: Option<f64>,
    /// Degrees of the south ice from the pole, with any quiet day; `None` without.
    pub cap_south: Option<f64>,
    pub bergs: Vec<Berg>,
}

/// An iceberg, by its quiet stretch's place in the list: where it floats and its radius, in
/// radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Berg {
    pub index: usize,
    pub lat: f64,
    pub lon: f64,
    pub radius: f64,
}

/// The ice's colour over what lay under it, whether it is solid, and on the sheet a faint
/// flow-line slope for the light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ice {
    pub colour: Rgb,
    pub solid: bool,
    pub slope: Option<f64>,
}

impl Polar {
    pub fn of(ground: &Ground) -> Self {
        let idle = ground.facts.idle_days;
        let quiet = ground.facts.quiet_days;
        let cap_north = (idle >= 3).then(|| (1.6 * f64::from(idle)).min(24.0));
        let cap_south = (quiet > 0).then(|| (10.0 + 1.5 * f64::from(quiet)).min(26.0));
        let mut polar = Self {
            cap_north,
            cap_south,
            bergs: Vec::new(),
        };
        if ground.look != Look::Gas {
            let stretches = &ground.facts.quiet_stretches;
            let edge = polar.edge_south();
            let seed = ground.seed();
            polar.bergs = stretches
                .iter()
                .enumerate()
                .map(|(index, stretch)| {
                    let k = index as f64;
                    Berg {
                        index,
                        lat: -edge + 0.035 + 0.06 * hash3(k, 3.0, 7.0, seed),
                        lon: -PI
                            + (k + 0.3 + 0.4 * hash3(k, 9.0, 2.0, seed)) * TAU
                                / stretches.len() as f64,
                        radius: 0.018 + 0.008 * f64::from(stretch.days),
                    }
                })
                .collect();
        }
        polar
    }

    /// The latitude the north cap's edge lies at, in radians.
    pub fn edge_north(&self) -> f64 {
        edge(self.cap_north)
    }

    /// How far south the south ice's edge lies, in radians.
    pub fn edge_south(&self) -> f64 {
        edge(self.cap_south)
    }

    /// The same ice without its icebergs, which come and go with nature.
    pub fn without_bergs(&self) -> Self {
        Self {
            bergs: Vec::new(),
            ..self.clone()
        }
    }

    /// Whether a latitude lies on the ice or its floes, which reach 3° out, with 1° of margin.
    pub fn covers(&self, lat: f64) -> bool {
        if lat > 0.0 {
            self.cap_north.is_some() && lat > self.edge_north() - 0.07
        } else {
            self.cap_south.is_some() && -lat > self.edge_south() - 0.07
        }
    }
}

fn edge(cap: Option<f64>) -> f64 {
    cap.map_or(NO_EDGE, |degrees| (90.0 - degrees) * PI / 180.0)
}

/// The ice at a place, over the colour `under` that lies there, or `None` away from it. `(sx,
/// sy, sz)` is the place on the unit sphere, which the ice's noise is drawn on so it joins
/// round the poles.
pub fn polar_at(
    polar: &Polar,
    lon: f64,
    lat: f64,
    sphere: (f64, f64, f64),
    under: Rgb,
    seed: f64,
) -> Option<Ice> {
    let (sx, sy, sz) = sphere;
    let mut out: Option<Ice> = None;
    let edge = if lat > 0.0 {
        polar.edge_north()
    } else {
        polar.edge_south()
    };
    // The edge is a fine ragged line round a gentle overall shape, worked out only near a cap,
    // since the noise costs as much as the rest of the place.
    let near = edge < 8.0 && lat.abs() > edge - 0.2;
    let ragged = if near {
        edge + 0.07 * fbm3(sx * 1.6, sy * 1.6, sz * 1.6, seed + 9.0, 2)
            + 0.028 * fbm3(sx * 11.0, sy * 11.0, sz * 11.0, seed + 10.0, 4)
    } else {
        NO_EDGE
    };
    let into = lat.abs() - ragged;
    if into > -0.055 && edge < 8.0 {
        // Sea ice first: a narrow ragged fringe of floes with dark water between them.
        let far = (-into / 0.055).clamp(0.0, 1.0);
        let cell = fbm3(sx * 46.0, sy * 46.0, sz * 46.0, seed + 71.0, 3);
        let cover =
            smooth(0.42, 0.56, (1.0 - far) * 0.85 + 0.55 * cell) * (1.0 - smooth(0.7, 1.0, far));
        let floes = mix_rgb(
            [176.0, 192.0, 210.0],
            [228.0, 234.0, 242.0],
            smooth(0.5, 1.0, cell + 0.5 * (1.0 - far)),
        );
        let mut ice = Ice {
            colour: mix_rgb(under, floes, cover * 0.94),
            solid: cover > 0.6,
            slope: None,
        };
        // Then the sheet over it: smooth and bright, faint blue flow lines toward its edge.
        if into > -0.004 {
            let streak = fbm3(sx * 4.0, sy * 4.0, sz * 22.0, seed + 61.0, 3);
            let crack = smooth(
                0.25,
                0.5,
                fbm3(sx * 14.0, sy * 14.0, sz * 14.0, seed + 63.0, 3),
            );
            let sheet = mix_rgb(
                mix_rgb(
                    [214.0, 224.0, 236.0],
                    [244.0, 247.0, 251.0],
                    smooth(0.0, 0.1, into),
                ),
                [178.0, 198.0, 222.0],
                (0.35 + streak).clamp(0.0, 1.0) * 0.16 + crack * 0.08,
            );
            ice.colour = mix_rgb(ice.colour, sheet, smooth(-0.004, 0.004, into));
            if into > 0.0 {
                ice.solid = true;
                ice.slope = Some(0.035 * streak);
            }
        }
        out = Some(ice);
    }
    for berg in &polar.bergs {
        if (lat - berg.lat).abs() > berg.radius * 3.0 {
            continue;
        }
        let dl = (lat - berg.lat) / berg.radius;
        let dn = (lon - berg.lon).sin().atan2((lon - berg.lon).cos()) * lat.cos() / berg.radius;
        let angle = dl.atan2(dn);
        let round = dl.hypot(dn * 0.7)
            / (1.0
                + 0.3
                    * vnoise3(
                        angle.cos() * 1.8,
                        angle.sin() * 1.8,
                        berg.index as f64,
                        31.0,
                    ));
        if round < 2.2 {
            let base = out.map_or(under, |ice| ice.colour);
            out = Some(if round < 1.0 {
                Ice {
                    colour: mix_rgb(
                        [252.0, 253.0, 255.0],
                        [190.0, 210.0, 228.0],
                        smooth(-0.4, 0.7, dn * 0.6 - dl * 0.6),
                    ),
                    solid: true,
                    slope: Some(0.0),
                }
            } else {
                let colour = if round < 1.12 {
                    mix_rgb(base, [150.0, 176.0, 196.0], 0.8)
                } else {
                    mix_rgb(base, [96.0, 176.0, 206.0], (2.2 - round) / 1.08 * 0.5)
                };
                match out {
                    Some(ice) => Ice { colour, ..ice },
                    None => Ice {
                        colour,
                        solid: false,
                        slope: None,
                    },
                }
            });
        }
    }
    out
}
