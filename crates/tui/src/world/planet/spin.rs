//! The planet as the terminal shows it: one picture each step of the turn, 36 a turn where the
//! world steps 8 times a second and as many more as it steps more often while a whole turn of
//! them fits in what is kept, each drawn the first time the turn or a drag reaches it and kept,
//! so a second turn costs only placements.
//!
//! The weather is drawn into each of those pictures at a moment fixed by its step, each motion's
//! speed rounded so one turn holds whole loops: nothing jumps when the turn starts over, and the
//! same step always shows the same picture. While the turn holds (the pointer is on the planet, a
//! drag just ended, or the crew stands still), what moves on that one picture plays its own small
//! loop instead. Where the clouds are kept apart they drift over the ground on pictures of their
//! own, one each step of their drift against the ground, so they too are drawn once.

use std::collections::HashMap;

use super::standing::{self, Standing};
use super::{Bounds, CloudsIn, Framed, Planet, Side, Sun, plain_sky};
use crate::base::draw::geo::{Globe, Plate, View};
use crate::base::draw::scene::{Depth, Image, Layer, PartId, Picture, Scene, Sprite, picture_key};
use crate::world::deliver;
use crate::world::deliver::clock::Rate;
use crate::world::planet_fx::{self, GlobeGround, Motion, Weather};
use crate::world::raster::{Brush, Canvas, Colour};

/// Tells the planet's kinds of picture apart in their keys.
const SKY: u64 = 0x504c_534b;
const GROUND: u64 = 0x504c_4752;
const CLOUDS: u64 = 0x504c_434c;
const WEATHER: u64 = 0x504c_5754;
const DOTS: u64 = 0x504c_4454;
/// The planet filling its own level, and your planet small in the universe.
const WHOLE: u64 = 0x504c_574c;
const HOME: u64 = 0x504c_484d;

const SKY_PART: PartId = PartId::new("planet sky", 0);

/// Where the world's clocks stand for one scene.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Moment {
    /// Seconds on the world's motion clock, which steps at the world's rate.
    pub t: f64,
    /// Seconds the clouds have drifted: their clock stops while the turn holds.
    pub drift: f64,
    /// The turn holds: the pointer is on the planet or a drag just turned it.
    pub holding: bool,
    /// Motion is reduced or off: nothing turns or moves, and each thing holds its still pose.
    pub still: bool,
    /// How often the world steps, which the turn's pictures and each loop's pictures follow.
    pub rate: Rate,
}

/// What one scene of the planet shows.
#[derive(Debug, Clone, Copy)]
pub struct Shot<'a> {
    /// Which way the planet faces: it shows the nearest of [`Spin::steps`].
    pub view: View,
    pub side: Side,
    /// The scene's pixels, its cells times the cell size.
    pub size: (usize, usize),
    /// Whether the clouds lie apart, where the terminal stacks pictures and they can drift.
    pub clouds: CloudsIn,
    /// What moves on the planet, in an order kept from one scene to the next.
    pub weather: &'a [&'a dyn Weather],
    /// What stands upright on it, as the wonders do.
    pub standing: &'a [&'a dyn Standing],
    pub moment: Moment,
    /// The universe's sky behind the planet, or the plain one.
    pub sky: Option<&'a Picture>,
}

/// Your planet as the universe shows it: which way it faces, its middle and radius in the
/// universe's scene `size` px, and what moves on it.
#[derive(Debug, Clone, Copy)]
pub struct Home<'a> {
    pub view: View,
    pub side: Side,
    pub centre: (f64, f64),
    pub radius: f64,
    pub size: (usize, usize),
    pub clouds: CloudsIn,
    pub weather: &'a [&'a dyn Weather],
    pub moment: Moment,
}

/// A globe to draw the pictures of: the planet's own level (`whole`, with its dots) or your
/// planet small in the universe.
struct Turn<'a> {
    globe: Globe,
    size: (usize, usize),
    side: Side,
    clouds: CloudsIn,
    weather: &'a [&'a dyn Weather],
    standing: &'a [&'a dyn Standing],
    moment: Moment,
    whole: bool,
}

/// The pictures a planet's scenes are made of, kept by key.
#[derive(Debug)]
pub struct Spin {
    kept: HashMap<u64, Kept>,
    bytes: usize,
    /// Bytes of pixels it keeps at most; when `None`, as many as the terminal keeps for scenes
    /// the size of the last one, so a picture the terminal still holds is seldom drawn again.
    limit: Option<usize>,
    /// The size of the last scene asked for.
    size: (usize, usize),
    uses: u64,
    /// The count of uses the scene being made started from: what it has used since stays.
    scene_from: u64,
    /// How many pictures it has drawn.
    drawn: u64,
    /// Each held step's loop boxes, by what they were worked out for.
    boxes: HashMap<u64, Option<Box>>,
}

#[derive(Debug, Clone)]
struct Kept {
    image: Image,
    /// How far into its scene's box it starts, where it was cut to what it shows.
    inset: (usize, usize),
    used: u64,
    /// The turn it belongs to: one look of the planet at one size and side.
    turn: u64,
}

/// A part of the scene in whole pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Box {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

impl Default for Spin {
    fn default() -> Self {
        Self::new()
    }
}

impl Spin {
    pub fn new() -> Self {
        Self {
            kept: HashMap::new(),
            bytes: 0,
            limit: None,
            size: (0, 0),
            uses: 0,
            scene_from: 0,
            drawn: 0,
            boxes: HashMap::new(),
        }
    }

    /// A spin that keeps at most `bytes` of pixels.
    pub fn with_limit(bytes: usize) -> Self {
        Self {
            limit: Some(bytes),
            ..Self::new()
        }
    }

    /// How many pictures it has drawn so far.
    pub fn drawn(&self) -> u64 {
        self.drawn
    }

    /// Bytes of pixels it keeps at most for scenes the size of the last one.
    pub fn limit(&self) -> usize {
        self.limit_at(self.size)
    }

    fn limit_at(&self, size: (usize, usize)) -> usize {
        self.limit.unwrap_or_else(|| {
            let map = Plate::MAP.width() * Plate::MAP.height();
            let scale = (size.0 * size.1) as f64 / map;
            deliver::kept_bytes(scale)
        })
    }

    /// The pictures a turn of `planet` filling a scene `size` px holds at `rate`, its clouds
    /// lying as `clouds` says.
    pub fn steps(
        &self,
        planet: &Planet,
        size: (usize, usize),
        clouds: CloudsIn,
        rate: Rate,
    ) -> u32 {
        let globe = planet.globe(View::stepped(0.0), size);
        self.steps_of(planet, &globe, size, clouds, true, rate)
    }

    /// [`turn_steps`] at `rate` where a whole turn of them fits in what it keeps for a scene
    /// `size` px, so a second turn costs only placements; else one each 10°.
    fn steps_of(
        &self,
        planet: &Planet,
        globe: &Globe,
        size: (usize, usize),
        clouds: CloudsIn,
        whole: bool,
        rate: Rate,
    ) -> u32 {
        let steps = turn_steps(rate);
        let bounds = planet.bounds(globe, size);
        let apart = clouds == CloudsIn::Apart && planet.skin.clouds.is_some();
        // The ground each step, and the clouds and the dots on pictures of their own where the
        // clouds lie apart, each at most the globe's box of four bytes a pixel.
        let pictures = 1 + usize::from(apart) * (1 + usize::from(whole));
        let bytes = steps as usize * pictures * bounds.width * bounds.height * 4;
        if bytes <= self.limit_at(size) {
            steps
        } else {
            steps.min(View::STEPS)
        }
    }

    /// How many pictures it keeps.
    pub fn kept(&self) -> usize {
        self.kept.len()
    }

    /// How many bytes of pixels it keeps.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The planet's scene for `shot`: the sky behind, the ground at the step it faces, the
    /// clouds where they lie apart, what moves while the turn holds, and the dots.
    pub fn scene(&mut self, planet: &Planet, shot: &Shot<'_>) -> Scene {
        let size = shot.size;
        self.size = size;
        self.scene_from = self.uses;
        let mut scene = Scene::new(size.0, size.1);
        let sky = match shot.sky {
            Some(sky) => sky.clone(),
            None => {
                let key = picture_key([SKY, size.0 as u64, size.1 as u64]);
                Picture::new(key, self.keep(key, key, || plain_sky(size).to_image()))
            }
        };
        scene.add_layer(Layer {
            id: SKY_PART,
            depth: Depth::Sky,
            picture: sky,
        });
        let globe = planet.globe(shot.view, size);
        let turn = Turn {
            globe,
            size,
            side: shot.side,
            clouds: shot.clouds,
            weather: shot.weather,
            standing: shot.standing,
            moment: shot.moment,
            whole: true,
        };
        let (sprites, moves) = self.parts(planet, &turn);
        for sprite in sprites {
            scene.add_sprite(sprite);
        }
        scene.moves = moves;
        scene
    }

    /// Your planet as the universe shows it, at its place and size there: the globe and its
    /// air, turning, with the clouds and whatever moves on it that still reads at that size,
    /// as sprites to stack among the universe's own. Whether any of it moves comes with them.
    pub fn home(&mut self, planet: &Planet, home: &Home<'_>) -> (Vec<Sprite>, bool) {
        self.scene_from = self.uses;
        let globe = Globe::new(home.view, home.centre, home.radius);
        let turn = Turn {
            globe,
            size: home.size,
            side: home.side,
            clouds: home.clouds,
            weather: home.weather,
            standing: &[],
            moment: home.moment,
            whole: false,
        };
        self.parts(planet, &turn)
    }

    /// The pictures of a turning globe: its ground at the step it faces, with the weather
    /// drawn in at the step's moment while it turns, held still, or bare of what moves while
    /// the turn holds and the loops play over it; the clouds where they lie apart; the loops;
    /// and the dots, on their own picture when anything lies between them and the ground.
    fn parts(&mut self, planet: &Planet, turn: &Turn<'_>) -> (Vec<Sprite>, bool) {
        let steps = self.steps_of(
            planet,
            &turn.globe,
            turn.size,
            turn.clouds,
            turn.whole,
            turn.moment.rate,
        );
        // Each step's picture is drawn facing that step, whatever view between steps asked.
        let globe = Globe {
            view: turn.globe.view.on_steps(steps),
            ..turn.globe
        };
        let size = turn.size;
        let side = side_number(turn.side);
        let step = u64::from(globe.view.step_in(steps));
        let bounds = planet.bounds(&globe, size);
        let ground = GlobeGround::new(globe, turn.side).onto(bounds.left as f64, bounds.top as f64);
        let moment = turn.moment;
        let still = moment.still;
        let holding = moment.holding || planet.turn_seconds().is_none();
        let moving: Vec<(usize, &dyn Weather)> = turn
            .weather
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, thing)| thing.moves_on(globe.radius))
            .collect();
        let loops = !still && holding && !moving.is_empty();
        let apart = turn.clouds == CloudsIn::Apart && planet.skin.clouds.is_some();
        let dots_apart = turn.whole && (apart || loops);
        let place = picture_key([
            if turn.whole { WHOLE } else { HOME },
            planet.key,
            side,
            size.0 as u64,
            size.1 as u64,
            globe.centre.0.to_bits(),
            globe.centre.1.to_bits(),
            globe.radius.to_bits(),
            globe.view.tilt.to_bits(),
        ]);
        let mut sprites = Vec::new();

        let pose = if still {
            Pose::Still
        } else if loops {
            Pose::Bare
        } else {
            Pose::Turning
        };
        let weather = picture_key(
            turn.weather
                .iter()
                .map(|thing| thing.fingerprint())
                .chain(turn.standing.iter().map(|thing| thing.fingerprint())),
        );
        let key = picture_key([
            GROUND,
            place,
            step,
            weather,
            pose as u64,
            u64::from(apart),
            u64::from(dots_apart),
        ]);
        let image = self.keep(key, place, || {
            let clouds = if apart {
                CloudsIn::Apart
            } else {
                CloudsIn::Ground
            };
            let mut canvas = if turn.whole {
                planet.ground(&globe, size, turn.side, clouds, 0.0).canvas
            } else {
                planet
                    .bare_ground(&globe, size, turn.side, clouds, 0.0)
                    .canvas
            };
            let shown: Vec<&dyn Weather> = turn
                .weather
                .iter()
                .copied()
                .filter(|thing| !loops || !thing.moves_on(globe.radius))
                .collect();
            let at = |thing: &dyn Weather| {
                if still {
                    0.0
                } else {
                    turning_moment(planet, thing, step, steps, moment.rate)
                }
            };
            planet_fx::draw_all(&mut canvas, &ground, &shown, at, still);
            let origin = (bounds.left as f64, bounds.top as f64);
            standing::draw(
                &mut canvas,
                turn.standing,
                &globe,
                Sun::of(turn.side),
                origin,
            );
            if turn.whole && !dots_apart {
                planet.dots(&globe, size, turn.side).lay_on_frame(
                    &mut canvas,
                    bounds.left,
                    bounds.top,
                );
            }
            canvas.to_image()
        });
        sprites.push(Sprite {
            id: part(turn.whole, Piece::Ground, 0),
            depth: if turn.whole {
                Depth::Ground
            } else {
                Depth::Dots
            },
            at: corner(bounds.left, bounds.top),
            picture: Picture::new(key, image),
        });

        if apart {
            // The clouds seen against the ground they drifted east over: as the ground turned
            // back by the steps they drifted.
            let step_angle = std::f64::consts::TAU / f64::from(steps);
            let drifted = (shown_drift(turn.clouds, moment, steps) / step_angle).round() as i64;
            let index = (step as i64 - drifted).rem_euclid(i64::from(steps)) as u64;
            let key = picture_key([CLOUDS, place, index]);
            let (image, inset) = self.keep_trimmed(key, place, || {
                let view = View {
                    lon: index as f64 * step_angle,
                    tilt: globe.view.tilt,
                };
                let turned = Globe { view, ..globe };
                planet
                    .clouds(&turned, size, turn.side, 0.0)
                    .map(|framed| framed.canvas)
            });
            sprites.push(Sprite {
                id: part(turn.whole, Piece::Clouds, 0),
                depth: if turn.whole {
                    Depth::Weather
                } else {
                    Depth::Dots
                },
                at: corner(bounds.left + inset.0, bounds.top + inset.1),
                picture: Picture::new(key, image),
            });
        }

        if loops {
            for (index, thing) in moving {
                let looped = picture_key([place, step, index as u64, thing.fingerprint()]);
                let motion = thing.motion().at(moment.rate);
                let Some(area) = self.loop_box(looped, thing, motion, &ground, bounds) else {
                    continue;
                };
                let picture = motion.picture_at(moment.t);
                let key = picture_key([WEATHER, looped, u64::from(picture)]);
                let image = self.keep(key, place, || {
                    let mut canvas = Canvas::new(area.width as u32, area.height as u32);
                    let local =
                        GlobeGround::new(globe, turn.side).onto(area.left as f64, area.top as f64);
                    thing.draw(&mut canvas, &local, motion.moment(picture), false);
                    canvas.to_image()
                });
                sprites.push(Sprite {
                    id: part(turn.whole, Piece::Weather, index as u32),
                    depth: if turn.whole {
                        Depth::Weather
                    } else {
                        Depth::Dots
                    },
                    at: corner(area.left, area.top),
                    picture: Picture::new(key, image),
                });
            }
        }

        if dots_apart {
            let key = picture_key([DOTS, place, step]);
            let (image, inset) = self.keep_trimmed(key, place, || {
                Some(planet.dots(&globe, size, turn.side).canvas)
            });
            sprites.push(Sprite {
                id: part(turn.whole, Piece::Dots, 0),
                depth: Depth::Dots,
                at: corner(bounds.left + inset.0, bounds.top + inset.1),
                picture: Picture::new(key, image),
            });
        }

        let turning = planet.turn_seconds().is_some() && !moment.holding;
        let moves = !still && (turning || loops || (apart && !moment.holding));
        (sprites, moves)
    }

    /// The box every picture of one thing's loop fits in at a held step, worked out once by
    /// drawing all its pictures over each other; `None` where it draws nothing there.
    fn loop_box(
        &mut self,
        place: u64,
        thing: &dyn Weather,
        motion: Motion,
        ground: &GlobeGround,
        bounds: Bounds,
    ) -> Option<Box> {
        if let Some(found) = self.boxes.get(&place) {
            return *found;
        }
        let (left, top) = (bounds.left, bounds.top);
        let mut all = bounds.canvas();
        for picture in 0..motion.frames {
            thing.draw(&mut all, ground, motion.moment(picture), false);
        }
        let found = inked_box(&all).map(|inked| Box {
            left: left + inked.left,
            top: top + inked.top,
            ..inked
        });
        self.boxes.insert(place, found);
        found
    }

    /// The picture kept under `key`, drawn by `draw` the first time it is asked for or after it
    /// was let go, as part of `turn`.
    fn keep(&mut self, key: u64, turn: u64, draw: impl FnOnce() -> Image) -> Image {
        self.keep_at(key, turn, || (draw(), (0, 0))).0
    }

    /// As [`Spin::keep`], for a picture mostly clear round what it shows, such as the clouds or
    /// the dots: kept cut to the box round what `draw` drew, with how far into the full picture
    /// that box starts.
    fn keep_trimmed(
        &mut self,
        key: u64,
        turn: u64,
        draw: impl FnOnce() -> Option<Canvas>,
    ) -> (Image, (usize, usize)) {
        self.keep_at(key, turn, || match draw() {
            Some(canvas) => trimmed(&canvas),
            None => (Image::transparent(1, 1), (0, 0)),
        })
    }

    /// The picture kept under `key` with how far into its scene's box it starts.
    ///
    /// Once more are kept than the limit, the pictures of other turns go first, those used
    /// longest ago first. A turn bigger than the limit then keeps the steps it holds and lets
    /// go of the one used last: that one comes round again only after every other step, so the
    /// turn keeps the same share of its steps on every pass instead of losing each step just
    /// before it is wanted.
    fn keep_at(
        &mut self,
        key: u64,
        turn: u64,
        draw: impl FnOnce() -> (Image, (usize, usize)),
    ) -> (Image, (usize, usize)) {
        self.uses += 1;
        if let Some(kept) = self.kept.get_mut(&key) {
            kept.used = self.uses;
            kept.turn = turn;
            return (kept.image.clone(), kept.inset);
        }
        let (image, inset) = draw();
        self.drawn += 1;
        self.bytes += image.pixels().len();
        self.kept.insert(
            key,
            Kept {
                image: image.clone(),
                inset,
                used: self.uses,
                turn,
            },
        );
        let limit = self.limit();
        // What the scene being made already uses stays.
        let from = self.scene_from;
        while self.bytes > limit && self.kept.len() > 1 {
            let others = self
                .kept
                .iter()
                .filter(|(_, kept)| kept.used <= from && kept.turn != turn)
                .min_by_key(|(_, kept)| kept.used)
                .map(|(kept_key, _)| *kept_key);
            let own = || {
                self.kept
                    .iter()
                    .filter(|(_, kept)| kept.used <= from)
                    .max_by_key(|(_, kept)| kept.used)
                    .map(|(kept_key, _)| *kept_key)
            };
            let Some(gone) = others.or_else(own) else {
                break;
            };
            if let Some(gone) = self.kept.remove(&gone) {
                self.bytes -= gone.image.pixels().len();
            }
        }
        (image, inset)
    }
}

/// `canvas` cut to the box round what it shows, with how far in that box starts; a clear pixel
/// where it shows nothing.
fn trimmed(canvas: &Canvas) -> (Image, (usize, usize)) {
    match inked_box(canvas) {
        Some(inked) => (
            canvas
                .to_image()
                .cropped(inked.left, inked.top, inked.width, inked.height),
            (inked.left, inked.top),
        ),
        None => (Image::transparent(1, 1), (0, 0)),
    }
}

/// The pictures one turn holds at the world's `rate`: one each 10° where the world steps 8
/// times a second, and as many more as it steps more often, so the turn moves as smoothly as
/// the rest of the world.
pub fn turn_steps(rate: Rate) -> u32 {
    View::STEPS * rate.per_second() / 8
}

/// The moment a thing shows in the picture of `step` of a turn of `steps` while the planet
/// turns: as far through its loop as the turn is through its steps, its speed rounded so one
/// turn holds whole loops, and on one of its stored pictures at the world's `rate`.
pub fn turning_moment(
    planet: &Planet,
    thing: &dyn Weather,
    step: u64,
    steps: u32,
    rate: Rate,
) -> f64 {
    let motion = thing.motion().at(rate);
    let Some(turn) = planet.turn_seconds() else {
        return 0.0;
    };
    let loops = (turn / motion.seconds).round().max(1.0);
    let through = (step as f64 / f64::from(steps.max(1)) * loops).rem_euclid(1.0);
    let picture = (through * f64::from(motion.frames)).floor() as u32;
    motion.moment(picture)
}

/// How far east the clouds a scene shows have drifted, in radians: whole steps of a turn of
/// `steps` where they lie apart and motion is on, else none, as they turn with the ground.
pub fn shown_drift(clouds: CloudsIn, moment: Moment, steps: u32) -> f64 {
    if clouds != CloudsIn::Apart || moment.still {
        return 0.0;
    }
    let step = std::f64::consts::TAU / f64::from(steps.max(1));
    (planet_fx::drift(moment.drift) / step).floor() * step
}

/// Where the planet faces `seconds` after it faced `lon`, turning at the crew's pace, east to
/// west; a crew that stands still stays where it was.
pub fn turned(planet: &Planet, lon: f64, seconds: f64) -> f64 {
    match planet.turn_seconds() {
        Some(turn) => lon - seconds * std::f64::consts::TAU / turn,
        None => lon,
    }
}

/// How the ground's picture shows what moves on it: each thing in its still pose, left out for
/// its own loop to play over, or at the moment its step fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pose {
    Still,
    Bare,
    Turning,
}

/// The pictures a turning globe is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece {
    Ground,
    Clouds,
    Weather,
    Dots,
}

/// The name a part keeps from one scene to the next.
fn part(whole: bool, piece: Piece, index: u32) -> PartId {
    let name = match (whole, piece) {
        (true, Piece::Ground) => "planet ground",
        (true, Piece::Clouds) => "planet clouds",
        (true, Piece::Weather) => "planet weather",
        (true, Piece::Dots) => "planet dots",
        (false, Piece::Ground) => "home ground",
        (false, Piece::Clouds) => "home clouds",
        (false, Piece::Weather) => "home weather",
        (false, Piece::Dots) => "home dots",
    };
    PartId::new(name, index)
}

fn side_number(side: Side) -> u64 {
    match side {
        Side::Day => 0,
        Side::Night => 1,
    }
}

fn corner(left: usize, top: usize) -> (i32, i32) {
    (
        i32::try_from(left).unwrap_or(i32::MAX),
        i32::try_from(top).unwrap_or(i32::MAX),
    )
}

/// The smallest box round every pixel `canvas` drew on, a pixel wider each way.
fn inked_box(canvas: &Canvas) -> Option<Box> {
    let (width, height) = (canvas.width() as usize, canvas.height() as usize);
    let pixels = canvas.premultiplied();
    let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..height {
        for x in 0..width {
            if pixels[(y * width + x) * 4 + 3] > 0 {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
    }
    (left <= right).then(|| {
        let (left, top) = (left.saturating_sub(1), top.saturating_sub(1));
        let (right, bottom) = ((right + 1).min(width - 1), (bottom + 1).min(height - 1));
        Box {
            left,
            top,
            width: right - left + 1,
            height: bottom - top + 1,
        }
    })
}

impl Framed {
    /// Lays the picture over a canvas that covers the scene from (`left`, `top`).
    fn lay_on_frame(&self, canvas: &mut Canvas, left: usize, top: usize) {
        canvas.draw(
            &self.canvas,
            self.left as f64 - left as f64,
            self.top as f64 - top as f64,
            &Brush::new(Colour::WHITE),
        );
    }
}
