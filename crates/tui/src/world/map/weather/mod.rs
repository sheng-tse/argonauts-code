//! Weather over the map: storms, hurricanes, tornadoes, clouds and their shadows, and what
//! stands on the land or moves on the water.
//!
//! A [`Weather`] is worked out once for a crew's map at one height of its land: where each storm
//! flies, clear of the dots, and the pictures each mark is drawn from. Each moment of the map is
//! then two pictures over the land: what moves on the ground and the water and what stands on
//! the land, under the dots; and the earthquakes' rings and the clouds above them, over the
//! dots. The same moment is also its [`parts`], each mark alone in a box of its own, for a
//! screen that keeps pictures and moves them.
//!
//! ```no_run
//! use argonauts_core::behaviors::behavior_map::BehaviorMap;
//! use argonauts_tui::world::map::fields::{Ground, GroundFacts};
//! use argonauts_tui::world::map::land::Polar;
//! use argonauts_tui::world::map::motion::Moment;
//! use argonauts_tui::world::map::nature::{GroundMarks, Nature, NatureFacts};
//! use argonauts_tui::world::map::relief::{Light, LitGround, Projected, Relief, STANDING};
//! use argonauts_tui::world::map::weather::{FlatLayers, Keep, Weather};
//! use argonauts_tui::world::wonders;
//!
//! # let map = BehaviorMap::default();
//! let ground = Ground::grow(GroundFacts::from_map(&map, 41));
//! let facts = NatureFacts::from_map(&map, &ground);
//! let nature = Nature::find(&ground, &facts);
//! let risen = map.world.as_ref().map(|world| world.wonders.clone()).unwrap_or_default();
//! let sunk = wonders::sunk(&ground, &facts, &risen);
//! let flat = FlatLayers::new(&ground, &nature, Keep::All, sunk);
//! let placed = wonders::place(&ground, &nature, &flat, &risen);
//! let marks = GroundMarks::new(&ground, &nature, true, sunk);
//! let relief = Relief::new(&ground);
//! let polar = Polar::of(&ground);
//! let light = Light::new(&ground, &relief, STANDING);
//! let lit = LitGround::new(&ground, &relief, &light, None, marks.marks(), Some(&polar));
//! let land = Projected::new(&relief, &lit, STANDING, 2);
//! let weather = Weather::new(&ground, &relief, &nature, &land, &placed);
//! // Over the land: what lies under the dots, the dots, then what passes over them.
//! let (under, over) = (weather.under_dots(Moment::Still), weather.over_dots(Moment::Still));
//! # let _ = (under, over, weather.sprites(Moment::At(0.5), 7));
//! ```

mod clouds;
mod fire;
pub mod flat;
mod fly;
pub mod hurricane;
pub mod icon;
mod pad;
pub mod parts;
pub mod point;
mod quake;
pub mod thunder;
mod tornado;
pub mod tsunami;
pub mod volcano;
mod water;

#[cfg(test)]
mod tests;

pub use clouds::CloudShadows;
pub use flat::{FlatLayers, Keep};
pub use fly::Taken;
pub use pad::Pad;

use super::fields::Ground;
use super::land::Polar;
use super::motion::{Flashes, Moment};
use super::nature::{CurrentRun, Feature, Hurricane, Kind, Nature, current_runs, fire_front};
use super::relief::{FORESHORTEN, Projected, Relief, STANDING, sun, top_row};
use crate::world::raster::{Canvas, Grid};
use crate::world::wonders::Placed;

/// What the weather draws from, for one crew's map at one height of its land.
#[derive(Debug, Clone)]
pub struct Weather {
    /// How far the land stands, px a unit of height; 0 lies flat.
    stand: f64,
    /// The height each place stands at, which sets where it lands on the screen.
    standing: Grid,
    /// What moves on the water, in nature's order.
    water: Vec<Water>,
    /// The storms that fly, in nature's order.
    fliers: Vec<Flying>,
    /// What stands on the land, far ones first, so a nearer one stands in front.
    upright: Vec<Standing>,
    /// Each earthquake's place and how many rings it runs.
    quakes: Vec<((f64, f64), usize)>,
    clouds: Option<CloudShadows>,
    /// What the pointer finds, worked out once: all of it keeps still but the tsunami.
    marks: Vec<point::Slot>,
    /// The parts' frames drawn so far, shared by the clones of this weather.
    frames: std::sync::Arc<std::sync::Mutex<parts::Frames>>,
}

/// Something standing on the land, with its place in nature's list.
#[derive(Debug, Clone)]
enum Standing {
    Wonder(Box<StandingWonder>),
    Fire {
        feature: Feature,
        front: Vec<(f64, f64)>,
    },
    Tornado(Feature),
    Volcano {
        feature: Feature,
        cone: volcano::Cone,
        smoking: bool,
    },
}

/// A wonder standing on the land, drawn in the map's own sunlight.
#[derive(Debug, Clone)]
struct StandingWonder {
    placed: Placed,
    sprite: crate::world::wonders::Sprite,
}

impl Storm {
    fn south(&self) -> bool {
        match self {
            Storm::Hurricane(storm) => storm.south,
            Storm::Thunder { .. } => false,
        }
    }
}

impl Standing {
    /// How far down the flat map it stands, which sets what stands in front of what.
    fn depth(&self) -> f64 {
        match self {
            Standing::Tornado(feature)
            | Standing::Volcano { feature, .. }
            | Standing::Fire { feature, .. } => feature.y,
            Standing::Wonder(wonder) => wonder.placed.ground.1,
        }
    }
}

/// A storm flying over the map: what it is, its picture, and the place it flies over.
#[derive(Debug, Clone)]
struct Flying {
    /// Its place in nature's list.
    feature: usize,
    storm: Storm,
    picture: Canvas,
    /// The point of its picture that stands over its place.
    anchor: (f64, f64),
    /// The place of the flat map it flies over, moved off the dots.
    over: (f64, f64),
}

/// The sessions each thunderstorm stopped, in the order nature lists them, which the map and
/// the planet time every storm's flashes from together.
pub fn thunderstorms(nature: &Nature) -> Vec<u32> {
    nature
        .features
        .iter()
        .filter_map(|feature| match feature.kind {
            Kind::Thunderstorm { sessions } => Some(sessions),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum Storm {
    Hurricane(Hurricane),
    Thunder {
        /// When it flashes, in step with every other storm on the screen.
        flashes: Flashes,
        radius: f64,
        behavior: usize,
    },
}

/// What moves on the water.
#[derive(Debug, Clone)]
enum Water {
    River(water::OnScreen),
    Current(water::Lanes),
    Tsunami(tsunami::Tsunami),
}

/// How far a shade falls along the ground for each pixel of height, and the way it falls:
/// away from the low sun in the west-north-west.
fn shade_run() -> (f64, (f64, f64)) {
    let [x, y, z] = sun();
    let length = x.hypot(y);
    (length / z, (-x / length, -y / length))
}

impl Weather {
    /// The weather of a crew's map over its land as `land` shows it, with the wonders where
    /// they stand.
    pub fn new(
        ground: &Ground,
        relief: &Relief,
        nature: &Nature,
        land: &Projected,
        wonders: &[Placed],
    ) -> Self {
        let standing = relief.standing.clone();
        let mut weather = Self {
            stand: land.stand,
            standing,
            water: Vec::new(),
            fliers: Vec::new(),
            upright: Vec::new(),
            quakes: Vec::new(),
            clouds: nature
                .clouds
                .map(|cover| CloudShadows::new(ground, relief, land, cover)),
            marks: Vec::new(),
            frames: Default::default(),
        };
        for feature in &nature.features {
            let moving = match &feature.kind {
                Kind::River(stream) => Water::River(water::OnScreen::new(&weather, stream)),
                Kind::Current(stream) => {
                    let runs: Vec<CurrentRun> = current_runs(ground, stream);
                    Water::Current(water::Lanes {
                        from: feature.behavior,
                        to: stream.to,
                        runs,
                    })
                }
                Kind::Tsunami(wave) => {
                    Water::Tsunami(tsunami::Tsunami::new(ground, feature.behavior, wave))
                }
                Kind::Rift(rift) if rift.shaking() => {
                    weather
                        .quakes
                        .push(((feature.x, feature.y), rift.recent.len()));
                    continue;
                }
                _ => continue,
            };
            weather.water.push(moving);
        }
        for feature in &nature.features {
            let standing = match feature.kind {
                Kind::Tornado { .. } => Standing::Tornado(feature.clone()),
                Kind::Volcano { smoking, .. } => Standing::Volcano {
                    feature: feature.clone(),
                    cone: volcano::cone(feature.radius, smoking, feature.behavior as f64),
                    smoking,
                },
                Kind::Wildfire { burning: true, .. } => Standing::Fire {
                    feature: feature.clone(),
                    front: fire_front(ground, feature),
                },
                _ => continue,
            };
            weather.upright.push(standing);
        }
        for placed in wonders {
            let sprite = crate::world::wonders::sprite(placed.wonder, placed.size, sun());
            weather
                .upright
                .push(Standing::Wonder(Box::new(StandingWonder {
                    placed: placed.clone(),
                    sprite,
                })));
        }
        weather
            .upright
            .sort_by(|a, b| a.depth().total_cmp(&b.depth()));
        weather.fly(ground, nature, wonders);
        weather.marks = weather.slots(ground, nature, wonders);
        weather
    }

    /// Moves each storm off the dots and off what lies flat on the ground, in nature's order.
    /// The storms are placed as the land stands, so they keep their places when it lies flat.
    fn fly(&mut self, ground: &Ground, nature: &Nature, wonders: &[Placed]) {
        let polar = Polar::of(ground);
        let dots: Vec<(f32, f32)> = ground
            .dots
            .iter()
            .map(|&(x, y)| {
                let (sx, sy) = self.standing_screen(f64::from(x), f64::from(y), 0.0);
                (sx as f32, sy as f32)
            })
            .collect();
        let buckets = fly::DotBuckets::new(&dots);
        let mut taken: Vec<Taken> = wonders
            .iter()
            .map(|placed| {
                let box_ = self.box_at(placed, Self::standing_screen);
                Taken {
                    cost: 4000.0,
                    ..box_
                }
            })
            .collect();
        taken.extend(
            nature
                .features
                .iter()
                .filter(|feature| feature.is_flat())
                .map(|feature| {
                    let (x, y) = self.standing_screen(feature.x, feature.y, 0.0);
                    let r = if feature.radius == 0.0 {
                        12.0
                    } else {
                        feature.radius
                    } * 1.3;
                    Taken {
                        x0: x - r,
                        y0: y - r,
                        x1: x + r,
                        y1: y + r,
                        cost: 600.0,
                    }
                }),
        );
        let mut lightning = Flashes::for_storms(&thunderstorms(nature)).into_iter();
        for (index, feature) in nature.features.iter().enumerate() {
            let (storm, picture, anchor, altitude, hurricane) = match &feature.kind {
                Kind::Hurricane(storm) => {
                    let seed = hurricane::seed(feature.behavior, storm);
                    let picture = hurricane::sprite(feature.radius, seed);
                    let anchor = (
                        f64::from(picture.width()) / 2.0,
                        f64::from(picture.height()) / 2.0,
                    );
                    let storm = Storm::Hurricane(*storm);
                    (
                        storm,
                        picture,
                        anchor,
                        hurricane::ALTITUDE,
                        Some(storm.south()),
                    )
                }
                Kind::Thunderstorm { .. } => {
                    let cloud =
                        thunder::cloud(thunder::size(feature.radius), feature.behavior as f64);
                    let storm = Storm::Thunder {
                        flashes: lightning.next().unwrap_or_default(),
                        radius: feature.radius,
                        behavior: feature.behavior,
                    };
                    (storm, cloud.canvas, cloud.base, thunder::ALTITUDE, None)
                }
                _ => continue,
            };
            let flier = fly::Flier {
                picture: &picture,
                anchor,
                altitude,
                hurricane,
            };
            let place = (feature.x, feature.y);
            let flown = fly::fly(self, ground, &polar, &flier, place, &taken, &buckets);
            let over = match flown {
                Some((over, held)) => {
                    taken.push(held);
                    over
                }
                None => place,
            };
            self.fliers.push(Flying {
                feature: index,
                storm,
                picture,
                anchor,
                over,
            });
        }
    }

    /// Whether a drifting cloud's shade covers a point of the screen at a moment.
    pub fn shaded(&self, x: f64, y: f64, moment: Moment) -> bool {
        self.clouds
            .as_ref()
            .is_some_and(|clouds| clouds.shades(x, y, moment))
    }

    /// When each of its storms flashes.
    #[cfg(test)]
    pub fn lightning(&self) -> Vec<Flashes> {
        self.fliers
            .iter()
            .filter_map(|flying| match flying.storm {
                Storm::Thunder { flashes, .. } => Some(flashes),
                Storm::Hurricane(_) => None,
            })
            .collect()
    }

    /// The drifting cloud shadows, when the clouds are many enough to cast them.
    pub fn cloud_shadows(&self) -> Option<&CloudShadows> {
        self.clouds.as_ref()
    }

    /// The box a wonder fills on the screen, moved up with the ground it stands on.
    pub fn wonder_box(&self, placed: &Placed) -> Taken {
        self.box_at(placed, Self::screen)
    }

    /// The box a wonder fills on a screen that places the map's points by `screen`.
    fn box_at(&self, placed: &Placed, screen: fn(&Self, f64, f64, f64) -> (f64, f64)) -> Taken {
        let (gx, gy) = placed.ground;
        let (x, y) = screen(self, gx, gy, 0.0);
        let (dx, dy) = (x - gx, y - gy);
        let b = placed.bounds;
        Taken {
            x0: b.x0 + dx,
            y0: b.y0 + dy,
            x1: b.x1 + dx,
            y1: b.y1 + dy,
            cost: 0.0,
        }
    }

    /// Where each storm flies over, by its place in nature's list.
    pub fn flight(&self, feature: usize) -> Option<(f64, f64)> {
        self.fliers
            .iter()
            .find(|flying| flying.feature == feature)
            .map(|flying| flying.over)
    }

    /// Where a place of the flat map lands on the screen, `alt` pixels above the ground.
    pub fn screen(&self, x: f64, y: f64, alt: f64) -> (f64, f64) {
        let height = self.standing.sample(x - 0.5, y - 0.5);
        (x, top_row() + FORESHORTEN * y - self.stand * (height + alt))
    }

    /// Where a place lands on the screen with the land standing, whatever height it is shown
    /// at.
    pub(super) fn standing_screen(&self, x: f64, y: f64, alt: f64) -> (f64, f64) {
        let height = self.standing.sample(x - 0.5, y - 0.5);
        (x, top_row() + FORESHORTEN * y - STANDING * (height + alt))
    }

    /// Whether anything on this map moves, so the screen keeps its clock running for it.
    pub fn moves(&self) -> bool {
        !self.water.is_empty()
            || !self.fliers.is_empty()
            || !self.upright.is_empty()
            || !self.quakes.is_empty()
            || self.clouds.is_some()
    }

    /// What moves on the ground and the water and what stands on the land at a moment, the
    /// picture between the land and the dots.
    pub fn under_dots(&self, moment: Moment) -> Canvas {
        self.laid(moment, parts::Over::Ground)
    }

    /// What passes over the dots at a moment: the earthquakes' rings, since the ground under
    /// the dots shook too, then the storms and hurricanes.
    pub fn over_dots(&self, moment: Moment) -> Canvas {
        self.laid(moment, parts::Over::Dots)
    }

    /// A storm's cloud at its height, or its shade on the ground below, thrown away from the
    /// sun.
    fn draw_storm(&self, pad: &mut Pad, flying: &Flying, moment: Moment, shade: bool) {
        let (run, (ux, uy)) = shade_run();
        let (px, py) = flying.over;
        match flying.storm {
            Storm::Hurricane(storm) => {
                let at = if shade {
                    let fall = hurricane::ALTITUDE * run;
                    self.screen(px + ux * fall, py + uy * fall, 0.0)
                } else {
                    self.screen(px, py, hurricane::ALTITUDE)
                };
                let angle = hurricane::angle(&storm, moment);
                hurricane::draw(pad, &flying.picture, at, angle, storm.south, shade);
            }
            Storm::Thunder {
                flashes,
                radius,
                behavior,
            } => {
                if shade {
                    let fall = thunder::ALTITUDE * run;
                    let at = self.screen(px + ux * fall, py + uy * fall, 0.0);
                    thunder::shade(pad, at, radius);
                } else {
                    thunder::draw(
                        pad,
                        self,
                        (&flying.picture, flying.anchor),
                        flying.over,
                        radius,
                        flashes,
                        behavior,
                        moment,
                    );
                }
            }
        }
    }
}
