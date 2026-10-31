//! The planet as the tab draws it: the map's ground wrapped round a globe that turns a step at
//! a time, the map's weather moving on it where the map has it, and the wonders standing
//! upright at their places.

use std::sync::Arc;

use super::{Grown, Look};
use crate::world::cosmos::HomeShot;
use crate::world::map::fields::{Look as GroundLook, Rgb};
use crate::world::map::land::{Compared, Texture, Tint};
use crate::world::map::motion::Flashes;
use crate::world::map::nature::{GroundMarks, Kind, current_runs, fire_front, wind};
use crate::world::map::weather::flat::storm_cloud_from_above;
use crate::world::map::weather::tsunami::coast;
use crate::world::map::weather::{FlatLayers, Keep, hurricane};
use crate::world::planet::{
    self, CloudsIn, DotColours, Planet, PlanetFacts, Shot, Side, Spin, Standing,
};
use crate::world::planet_fx::{
    self, Current, Flow, Quake, Stretch, Thunderstorm, Tornado, Tsunami, Volcano, Wildfire,
};
use crate::world::raster::Canvas;
use crate::world::wonders::{self, Placed};
use crate::world::{Mark, Picture, Pointed, Scene, View};

/// A wonder standing upright on the planet, drawn as the map draws it.
#[derive(Debug, Clone)]
struct Upright {
    placed: Placed,
    /// Its place in the map's list of wonders, which its line comes from.
    index: u32,
}

impl Standing for Upright {
    fn place(&self) -> (f64, f64) {
        self.placed.ground
    }

    fn size(&self) -> f64 {
        self.placed.size
    }

    fn picture(&self, size: f64, light: [f64; 3]) -> (Canvas, (f64, f64)) {
        let sprite = wonders::sprite(self.placed.wonder, size, light);
        (sprite.canvas, sprite.ground)
    }

    fn pointed(&self) -> Pointed {
        Pointed::Wonder(self.index)
    }
}

/// What one look of the planet draws from: the planet, what moves on it and what stands on it,
/// and the flat map's marks, which the planet carries to where they lie on the globe.
pub struct PlanetLook {
    pub planet: Arc<Planet>,
    weather: Vec<Box<dyn planet_fx::Weather>>,
    standing: Vec<Upright>,
    marks: Vec<Mark>,
    /// Its pictures turned so far, kept so a second turn draws nothing.
    spin: Spin,
    /// The planet as one picture for a zoom, and what it was drawn for.
    shot: Option<(u64, HomeShot)>,
    /// The ground it is wrapped in, laid flat at the map's size, which it unrolls from.
    flat_ground: Canvas,
}

/// Where the scene of a planet stands: which way it faces, its side, its size and its moment.
#[derive(Debug, Clone)]
pub struct Facing {
    pub view: View,
    pub side: Side,
    pub size: (usize, usize),
    pub moment: planet::Moment,
    /// The universe's sky behind the planet, or the plain one.
    pub sky: Option<Picture>,
}

impl PlanetLook {
    /// The planet of a crew's world in one look; `facts` are what it is drawn from besides its
    /// ground, and `key` tells its pictures from every other planet's.
    pub fn new(grown: &Arc<Grown>, look: &Look, facts: &PlanetFacts, key: u64) -> Self {
        let ground = &grown.ground;
        let compared = look.compared.and_then(|group| Compared::new(ground, group));
        let tint = compared.map(|compared| Tint::new(ground, compared));
        let shown = look.shows_nature() && ground.look != GroundLook::Gas;
        let marks = GroundMarks::new(ground, &grown.nature, shown, grown.sunk);
        let texture = Texture::new(ground, tint.as_ref(), marks.marks());
        let flat_ground = planet::flat_ground(ground, tint.as_ref(), marks.marks());
        // What moves on the planet is drawn there moving, so the still pictures leave it out.
        let flat = FlatLayers::new(ground, &grown.nature, Keep::Planet, grown.sunk);
        let (lights, sky) = (flat.glow.to_rgba(), flat.sky.to_rgba());
        let nature = planet::Nature {
            shown,
            lights: Some(&lights),
            sky: Some(&sky),
        };
        let colours = DotColours {
            groups: look
                .colours
                .iter()
                .map(|rgb| rgb.map(f64::from))
                .collect::<Vec<Rgb>>(),
            compared: look.compared,
        };
        let planet = Planet::new(key, ground, texture, facts, nature, &colours);
        let (weather, standing) = if shown {
            let standing = grown
                .wonders
                .iter()
                .enumerate()
                .map(|(index, placed)| Upright {
                    placed: placed.clone(),
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                })
                .collect();
            (weather(grown), standing)
        } else {
            (Vec::new(), Vec::new())
        };
        Self {
            planet: Arc::new(planet),
            weather,
            standing,
            marks: Vec::new(),
            spin: Spin::new(),
            shot: None,
            flat_ground,
        }
    }

    /// When each of its storms flashes.
    #[cfg(test)]
    pub fn lightning(&self) -> Vec<Flashes> {
        self.weather
            .iter()
            .filter_map(|thing| thing.flashes())
            .collect()
    }

    /// Carries the flat map's marks of nature onto the planet, where it finds them.
    pub fn with_marks(mut self, marks: Vec<Mark>) -> Self {
        self.marks = marks;
        self
    }

    /// The picture `through` of the way from the planet facing `from` to `flat_map`, the map's
    /// land laid flat and lit as the map lights it, from 0 to 1. The dots and the wonders come
    /// along on their own, so `flat_map` holds neither. `None` for a map of another size.
    pub fn unrolled(
        &self,
        from: View,
        through: f64,
        side: Side,
        flat_map: &Canvas,
    ) -> Option<Canvas> {
        let standing: Vec<&dyn Standing> = self
            .standing
            .iter()
            .map(|thing| thing as &dyn Standing)
            .collect();
        self.planet
            .unrolled(from, through, side, flat_map, &self.flat_ground, &standing)
    }

    /// The planet small in the middle of the universe's scene `size` px, turning with what
    /// moves on it that still reads at that size: its sprites, and whether they move.
    pub fn home(
        &mut self,
        size: (usize, usize),
        view: View,
        moment: planet::Moment,
    ) -> (Vec<crate::world::Sprite>, bool) {
        let (width, height) = (size.0 as f64, size.1 as f64);
        let radius = crate::world::cosmos::home_radius(width, height);
        self.home_at(size, view, moment, radius)
    }

    /// As [`PlanetLook::home`], at `radius` px, as the zoom out from the planet shrinks it.
    pub fn home_at(
        &mut self,
        size: (usize, usize),
        view: View,
        moment: planet::Moment,
        radius: f64,
    ) -> (Vec<crate::world::Sprite>, bool) {
        let weather: Vec<&dyn planet_fx::Weather> =
            self.weather.iter().map(|thing| thing.as_ref()).collect();
        let (width, height) = (size.0 as f64, size.1 as f64);
        let home = planet::Home {
            view,
            side: Side::Day,
            centre: (width / 2.0, height / 2.0),
            radius,
            size,
            clouds: CloudsIn::Apart,
            weather: &weather,
            moment,
        };
        self.spin.home(&self.planet, &home)
    }

    /// The planet as its level shows it facing `facing`, without the sky, as one picture cut
    /// to what shows of it, for the zoom between it and the universe to shrink or grow; its
    /// globe is `radius` px round the middle. Drawn once for each look and kept for the zoom's
    /// steps.
    pub fn shot(&mut self, facing: Facing, radius: f64) -> HomeShot {
        let size = facing.size;
        let whole = self.scene(facing).without(crate::world::Depth::Sky);
        let key = crate::world::picture_key(
            whole
                .stack()
                .iter()
                .flat_map(|part| {
                    let (x, y) = match part {
                        crate::world::Part::Sprite(sprite) => sprite.at,
                        _ => (0, 0),
                    };
                    [part.picture().key, u64::from(x as u32), u64::from(y as u32)]
                })
                .chain([size.0 as u64, size.1 as u64, radius.to_bits()]),
        );
        if let Some((kept, shot)) = &self.shot
            && *kept == key
        {
            return shot.clone();
        }
        let (image, (left, top)) = whole.flatten().trimmed();
        let shot = HomeShot {
            picture: Picture::new(key, image),
            centre: (
                size.0 as f64 / 2.0 - left as f64,
                size.1 as f64 / 2.0 - top as f64,
            ),
            radius,
        };
        self.shot = Some((key, shot.clone()));
        shot
    }

    /// The planet facing `facing`, its clouds apart where they drift, and what the pointer
    /// finds on it.
    pub fn scene(&mut self, facing: Facing) -> Scene {
        let weather: Vec<&dyn planet_fx::Weather> =
            self.weather.iter().map(|thing| thing.as_ref()).collect();
        let standing: Vec<&dyn Standing> = self
            .standing
            .iter()
            .map(|thing| thing as &dyn Standing)
            .collect();
        let shot = Shot {
            view: facing.view,
            side: facing.side,
            size: facing.size,
            clouds: CloudsIn::Apart,
            weather: &weather,
            standing: &standing,
            moment: facing.moment,
            sky: facing.sky.as_ref(),
        };
        let mut scene = self.spin.scene(&self.planet, &shot);
        scene.hits = self
            .planet
            .hits(facing.view, facing.size, &self.marks, &standing);
        scene
    }
}

/// What moves on the planet, from what nature found on the map, in the order the planet draws
/// it: the water, the tsunami, the earthquakes, the fires, the volcanoes, the tornadoes, the
/// storms and the hurricanes.
fn weather(grown: &Arc<Grown>) -> Vec<Box<dyn planet_fx::Weather>> {
    let ground = &grown.ground;
    let features = &grown.nature.features;
    let mut water: Vec<Box<dyn planet_fx::Weather>> = Vec::new();
    let mut waves: Vec<Box<dyn planet_fx::Weather>> = Vec::new();
    let mut quakes: Vec<Box<dyn planet_fx::Weather>> = Vec::new();
    let mut fires: Vec<Box<dyn planet_fx::Weather>> = Vec::new();
    let mut volcanoes: Vec<Box<dyn planet_fx::Weather>> = Vec::new();
    let mut tornadoes: Vec<Box<dyn planet_fx::Weather>> = Vec::new();
    let mut storms: Vec<Box<dyn planet_fx::Weather>> = Vec::new();
    let mut hurricanes: Vec<Box<dyn planet_fx::Weather>> = Vec::new();
    let mut lightning =
        Flashes::for_storms(&crate::world::map::weather::thunderstorms(&grown.nature)).into_iter();
    for (index, feature) in features.iter().enumerate() {
        let seed = u32::try_from(index).unwrap_or(u32::MAX);
        let at = (feature.x, feature.y);
        match &feature.kind {
            Kind::River(stream) => water.push(Box::new(Flow {
                river: true,
                path: stream.path.clone(),
                width: stream.width,
                at_sea: None,
            })),
            Kind::Current(stream) => water.push(Box::new(Current {
                stretches: current_runs(ground, stream)
                    .into_iter()
                    .map(|run| Stretch::along(run.points, run.band))
                    .collect(),
                seed,
                to: u32::try_from(stream.to).unwrap_or(u32::MAX),
            })),
            Kind::Tsunami(wave) => {
                let world = grown.clone();
                waves.push(Box::new(Tsunami {
                    from: wave.from,
                    to: wave.to,
                    toward: (-wave.out.0, -wave.out.1),
                    size: wave.size,
                    seed,
                    coast: coast(ground, wave),
                    at_sea: Arc::new(move |x, y| world.ground.on_sea(x, y)),
                }));
            }
            Kind::Rift(rift) if rift.shaking() => quakes.push(Box::new(Quake {
                at,
                crashes: u32::try_from(rift.recent.len()).unwrap_or(u32::MAX).min(3),
            })),
            Kind::Wildfire { burning, .. } => fires.push(Box::new(Wildfire {
                front: fire_front(ground, feature),
                burning: *burning,
                wind: wind(),
            })),
            Kind::Volcano { smoking, .. } => volcanoes.push(Box::new(Volcano {
                at,
                radius: feature.radius,
                smoking: *smoking,
                wind: wind(),
            })),
            Kind::Tornado { .. } => tornadoes.push(Box::new(Tornado {
                at,
                radius: feature.radius,
                seed,
            })),
            Kind::Thunderstorm { sessions } => storms.push(Box::new(Thunderstorm {
                at,
                radius: feature.radius,
                seed,
                stopped: *sessions,
                flashes: lightning.next().unwrap_or_default(),
                wind: wind(),
                picture: Arc::new(storm_cloud_from_above(
                    (feature.radius + 0.5).floor(),
                    feature.behavior as f64,
                )),
            })),
            Kind::Hurricane(storm) => hurricanes.push(Box::new(planet_fx::Hurricane {
                at,
                radius: feature.radius,
                category: u8::try_from(storm.category).unwrap_or(u8::MAX),
                south: storm.south,
                picture: Arc::new(hurricane::sprite(
                    feature.radius,
                    hurricane::seed(feature.behavior, storm),
                )),
            })),
            _ => {}
        }
    }
    [
        water, waves, quakes, fires, volcanoes, tornadoes, storms, hurricanes,
    ]
    .into_iter()
    .flatten()
    .collect()
}
