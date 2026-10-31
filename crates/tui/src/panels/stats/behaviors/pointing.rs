//! What the pointer finds where, and the scene the screen places: on every level the world
//! hands the screen its picture and a hit map of what it drew, so a hover line comes from
//! pointing at what is there. The map's dots, the planet's near side and the universe's bodies
//! answer here; the land, nature and the wonders join them as the world draws them.

use std::hash::{DefaultHasher, Hash, Hasher};

use argonauts_core::behaviors::behavior_map::BehaviorMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::drawn::{Drawn, Facing, Globed, Grown, Look, Made, MapLook, PlanetLook, SEED};
use super::level::Level;
use super::places::{Kind, places};
use super::{BehaviorsTab, Fit, GroupColours};
use crate::frame::view::map::MapPoint;
use crate::world::map::motion::Moment as MapMoment;
use crate::world::map::relief::STANDING;
use crate::world::raster::noise::smooth;
use crate::world::{HitMap, Mark, PartId, Picture, Pointed, Scene, Shape, Under, picture_key};

/// Names the worlds the tab grows, one for each map it took.
const WORLD: PartId = PartId::new("behaviors world", 0);
/// Names the planet of each world, apart from its map.
const PLANET: PartId = PartId::new("behaviors planet", 0);
/// Names the universe of each world.
const COSMOS: PartId = PartId::new("behaviors cosmos", 0);
/// Names the zooms between the world's levels.
const ZOOM: PartId = PartId::new("behaviors zoom", 0);
/// The share of the way between the planet and the map the unrolling takes. The land stands
/// up over the rest, so the map arrives standing rather than cutting to it at the end.
const UNROLLED: f64 = 0.85;

/// What a job needs to make a map's universe.
#[derive(Debug)]
pub(super) struct CosmosParts {
    facts: Box<argonauts_core::behaviors::behavior_world::WorldFacts>,
    models: Vec<[u8; 3]>,
    actions: Vec<crate::world::cosmos::StarAction>,
    key: u64,
}

impl CosmosParts {
    fn make(&self) -> crate::world::cosmos::Cosmos {
        crate::world::cosmos::Cosmos::new(&self.facts, &self.models, self.actions.clone())
    }
}

/// The universe's own level for one of the tab's, inside the universe.
fn cosmos_level(level: Level) -> Option<crate::world::cosmos::Level> {
    use crate::world::cosmos::Level as Cosmos;
    match level {
        Level::Universe => Some(Cosmos::Universe),
        Level::Galaxy(maker) => Some(Cosmos::Galaxy(maker)),
        Level::System(star) => Some(Cosmos::System(star)),
        Level::Map | Level::Planet => None,
    }
}

/// What the universe's scene reads besides the tab: the theme's colours and the names the app
/// keeps.
#[derive(Clone, Copy)]
pub struct CosmosLook<'a> {
    pub theme: &'a crate::base::theme::Theme,
    pub names: &'a argonauts_core::look::display_names::DisplayNames,
}

/// What a job needs to find or make a map's planet: the map, its flat look, each CLI's colour
/// for a gas giant's bands, and the keys of its world, its flat map and its planet.
struct PlanetParts {
    map: Arc<BehaviorMap>,
    look: Look,
    clis: Vec<(String, [f64; 3])>,
    world: u64,
    flat: u64,
    globe: u64,
}

impl PlanetParts {
    /// The planet these parts name, made now unless it is the one kept, the flat map's marks of
    /// nature carried onto it.
    fn planet<'k>(
        &self,
        kept: &'k mut super::drawn::Kept,
        grown: &Arc<Grown>,
    ) -> Option<&'k mut PlanetLook> {
        if kept.kept_planet(self.globe).is_none() {
            let flat_map = kept.look(self.flat, || {
                Some(MapLook::new(grown, &self.look, self.flat))
            })?;
            let marks: Vec<Mark> = flat_map
                .scene(MapMoment::Still, self.flat)
                .hits
                .marks()
                .iter()
                .filter(|mark| !matches!(mark.pointed, Pointed::Action(_)))
                .cloned()
                .collect();
            let colour_of = |cli: &str| {
                self.clis
                    .iter()
                    .find(|(name, _)| name == cli)
                    .map_or([150.0, 156.0, 170.0], |(_, rgb)| *rgb)
            };
            let facts = crate::world::planet::PlanetFacts::from_map(&self.map, colour_of);
            let made = PlanetLook::new(grown, &self.look, &facts, self.globe).with_marks(marks);
            kept.set_planet(self.globe, made);
        }
        kept.kept_planet(self.globe)
    }
}

/// How far from a dot the pointer still finds it right under it, and how far it finds the
/// nearest one on the planet, where the bare ground answers beyond.
const DOT_REACH: f32 = 6.0;
const PLANET_REACH: f32 = 30.0;
/// How far outside a body of the universe the pointer still finds it.
const BODY_REACH: f32 = 4.0;
/// How long after the wheel zoomed its next turns are taken for the same swipe.
pub const WHEEL_GAP: Duration = Duration::from_millis(380);

/// The braille cells' dots across and down in `fit`'s cells: what the pointer finds in braille
/// is found among them, where the dots were drawn.
fn dots(fit: Fit) -> (f32, f32) {
    ((fit.columns * 2) as f32, (fit.rows * 4) as f32)
}
/// Dots across a round shape takes for each one down, as braille draws it.
const ACROSS: f32 = 10.5 / 9.0;

/// Where a point over the braille cells falls among their `dots`.
fn in_dots(at: MapPoint, dots: (f32, f32)) -> (f32, f32) {
    let scale = |at: f32, of: f32, to: f32| if of > 0.0 { at * to / of } else { at };
    (
        scale(at.x, at.width, dots.0),
        scale(at.y, at.height, dots.1),
    )
}

/// Where a place on the map, 0..1 across and up, falls among the braille dots.
fn braille_place(x: f32, y: f32, dots: (f32, f32)) -> (f32, f32) {
    (x * dots.0, (1.0 - y) * dots.1)
}

impl BehaviorsTab {
    /// What the pointer finds on the level shown where braille draws it, among its dots.
    pub fn hit_map(&self) -> HitMap {
        let fit = self.fit();
        let (width, height) = dots(fit);
        let mut hits = HitMap::new();
        let Some(map) = self.map.as_ref() else {
            return hits;
        };
        let point = |shape: Shape, pointed: Pointed| Mark::new(shape, pointed);
        let index = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        match self.level {
            Level::Map => {
                for (at, action) in map.actions.iter().enumerate() {
                    let (x, y) = braille_place(action.x, action.y, (width, height));
                    let shape = Shape::Point { x, y };
                    // Right under the pointer first; else the nearest dot anywhere.
                    hits.add(point(shape.clone(), Pointed::Action(index(at))).reach(DOT_REACH));
                    hits.add(
                        point(shape, Pointed::Action(index(at)))
                            .reach(f32::INFINITY)
                            .tier(2),
                    );
                }
            }
            Level::Planet => {
                let view = self.planet_view();
                for (at, action) in map.actions.iter().enumerate() {
                    let Some((x, y)) = braille_on_planet(action.x, action.y, view, fit) else {
                        continue;
                    };
                    let shape = Shape::Point { x, y };
                    hits.add(point(shape.clone(), Pointed::Action(index(at))).reach(DOT_REACH));
                    hits.add(
                        point(shape, Pointed::Action(index(at)))
                            .reach(PLANET_REACH)
                            .tier(2),
                    );
                }
            }
            _ => {
                for place in places(&self.universe, self.level) {
                    let (x, y) = (place.at.0 as f32 * width, place.at.1 as f32 * height);
                    let radius = place.radius as f32 * height;
                    let mark = match place.kind {
                        Kind::Belt { orbit } => {
                            let points = (0..=72)
                                .map(|step| {
                                    let (sin, cos) = (step as f32 * 5.0).to_radians().sin_cos();
                                    (
                                        width * (0.5 + cos * orbit.0 as f32),
                                        height * (0.5 + sin * orbit.1 as f32),
                                    )
                                })
                                .collect();
                            point(Shape::Path { points, width: 6.0 }, place.pointed).tier(1)
                        }
                        _ => point(
                            Shape::Ellipse {
                                x,
                                y,
                                rx: radius * ACROSS,
                                ry: radius,
                                angle: 0.0,
                            },
                            place.pointed,
                        ),
                    };
                    hits.add(mark.reach(BODY_REACH));
                }
                hits.set_under(Under::Sky);
            }
        }
        hits
    }

    /// The pointer rests at `at` over the world, or has left it. Whether what it rests on
    /// changed.
    pub fn point(&mut self, at: Option<MapPoint>) -> bool {
        self.held = at.filter(|at| at.inside());
        let found = at.filter(|at| at.inside()).and_then(|at| {
            let found = match self.drawn_here() {
                Some(drawn) => {
                    let (x, y) = at.in_scene(&drawn.scene);
                    let found = drawn.scene.hits.find(x, y);
                    // Off the globe the pointer is on the universe's sky, which names the
                    // earlier month whose cluster it rests in, as the universe does there.
                    let sky = drawn.globe.as_ref().and_then(|globe| globe.sky.as_ref());
                    match (found, sky) {
                        (Some(Pointed::Sky(None)), Some(sky)) => Some(sky.at(x, y)),
                        (found, _) => found,
                    }
                }
                // Until the world's first picture lands there is nothing to point at.
                None if self.draws_world() => None,
                None => {
                    let (x, y) = in_dots(at, dots(self.fit()));
                    self.hit_map().find(x, y)
                }
            };
            // Open sky with nothing earlier in it says nothing, and the sea where no one place
            // shows neither.
            found.filter(|found| !matches!(found, Pointed::Ground(None)))
        });
        if self.level == Level::Planet {
            self.hold.rest(self.held.is_some(), self.moment.now);
        }
        let changed = found != self.pointed;
        self.pointed = found;
        changed
    }

    /// The pointer at `place` on the map, in the map's own units, 0..1 across and up: on the
    /// map the dot nearest it, read among the braille cells' dots, which lie where the map's own
    /// units put them; elsewhere the point there over the level's picture.
    pub fn point_at(&mut self, place: Option<(f32, f32)>) -> bool {
        let at = place.map(|(x, y)| {
            let dots = dots(self.fit());
            let (x, y) = braille_place(x, y, dots);
            MapPoint {
                x,
                y,
                width: dots.0,
                height: dots.1,
            }
        });
        if self.level != Level::Map {
            return self.point(at);
        }
        // A point just off the map's edge still finds the dot nearest it.
        let found = at.and_then(|at| self.hit_map().find(at.x, at.y));
        let found = found.filter(|found| matches!(found, Pointed::Action(_)));
        let changed = found != self.pointed;
        self.pointed = found;
        changed
    }

    /// A click where the pointer rests: on a dot, its card; in the universe or a galaxy, into
    /// what it rests on; in a system, the body it rests on picked.
    pub fn click(&mut self) -> bool {
        match (self.level, self.pointed) {
            (Level::Map | Level::Planet, Some(Pointed::Action(_))) => self.open_pointed_card(),
            (Level::System(_), Some(target @ (Pointed::Body(_) | Pointed::Star(_)))) => {
                self.picked = Some(target);
                self.pointed = None;
                true
            }
            (level, Some(target)) if level.in_cosmos() => {
                match self.universe.inside(level, target) {
                    Some(to) => {
                        self.go(to);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// The scene of the level shown at `cell` pixels a cell, and what the pointer finds where.
    /// The map comes from the world drawn off the frame, the newest scene finished, with the
    /// ring round what the pointer found laid over it, and nothing until the first lands; the
    /// other levels are one picture.
    pub fn scene(
        &self,
        colours: &GroupColours,
        theme: &crate::base::theme::Theme,
        names: &argonauts_core::look::display_names::DisplayNames,
        cell: (u16, u16),
        now: Instant,
    ) -> Option<Arc<Scene>> {
        if self.draws_world()
            && let Some(zoom) = self.zoom(now)
        {
            let look = CosmosLook { theme, names };
            if let Some(drawn) = self.zoom_drawn(zoom, colours, look, cell, now) {
                return Some(drawn.scene);
            }
        }
        if !self.draws_world() {
            return None;
        }
        match self.level {
            Level::Map => {
                let drawn = self.map_drawn(colours, now)?;
                Some(Arc::new(self.marked(&drawn)))
            }
            Level::Planet => {
                let look = CosmosLook { theme, names };
                let drawn = self.planet_drawn(colours, look, cell, now)?;
                Some(Arc::new(self.marked(&drawn)))
            }
            Level::Universe | Level::Galaxy(_) | Level::System(_) => {
                let look = CosmosLook { theme, names };
                Some(self.cosmos_drawn(colours, look, cell, now)?.scene)
            }
        }
    }

    /// The map's newest scene drawn for what the tab shows, while the one wanted is drawn.
    fn map_drawn(&self, colours: &GroupColours, now: Instant) -> Option<Drawn> {
        let map = self.map.clone()?;
        let look = self.map_look(&map, colours);
        let world = picture_key([WORLD.get(), self.maps_taken]);
        let mut hasher = DefaultHasher::new();
        look.hash(&mut hasher);
        let key = picture_key([world, hasher.finish()]);
        let tick = (!self.moment.still).then(|| self.drawings.tick(now));
        self.drawings.want(key, tick, move |kept, cancel| {
            let stop = || cancel.cancelled();
            let grown = kept.grown(world, || Grown::unless(&map, SEED, &stop))?;
            let shown = kept.look(key, || MapLook::unless(&grown, &look, key, &stop))?;
            let moment = tick.map_or(MapMoment::Still, MapMoment::of_tick);
            Some(Made {
                scene: shown.scene(moment, key),
                world: Some(grown),
                globe: None,
            })
        })
    }

    /// The look of the map the tab shows: its groups' colours, a group compared, nature shown
    /// or hidden, standing or laid flat.
    fn map_look(&self, map: &BehaviorMap, colours: &GroupColours) -> Look {
        Look {
            flat: self.flat,
            compared: self.compared,
            nature: !self.nature_hidden,
            colours: (0..map.groups.len())
                .map(|group| {
                    let (r, g, b) = colours.rgb(group);
                    [r, g, b]
                })
                .collect(),
        }
    }

    /// What the jobs need to find or make the planet of the map the tab shows.
    fn planet_parts(
        &self,
        colours: &GroupColours,
        theme: &crate::base::theme::Theme,
    ) -> Option<PlanetParts> {
        let map = self.map.clone()?;
        let look = Look {
            flat: true,
            ..self.map_look(&map, colours)
        };
        let clis: Vec<(String, [f64; 3])> = map
            .actions
            .iter()
            .map(|action| (action.cli.as_str(), action.agent.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_iter()
            .map(|(cli, agent)| {
                let rgb = match theme.color(crate::base::theme::Role::for_cli(agent)) {
                    Some(ratatui::style::Color::Rgb(r, g, b)) => [r, g, b],
                    _ => [150, 156, 170],
                };
                (cli.to_owned(), rgb.map(f64::from))
            })
            .collect();
        let world = picture_key([WORLD.get(), self.maps_taken]);
        let mut hasher = DefaultHasher::new();
        look.hash(&mut hasher);
        let flat = picture_key([world, hasher.finish()]);
        Some(PlanetParts {
            map,
            look,
            clis,
            world,
            flat,
            globe: picture_key([flat, PLANET.get()]),
        })
    }

    /// The planet's newest scene drawn for what the tab shows, while the one wanted is drawn:
    /// turned as far as the crew's pace has turned it since the world opened, from where a
    /// drag left it, at `cell` pixels a cell.
    fn planet_drawn(
        &self,
        colours: &GroupColours,
        look: CosmosLook<'_>,
        cell: (u16, u16),
        now: Instant,
    ) -> Option<Drawn> {
        use crate::world::planet::{self, Side, turned};
        let parts = self.planet_parts(colours, look.theme)?;
        // The sky behind the planet is the universe's, its earlier months among the far stars.
        let sky = self.cosmos_parts(&parts, look);
        let side = if self.night { Side::Night } else { Side::Day };
        let size = self.fit().pixels(cell);
        let still = self.moment.still;
        let tick = (!still).then(|| self.drawings.tick(now));
        let key = picture_key([
            parts.globe,
            u64::from(self.night),
            u64::try_from(self.turn.rem_euclid(36)).unwrap_or(0),
            u64::from(cell.0),
            u64::from(cell.1),
            size.0 as u64,
            size.1 as u64,
        ]);
        let from = self.planet_view().lon;
        // The turn and the clouds' drift stop while the pointer rests on the planet; what moves
        // on it plays on.
        let holding = self.hold.since.is_some();
        let turning = tick.map_or(0.0, |_| {
            let run = self.drawings.since_start(now);
            run.saturating_sub(self.hold.held(now)).as_secs_f64()
        });
        self.drawings.want(key, tick, move |kept, cancel| {
            let stop = || cancel.cancelled();
            let grown = kept.grown(parts.world, || Grown::unless(&parts.map, SEED, &stop))?;
            let (sky, clusters) = match sky {
                Some(sky) => {
                    // Under the key the zooms to and from the universe show it by, so they
                    // send it no second time.
                    let universe = kept.cosmos(sky.key, || sky.make());
                    let clusters = universe.sky_clusters(size.0, size.1);
                    (Some(universe.sky_layer(size.0, size.1)), Some(clusters))
                }
                None => (None, None),
            };
            let shown = parts.planet(kept, &grown)?;
            let seconds = tick.map_or(0.0, |tick| tick as f64 / 8.0);
            let view = crate::world::View::stepped(turned(&shown.planet, from, turning));
            let moment = planet::Moment {
                t: seconds,
                drift: turning,
                holding,
                still,
            };
            let scene = shown.scene(Facing {
                view,
                side,
                size,
                moment,
                sky,
            });
            let globe = Globed {
                planet: shown.planet.clone(),
                side,
                drift: planet::shown_drift(planet::CloudsIn::Apart, moment),
                sky: clusters,
            };
            Some(Made {
                scene,
                world: Some(grown),
                globe: Some(globe),
            })
        })
    }

    /// The universe's newest scene drawn for the level the tab shows, while the one wanted is
    /// drawn: a maker's galaxy, a model's system, or every galaxy round your planet turning
    /// small in the middle.
    fn cosmos_drawn(
        &self,
        colours: &GroupColours,
        look: CosmosLook<'_>,
        cell: (u16, u16),
        now: Instant,
    ) -> Option<Drawn> {
        use crate::world::cosmos;
        use crate::world::planet;
        let parts = self.planet_parts(colours, look.theme)?;
        let sky = self.cosmos_parts(&parts, look)?;
        let level = cosmos_level(self.level)?;
        let size = self.fit().pixels(cell);
        let still = self.moment.still;
        let tick = (!still).then(|| self.drawings.tick(now));
        let ringed = self.pointed.or(self.picked);
        let universe = sky.key;
        let key = picture_key([
            universe,
            format!("{level:?} {ringed:?}").len() as u64,
            {
                let mut hasher = DefaultHasher::new();
                format!("{level:?} {ringed:?}").hash(&mut hasher);
                hasher.finish()
            },
            u64::from(cell.0),
            u64::from(cell.1),
            size.0 as u64,
            size.1 as u64,
        ]);
        let from = self.planet_view().lon;
        self.drawings.want(key, tick, move |kept, cancel| {
            let seconds = tick.map_or(0.0, |tick| tick as f64 / 8.0);
            let view = cosmos::View {
                still,
                ringed,
                ..cosmos::View::new(level, size.0, size.1)
            };
            let made = kept.cosmos(universe, || sky.make());
            let mut scene = made.scene(&view, seconds);
            if level == cosmos::Level::Universe {
                let stop = || cancel.cancelled();
                let grown = kept.grown(parts.world, || Grown::unless(&parts.map, SEED, &stop))?;
                let home = parts.planet(kept, &grown)?;
                let turned =
                    crate::world::View::stepped(planet::turned(&home.planet, from, seconds));
                let moment = planet::Moment {
                    t: seconds,
                    drift: seconds,
                    holding: false,
                    still,
                };
                let (sprites, moves) = home.home(size, turned, moment);
                for sprite in sprites {
                    scene.add_sprite(sprite);
                }
                scene.moves |= moves && !still;
            }
            Some(Made {
                scene,
                world: None,
                globe: None,
            })
        })
    }

    /// What the jobs need to make the map's universe: its facts, each model's colour and the
    /// map's actions that belong to its models. Worked out once for each map taken, as the jobs
    /// make its universe once, not at every step of the motion.
    fn cosmos_parts(&self, parts: &PlanetParts, look: CosmosLook<'_>) -> Option<Arc<CosmosParts>> {
        use crate::world::cosmos::star_actions;
        let key = picture_key([parts.world, COSMOS.get()]);
        if let Some(kept) = self.cosmos.borrow().as_ref().filter(|kept| kept.key == key) {
            return Some(Arc::clone(kept));
        }
        let facts = parts.map.world.clone()?;
        let models = facts
            .models
            .iter()
            .map(|model| {
                match look
                    .theme
                    .color(crate::base::theme::Role::for_cli(&model.agent))
                {
                    Some(ratatui::style::Color::Rgb(r, g, b)) => [r, g, b],
                    _ => [150, 156, 170],
                }
            })
            .collect();
        let actions = star_actions(&parts.map, &facts, look.names, &self.clock);
        let made = Arc::new(CosmosParts {
            facts,
            models,
            actions,
            key,
        });
        *self.cosmos.borrow_mut() = Some(Arc::clone(&made));
        Some(made)
    }

    /// A frame of the zoom on its way: the planet unrolling into the map or rolling up out of
    /// it, the universe growing round your planet or closing in on it, or the universe's own
    /// zooms between its levels. `None` between levels no zoom joins, which then show the
    /// level arrived at.
    fn zoom_drawn(
        &self,
        zoom: super::level::Zoom,
        colours: &GroupColours,
        look: CosmosLook<'_>,
        cell: (u16, u16),
        now: Instant,
    ) -> Option<Drawn> {
        use crate::world::planet::{self, Side};
        use crate::world::{Globe, cosmos};
        let parts = self.planet_parts(colours, look.theme)?;
        let sky = match (zoom.from, zoom.to) {
            (Level::Map, Level::Planet) | (Level::Planet, Level::Map) => None,
            _ => Some(self.cosmos_parts(&parts, look)?),
        };
        let frame = zoom.frame(now);
        let k = f64::from(frame) / f64::from(super::level::ZOOM_FRAMES);
        let size = self.fit().pixels(cell);
        let side = if self.night { Side::Night } else { Side::Day };
        let facing = self.planet_view();
        let still = self.moment.still;
        // The level left as it was when the zoom began, the one arrived at as it will be.
        let began = self.drawings.tick(zoom.since()) as f64 / 8.0;
        let times = cosmos::Times {
            from: began,
            to: began + f64::from(super::level::ZOOM_FRAMES) / 8.0,
        };
        // How far the planet has turned at its end of the zoom, as its level turns it.
        let planet_end = if zoom.to == Level::Planet {
            zoom.since() + Duration::from_millis(125) * super::level::ZOOM_FRAMES
        } else {
            zoom.since()
        };
        let turning = if still {
            0.0
        } else {
            let run = self.drawings.since_start(planet_end);
            run.saturating_sub(self.hold.held(planet_end)).as_secs_f64()
        };
        let ringed = self.picked;
        let shown = Look {
            flat: self.flat,
            ..parts.look.clone()
        };
        let mut hasher = DefaultHasher::new();
        format!("{:?} {:?} {ringed:?} {}", zoom.from, zoom.to, shown.flat).hash(&mut hasher);
        let key = picture_key([
            ZOOM.get(),
            parts.globe,
            hasher.finish(),
            u64::from(self.night),
            u64::from(cell.0),
            u64::from(cell.1),
            size.0 as u64,
            size.1 as u64,
        ]);
        let moment = move |t: f64| planet::Moment {
            t,
            drift: t,
            holding: false,
            still,
        };
        self.drawings
            .want_every_step(key, Some(u64::from(frame)), move |kept, cancel| {
                let stop = || cancel.cancelled();
                let grown = kept.grown(parts.world, || Grown::unless(&parts.map, SEED, &stop))?;
                let mut scene = match (zoom.from, zoom.to, sky) {
                    (Level::Map, Level::Planet, _) | (Level::Planet, Level::Map, _) => {
                        let on_map = if zoom.to == Level::Map { k } else { 1.0 - k };
                        let frame = picture_key([key, frame.into()]);
                        let picture = if on_map > UNROLLED {
                            let full = if shown.flat { 0.0 } else { STANDING };
                            let stand = full * smooth(UNROLLED, 1.0, on_map);
                            MapLook::standing_unless(&grown, &shown, frame, (stand, 1), &stop)?
                                .scene(MapMoment::Still, frame)
                                .flatten()
                        } else {
                            // Only the land unrolls: the dots slide and the wonders ride along
                            // on their own, and the weather waits for the map.
                            let flat = kept.look(parts.flat, || {
                                MapLook::unless(&grown, &parts.look, parts.flat, &stop)
                            })?;
                            let globe = parts.planet(kept, &grown)?;
                            let through = on_map / UNROLLED;
                            let unrolled =
                                globe.unrolled(facing, through, side, &flat.land.picture);
                            unrolled?.to_image()
                        };
                        Scene::single(Picture::new(frame, picture))
                    }
                    (Level::Planet, Level::Universe, Some(sky))
                    | (Level::Universe, Level::Planet, Some(sky)) => {
                        // Each end drawn as it shows there: the planet as it was or will be
                        // on its level, the universe on its own.
                        let (z, planet_t, universe_t) = if zoom.to == Level::Universe {
                            (1.0 - k, times.from, times.to)
                        } else {
                            (k, times.to, times.from)
                        };
                        let on_planet = Globe::RADIUS_ON_MAP * size.1 as f64 / 546.0;
                        let globe = parts.planet(kept, &grown)?;
                        let turned = planet::turned(&globe.planet, facing.lon, turning);
                        let facing = Facing {
                            view: crate::world::View::stepped(turned),
                            side,
                            size,
                            moment: planet::Moment {
                                drift: turning,
                                ..moment(planet_t)
                            },
                            sky: None,
                        };
                        let home = globe.shot(facing, on_planet);
                        let universe = kept.cosmos(sky.key, || sky.make());
                        universe.zoom_from_planet(size.0, size.1, &home, z, universe_t)
                    }
                    (from, to, Some(sky)) => {
                        let (from, to) = (cosmos_level(from)?, cosmos_level(to)?);
                        let view = |level| cosmos::View {
                            still,
                            ringed,
                            ..cosmos::View::new(level, size.0, size.1)
                        };
                        let universe = kept.cosmos(sky.key, || sky.make());
                        universe.zoom(&view(from), &view(to), k, times)
                    }
                    _ => return None,
                };
                scene.moves = true;
                Some(Made {
                    scene,
                    world: None,
                    globe: None,
                })
            })
    }

    /// A drawn scene with what the list lights and the ring round what the pointer found.
    fn marked(&self, drawn: &Drawn) -> Scene {
        let mut scene = (*drawn.scene).clone();
        let marks = self.drawings.marks();
        if let Some(layer) = marks.lit(&scene, &self.lit()) {
            scene.add_layer(layer);
        }
        let ringed = self
            .ringed()
            .and_then(|at| u32::try_from(at).ok())
            .map(Pointed::Action)
            .or(self.pointed);
        if let Some(sprite) = ringed.and_then(|pointed| marks.ring(&scene, pointed)) {
            scene.add_sprite(sprite);
        }
        scene
    }

    /// The world drawn for the level shown, where the pointer points into what it drew.
    pub(super) fn drawn_here(&self) -> Option<Drawn> {
        self.draws_world()
            .then(|| self.drawings.current())
            .flatten()
    }
}

/// Where an action's dot falls among the braille dots on the planet, while it is on the near
/// side, as braille draws the planet.
fn braille_on_planet(x: f32, y: f32, view: crate::world::View, fit: Fit) -> Option<(f32, f32)> {
    let (px, py) = super::braille::planet_dot(x, y, view, fit)?;
    Some((px as f32, py as f32))
}

impl BehaviorsTab {
    /// Which way the planet faces: the terminal's step nearest to where it was turned.
    pub fn planet_view(&self) -> crate::world::View {
        let steps = crate::world::View::STEPS as f64;
        crate::world::View::stepped(f64::from(self.turn) * std::f64::consts::TAU / steps)
    }

    /// Turns the planet `by` of the terminal's steps, east to west as a drag to the left turns
    /// it. Whether it turned.
    pub fn turn_planet(&mut self, by: i32) -> bool {
        if self.level != Level::Planet || by == 0 {
            return false;
        }
        self.turn = (self.turn + by).rem_euclid(crate::world::View::STEPS as i32);
        self.pointed = None;
        true
    }

    /// The pointer moved to `to` with its button held. On the planet the globe turns with it, a
    /// radian for each of its radii the pointer went, to the nearest of the terminal's steps,
    /// and what is left over waits for the drag to go on. Whether it turned.
    pub fn drag(&mut self, to: MapPoint) -> bool {
        let from = self.held.replace(to);
        if self.level != Level::Planet {
            self.turning = 0.0;
            return false;
        }
        let radius = self.planet_radius(to.height);
        let Some(from) = from.filter(|_| radius > 0.0) else {
            return false;
        };
        // Dragging right turns the planet west, as a globe under the hand turns.
        self.turning -= (to.x - from.x) / radius;
        let step = std::f32::consts::TAU / crate::world::View::STEPS as f32;
        let steps = (self.turning / step).round();
        self.turning -= steps * step;
        self.turn_planet(steps as i32)
    }

    /// The planet's radius over a picture `height` tall, in the same units, as it is drawn.
    fn planet_radius(&self, height: f32) -> f32 {
        use crate::world::{Globe, Plate};
        let share = if self.draws_world() {
            Globe::RADIUS_ON_MAP / Plate::MAP.height()
        } else {
            super::braille::planet_radius(self.fit()) / f64::from(dots(self.fit()).1)
        };
        share as f32 * height
    }

    /// A turn of the wheel at `at`: one level in, toward what is under the pointer, or one out.
    /// A trackpad sends many turns for one swipe, so the turns that come within [`WHEEL_GAP`]
    /// of one that zoomed are let go, and one swipe goes one level. Whether what shows changed.
    pub fn scroll(&mut self, at: MapPoint, up: bool, now: Instant) -> bool {
        let pointed = self.point(Some(at));
        let soon = self
            .wheeled
            .is_some_and(|then| now.saturating_duration_since(then) < WHEEL_GAP);
        if soon {
            return pointed;
        }
        // The zoom is timed from now, however long ago the last frame was.
        self.moment.now = now;
        if !self.wheel(up) {
            return pointed;
        }
        self.wheeled = Some(now);
        true
    }
}
