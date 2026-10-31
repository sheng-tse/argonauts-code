//! The universe as the tab steps through it: each maker a galaxy, most used first; in a galaxy
//! each of its models a star, most used first; and round a star its busiest behaviors as planets
//! with their finer ones as moons, the rest of its actions as a belt, and a flare for each
//! session its CLI failed in. Between the galaxies fly the comets of runs that mixed makers, and
//! tidal streams join makers whose models work together. Everything is numbered as the world's
//! universe numbers it, so the [`Pointed`] marks its pictures carry name the same things here.

use argonauts_core::behaviors::behavior_world::WorldFacts;

use super::level::Level;
use crate::world::Pointed;
use crate::world::cosmos;

/// A star's planets: its six busiest behaviors.
pub const PLANETS: usize = 6;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Universe {
    pub makers: Vec<Maker>,
    pub stars: Vec<Star>,
    pub bodies: Vec<Body>,
    /// Every model's actions together.
    pub actions: u32,
}

/// A maker's galaxy.
#[derive(Debug, Clone, PartialEq)]
pub struct Maker {
    /// As the registry names providers, such as `anthropic`.
    pub key: String,
    pub actions: u32,
    /// Its models' stars, most used first.
    pub stars: Vec<usize>,
}

/// A model's star.
#[derive(Debug, Clone, PartialEq)]
pub struct Star {
    /// The model's index among the world's.
    pub model: usize,
    pub maker: usize,
    pub actions: u32,
    /// Every body round it: its planets, its belt, its moons and its flares.
    pub bodies: Vec<usize>,
    /// The star it goes round with in a binary, by its index among the stars.
    pub binary: Option<usize>,
    /// It sits in a supernova's remnant: its CLI failed in 3 or more sessions in one day.
    pub supernova: bool,
}

/// Something else the pointer can find in the universe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// One of its model's busiest behaviors, by its index among the model's.
    Planet { star: usize, behavior: usize },
    /// A finer behavior going round the planet of `planet`, both by their index among the
    /// model's behaviors.
    Moon {
        star: usize,
        behavior: usize,
        planet: usize,
    },
    /// The rest of its model's actions.
    Belt { star: usize },
    /// A session its CLI failed in, by its index among the model's failures.
    Flare { star: usize, failure: usize },
    /// A run that mixed makers, by its index among the world's, flying from one star to
    /// another.
    Comet { run: usize, from: usize, to: usize },
    /// Two makers' models that share runs far more often than chance, by the index of their
    /// pair among the world's.
    Stream { pair: usize },
}

impl Universe {
    /// The universe the world's facts make, numbered as the world draws it.
    pub fn of(world: &WorldFacts) -> Self {
        Self::from(&cosmos::Universe::new(world, &[], Vec::new()))
    }

    pub(super) fn from(drawn: &cosmos::Universe) -> Self {
        let stars: Vec<Star> = drawn
            .stars
            .iter()
            .map(|star| Star {
                model: star.model,
                maker: star.maker,
                actions: star.actions,
                bodies: star.bodies.clone(),
                binary: star.binary.map(|binary| binary.with),
                supernova: star.supernova.is_some(),
            })
            .collect();
        let planet = |star: usize, planet: usize| drawn.stars.get(star)?.system.planets.get(planet);
        let bodies = drawn
            .bodies
            .iter()
            .filter_map(|body| {
                Some(match *body {
                    cosmos::Body::Planet { star, planet: at } => Body::Planet {
                        star,
                        behavior: planet(star, at)?.behavior,
                    },
                    cosmos::Body::Moon {
                        star,
                        planet: at,
                        moon,
                    } => {
                        let planet = planet(star, at)?;
                        Body::Moon {
                            star,
                            behavior: planet.moons.get(moon)?.behavior,
                            planet: planet.behavior,
                        }
                    }
                    cosmos::Body::Belt { star } => Body::Belt { star },
                    cosmos::Body::Flare { star, flare } => Body::Flare {
                        star,
                        failure: *drawn.stars.get(star)?.system.flares.get(flare)?,
                    },
                    cosmos::Body::Comet { comet } => {
                        let comet = drawn.comets.get(comet)?;
                        Body::Comet {
                            run: comet.run,
                            from: comet.from,
                            to: comet.to,
                        }
                    }
                    cosmos::Body::Stream { stream } => Body::Stream {
                        pair: drawn.streams.get(stream)?.pair,
                    },
                })
            })
            .collect::<Vec<_>>();
        Self {
            makers: drawn
                .makers
                .iter()
                .map(|maker| Maker {
                    key: maker.key.clone(),
                    actions: maker.actions,
                    stars: maker.stars.clone(),
                })
                .collect(),
            stars,
            // A body the world drew that names nothing it knows is left out, and so is every
            // body after it, so the numbers never slip.
            bodies: if bodies.len() == drawn.bodies.len() {
                bodies
            } else {
                Vec::new()
            },
            actions: drawn.total,
        }
    }

    /// What ← → steps through on `level`, in order: the galaxies biggest first and then your
    /// planet; a galaxy's stars; a system's star, its planets and its belt.
    pub fn picks(&self, level: Level) -> Vec<Pointed> {
        let index = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        match level {
            Level::Universe => (0..self.makers.len())
                .map(|at| Pointed::Galaxy(index(at)))
                .chain([Pointed::Home])
                .collect(),
            Level::Galaxy(maker) => self.makers.get(maker).map_or_else(Vec::new, |m| {
                m.stars.iter().map(|s| Pointed::Star(index(*s))).collect()
            }),
            Level::System(star) => self.stars.get(star).map_or_else(Vec::new, |s| {
                // ← → step through the star, its planets and its belt; the moons and the
                // flares answer the pointer.
                let listed = |body: &&usize| {
                    matches!(
                        self.bodies.get(**body),
                        Some(Body::Planet { .. } | Body::Belt { .. })
                    )
                };
                std::iter::once(Pointed::Star(index(star)))
                    .chain(
                        s.bodies
                            .iter()
                            .filter(listed)
                            .map(|b| Pointed::Body(index(*b))),
                    )
                    .collect()
            }),
            Level::Map | Level::Planet => Vec::new(),
        }
    }

    /// The level Enter or + goes to from `level` with `picked` picked, if any.
    pub fn inside(&self, level: Level, picked: Pointed) -> Option<Level> {
        let at = |index: u32| usize::try_from(index).ok();
        match (level, picked) {
            (Level::Universe, Pointed::Home) => Some(Level::Planet),
            (Level::Universe, Pointed::Galaxy(maker)) => at(maker)
                .filter(|m| *m < self.makers.len())
                .map(Level::Galaxy),
            (Level::Universe, Pointed::Star(star)) => {
                Some(Level::Galaxy(self.stars.get(at(star)?)?.maker))
            }
            (Level::Galaxy(_), Pointed::Star(star)) => at(star)
                .filter(|s| *s < self.stars.len())
                .map(Level::System),
            _ => None,
        }
    }

    /// The level − or Esc goes back out to from inside the universe, and what is picked there:
    /// the galaxy or star it came from.
    pub fn out_of(&self, level: Level) -> Option<(Level, Pointed)> {
        let index = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        match level {
            Level::System(star) => {
                let maker = self.stars.get(star)?.maker;
                Some((Level::Galaxy(maker), Pointed::Star(index(star))))
            }
            Level::Galaxy(maker) => Some((Level::Universe, Pointed::Galaxy(index(maker)))),
            _ => None,
        }
    }

    pub fn star(&self, at: u32) -> Option<&Star> {
        self.stars.get(usize::try_from(at).ok()?)
    }

    pub fn maker(&self, at: u32) -> Option<&Maker> {
        self.makers.get(usize::try_from(at).ok()?)
    }

    pub fn body(&self, at: u32) -> Option<Body> {
        self.bodies.get(usize::try_from(at).ok()?).copied()
    }
}
