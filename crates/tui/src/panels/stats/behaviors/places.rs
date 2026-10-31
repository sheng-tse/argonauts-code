//! Where the universe's bodies stand on the picture, in its own units: 0 to 1 across and down,
//! a radius as a share of the picture's height. The braille cells, the picture and what the
//! pointer finds all place a body the same way, so pointing finds what was drawn.
//!
//! The picture is the map's, 64 × 13 cells of 18 × 42 px, so a unit across is 1152 / 546 times a
//! unit down.

use super::level::Level;
use super::universe::{Body, Universe};
use crate::world::Pointed;

/// How many units down a unit across is: the picture's width over its height.
pub const ASPECT: f64 = 1152.0 / 546.0;

/// One body of the universe as drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub pointed: Pointed,
    pub kind: Kind,
    /// Its middle, across and down.
    pub at: (f64, f64),
    /// Its radius, as a share of the picture's height.
    pub radius: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// Your planet, with the faint ring round it.
    Home,
    Galaxy,
    Star,
    Planet {
        /// Its orbit's radii, across and down, in the units of [`Place::at`].
        orbit: (f64, f64),
    },
    /// The belt is its orbit.
    Belt {
        orbit: (f64, f64),
    },
}

/// Where `at` of `count` stands on an ellipse round the middle, the first at the top.
fn around(at: usize, count: usize, (rx, ry): (f64, f64)) -> (f64, f64) {
    let angle =
        -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * at as f64 / count.max(1) as f64;
    let (sin, cos) = angle.sin_cos();
    (0.5 + cos * rx, 0.5 + sin * ry)
}

fn index(at: usize) -> u32 {
    u32::try_from(at).unwrap_or(u32::MAX)
}

/// The bodies `level` draws, back to front.
pub fn places(universe: &Universe, level: Level) -> Vec<Place> {
    let mut out = Vec::new();
    match level {
        Level::Universe => {
            out.push(Place {
                pointed: Pointed::Home,
                kind: Kind::Home,
                at: (0.5, 0.5),
                radius: 0.05,
            });
            let count = universe.makers.len();
            for (at, maker) in universe.makers.iter().enumerate() {
                let share = f64::from(maker.actions) / f64::from(universe.actions.max(1));
                out.push(Place {
                    pointed: Pointed::Galaxy(index(at)),
                    kind: Kind::Galaxy,
                    at: around(at, count, (0.36, 0.35)),
                    radius: (1.5 + 4.0 * share.sqrt()) / 52.0,
                });
            }
        }
        Level::Galaxy(maker) => {
            let Some(found) = universe.makers.get(maker) else {
                return out;
            };
            let others = found.stars.len().saturating_sub(1);
            for (at, star) in found.stars.iter().enumerate() {
                let actions = universe.stars.get(*star).map_or(0, |s| s.actions);
                let share = f64::from(actions) / f64::from(found.actions.max(1));
                out.push(Place {
                    pointed: Pointed::Star(index(*star)),
                    kind: Kind::Star,
                    at: match at {
                        0 => (0.5, 0.5),
                        at => around(at - 1, others, (0.27, 0.27)),
                    },
                    radius: (1.0 + 4.0 * share.sqrt()) / 52.0,
                });
            }
        }
        Level::System(star) => {
            let Some(found) = universe.stars.get(star) else {
                return out;
            };
            // The planets and the belt go round on orbits; the moons and the flares are the
            // pictures' alone.
            let orbiting = found.bodies.iter().filter(|body| {
                matches!(
                    universe.bodies.get(**body),
                    Some(Body::Planet { .. } | Body::Belt { .. })
                )
            });
            for (k, body) in orbiting.enumerate() {
                let down = (6.0 + 3.6 * k as f64) / 52.0;
                let orbit = (down * 1.6 / ASPECT, down);
                let kind = match universe.bodies.get(*body) {
                    Some(Body::Belt { .. }) => Kind::Belt { orbit },
                    _ => Kind::Planet { orbit },
                };
                let (sin, cos) = (0.9 + 2.4 * k as f64).sin_cos();
                let at = match kind {
                    Kind::Belt { .. } => (0.5, 0.5),
                    _ => (0.5 + cos * orbit.0, 0.5 + sin * orbit.1),
                };
                out.push(Place {
                    pointed: Pointed::Body(index(*body)),
                    kind,
                    at,
                    radius: 1.2 / 52.0,
                });
            }
            out.push(Place {
                pointed: Pointed::Star(index(star)),
                kind: Kind::Star,
                at: (0.5, 0.5),
                radius: 3.5 / 52.0,
            });
        }
        Level::Map | Level::Planet => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_universe_puts_your_planet_in_the_middle_and_each_maker_round_it() {
        let universe = Universe::of(&super::super::sample::three_makers());
        let places = places(&universe, Level::Universe);
        assert_eq!(places[0].pointed, Pointed::Home);
        assert_eq!(places[0].at, (0.5, 0.5));
        assert_eq!(places.len(), 1 + universe.makers.len());
        let top = places[1];
        assert_eq!(top.pointed, Pointed::Galaxy(0));
        assert!(
            (top.at.0 - 0.5).abs() < 1e-9 && top.at.1 < 0.5,
            "the biggest at the top"
        );
        assert!(places[1].radius > places[2].radius, "as large as its share");
    }

    #[test]
    fn a_system_draws_its_bodies_on_their_orbits_and_its_star_last() {
        let universe = Universe::of(&super::super::sample::three_makers());
        let places = places(&universe, Level::System(0));
        assert_eq!(places.last().map(|p| p.pointed), Some(Pointed::Star(0)));
        let belt = places
            .iter()
            .find(|p| matches!(p.kind, Kind::Belt { .. }))
            .expect("Haiku has more behaviors than planets");
        let Pointed::Body(at) = belt.pointed else {
            panic!("the belt is a body: {:?}", belt.pointed);
        };
        assert_eq!(universe.body(at), Some(Body::Belt { star: 0 }));
        for place in &places {
            if let Kind::Planet { orbit } = place.kind {
                let (dx, dy) = ((place.at.0 - 0.5) / orbit.0, (place.at.1 - 0.5) / orbit.1);
                assert!((dx * dx + dy * dy - 1.0).abs() < 1e-9, "on its orbit");
            }
        }
    }
}
