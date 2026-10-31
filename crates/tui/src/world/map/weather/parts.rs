//! The weather as parts: each mark drawn alone in a box of its own at a moment, named so the
//! screen keeps it from one moment to the next, and keyed by the frame it shows, so a part that
//! has not changed is never sent again and one that has costs only its own small picture. The
//! two pictures over the land are these parts laid together.

use std::collections::HashMap;
use std::sync::PoisonError;

use super::water::spread;
use super::{
    Pad, Standing, Storm, Water, Weather, fire, hurricane, quake, shade_run, thunder, tornado,
    volcano,
};
use crate::base::draw::scene::{Depth, Image, Layer, PartId, Picture, Sprite, picture_key};
use crate::world::map::fields::{HEIGHT, WIDTH};
use crate::world::map::motion::{
    CLOUDS, CURRENT, FIRE, LIGHTNING, Moment, Motion, QUAKE, RIVER, SMOKE, TORNADO, TSUNAMI,
};
use crate::world::map::nature::wind;
use crate::world::raster::{Canvas, Colour};

/// Where a part lies: under the dots, as water and what stands on the land do, or over them, as
/// the storms and the earthquakes' rings do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Over {
    Ground,
    Dots,
}

/// One mark of the weather drawn alone at a moment.
#[derive(Debug, Clone)]
pub struct Part {
    pub over: Over,
    /// Its name, the same from one moment to the next.
    pub id: PartId,
    /// The frame it shows: the same frame draws the same picture.
    pub frame: u64,
    pub pad: Pad,
}

/// The frame a motion shows at a moment, or the still pose's own.
fn frame_of(moment: Moment, motion: Motion) -> u64 {
    moment.frame(motion).map_or(u64::MAX, u64::from)
}

/// A pad over a box, its corner on whole pixels.
fn pad_over([x0, y0, x1, y1]: [f64; 4]) -> Pad {
    let (left, top) = (x0.floor(), y0.floor());
    let width = (x1.ceil() - left).clamp(1.0, 4.0 * WIDTH as f64) as u32;
    let height = (y1.ceil() - top).clamp(1.0, 4.0 * HEIGHT as f64) as u32;
    Pad::new(left, top, width, height)
}

fn union(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// How a part is drawn into its pad.
type Draw<'a> = Box<dyn Fn(&mut Pad) + 'a>;

/// One mark of the weather planned at a moment: where it lies, the frame it shows and how it is
/// drawn, so a frame drawn before is found by its key before anything is drawn.
struct Planned<'a> {
    over: Over,
    id: PartId,
    frame: u64,
    area: Area,
    draw: Draw<'a>,
}

fn planned<'a>(
    (over, name, index): (Over, &str, usize),
    frame: u64,
    area: Area,
    draw: impl Fn(&mut Pad) + 'a,
) -> Planned<'a> {
    Planned {
        over,
        id: PartId::new(name, index as u32),
        frame,
        area,
        draw: Box::new(draw),
    }
}

impl Planned<'_> {
    fn drawn(self) -> Part {
        let mut pad = self.area.pad();
        (self.draw)(&mut pad);
        Part {
            over: self.over,
            id: self.id,
            frame: self.frame,
            pad,
        }
    }
}

/// The box a part is drawn in: a box on the map, its corner on whole pixels, or the whole map.
#[derive(Debug, Clone, Copy)]
enum Area {
    Box([f64; 4]),
    Whole,
}

impl Area {
    fn pad(self) -> Pad {
        match self {
            Area::Box(area) => pad_over(area),
            Area::Whole => Pad::whole(),
        }
    }

    /// The corner its pad starts from, as [`Area::pad`] places it.
    fn corner(self) -> (f64, f64) {
        match self {
            Area::Box([x0, y0, ..]) => (x0.floor(), y0.floor()),
            Area::Whole => (0.0, 0.0),
        }
    }
}

/// Bytes of pixels the frames of the parts' loops keep at most, for one look of one map: room
/// for every loop of a busy map, whose loops take about 33 MB at 8 steps a second and twice that
/// at 16, so after the first time round a step draws nothing.
pub(super) const LOOPS_KEPT: usize = 80 << 20;
/// A frame bigger than this is kept only as its part's last frame: a loop of them would fill the
/// room alone.
pub(super) const LOOP_FRAME_AT_MOST: usize = 1 << 20;
/// How big the clouds' shade over the whole map is drawn, against the map's own size each way,
/// for the terminal to stretch over the map's cells: its loop of 48 frames then takes 30 MB at
/// the usual cell rather than 120, and is kept whole.
const SHADOWS_SCALE: f64 = 0.5;
/// The clouds' shade's name on the screen.
const SHADOWS: (Over, &str, usize) = (Over::Ground, "cloud shadows", 0);
/// Every river and current, flowing on the one loop they share.
const WATER: (Over, &str, usize) = (Over::Ground, "water", 0);
const _: () = assert!(RIVER.frames == CURRENT.frames && RIVER.seconds == CURRENT.seconds);

/// The parts drawn so far, by the key of the frame each showed: each part's last frame, and the
/// frames of the loops the parts play, so a loop that comes round again is placed without
/// being drawn again. Frames are kept until the room is full and none is let go before the
/// weather is: a loop played in order would otherwise lose each frame just before it came
/// round again.
#[derive(Debug, Default)]
pub(super) struct Frames {
    last: HashMap<PartId, (u64, Image)>,
    loops: HashMap<u64, Image>,
    bytes: usize,
}

impl Frames {
    fn get(&self, id: PartId, key: u64) -> Option<Image> {
        match self.last.get(&id) {
            Some((last, image)) if *last == key => Some(image.clone()),
            _ => self.loops.get(&key).cloned(),
        }
    }

    fn keep(&mut self, id: PartId, key: u64, image: &Image) {
        self.last.insert(id, (key, image.clone()));
        let bytes = image.width() * image.height() * 4;
        let room = LOOPS_KEPT.saturating_sub(self.bytes);
        if bytes > LOOP_FRAME_AT_MOST || bytes > room || self.loops.contains_key(&key) {
            return;
        }
        self.loops.insert(key, image.clone());
        self.bytes += bytes;
    }

    /// Bytes of pixels the loops' frames keep.
    #[cfg(test)]
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Weather {
    /// Every part of the weather planned at a moment, in the order they stack.
    fn plan(&self, moment: Moment) -> Vec<Planned<'_>> {
        let mut plans = Vec::new();
        if let Some(clouds) = &self.clouds {
            let frame = frame_of(moment, CLOUDS);
            plans.push(planned(SHADOWS, frame, Area::Whole, move |pad| {
                clouds.draw(pad, moment);
            }));
        }
        // The rivers and the currents flow on one loop, so they go as one moving thing: a
        // picture a frame round all of them, and a step is a single placement.
        let flowing: Vec<&Water> = self
            .water
            .iter()
            .filter(|water| matches!(water, Water::River(_) | Water::Current(_)))
            .collect();
        let area = flowing
            .iter()
            .filter_map(|water| match water {
                Water::River(river) => Some(river.bounds()),
                Water::Current(lanes) => Some(lanes.bounds(self)),
                Water::Tsunami(_) => None,
            })
            .reduce(union);
        if let Some(area) = area {
            plans.push(planned(
                WATER,
                frame_of(moment, RIVER),
                Area::Box(area),
                move |pad| {
                    for water in &flowing {
                        match water {
                            Water::River(river) => river.flecks(pad, moment),
                            Water::Current(lanes) => lanes.flow(pad, self, moment),
                            Water::Tsunami(_) => {}
                        }
                    }
                },
            ));
        }
        for (index, moving) in self.water.iter().enumerate() {
            if let Water::Tsunami(wave) = moving {
                let (area, _) = wave.bounds(self, moment);
                let shore = wave.shore().map(|(x, y)| self.screen(x, y, 0.0));
                let area = Area::Box(union(area, spread(shore, 10.0)));
                let name = (Over::Ground, "tsunami", index);
                plans.push(planned(name, frame_of(moment, TSUNAMI), area, move |pad| {
                    wave.draw(pad, self, moment);
                }));
            }
        }
        for (index, flying) in self.fliers.iter().enumerate() {
            let area = Area::Box(self.storm_bounds(flying, true));
            let frame = self.storm_frame(flying, moment);
            let name = (Over::Ground, "storm shade", index);
            plans.push(planned(name, frame, area, move |pad| {
                self.draw_storm(pad, flying, moment, true);
            }));
        }
        for (index, standing) in self.upright.iter().enumerate() {
            let (area, frame, draw) = self.standing_part(standing, moment);
            let name = (Over::Ground, "standing", index);
            plans.push(planned(name, frame, Area::Box(area), draw));
        }
        for (index, &(place, rings)) in self.quakes.iter().enumerate() {
            let (x, y) = self.screen(place.0, place.1, 0.0);
            let area = Area::Box([x - 44.0, y - 36.0, x + 44.0, y + 36.0]);
            let name = (Over::Dots, "earthquake", index);
            plans.push(planned(name, frame_of(moment, QUAKE), area, move |pad| {
                quake::draw(pad, self, place, rings, moment);
            }));
        }
        for (index, flying) in self.fliers.iter().enumerate() {
            let area = Area::Box(self.storm_bounds(flying, false));
            let frame = self.storm_frame(flying, moment);
            let name = (Over::Dots, "storm", index);
            plans.push(planned(name, frame, area, move |pad| {
                self.draw_storm(pad, flying, moment, false);
            }));
        }
        plans
    }

    /// Lets go of every frame drawn so far, as when the map shows another look: the frames
    /// are drawn again should this one come back.
    pub fn let_frames_go(&self) {
        *self.frames.lock().unwrap_or_else(PoisonError::into_inner) = Frames::default();
    }

    /// Every part of the weather at a moment, in the order they stack.
    pub fn parts(&self, moment: Moment) -> Vec<Part> {
        self.plan(moment).into_iter().map(Planned::drawn).collect()
    }

    /// The parts laid together over the whole map, those under the dots or those over them.
    pub(super) fn laid(&self, moment: Moment, over: Over) -> Canvas {
        let mut canvas = Canvas::new(WIDTH as u32, HEIGHT as u32);
        for planned in self.plan(moment) {
            if planned.over == over {
                planned.drawn().pad.onto(&mut canvas);
            }
        }
        canvas
    }

    /// The parts as the screen's sprites, in the map's own pixels: under the dots as nature,
    /// over them as weather. `world` keys the crew and grouping they were drawn for, so a
    /// picture is sent once and placed whenever a moment shows it again. A frame drawn before
    /// is not drawn again: a step of a map whose loops have all come round draws nothing.
    pub fn sprites(&self, moment: Moment, world: u64) -> Vec<Sprite> {
        let mut frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        self.plan(moment)
            .into_iter()
            // The clouds' shade over the whole map goes as its own layer.
            .filter(|planned| !matches!(planned.area, Area::Whole))
            .filter_map(|planned| {
                let (left, top) = planned.area.corner();
                let key = picture_key([
                    world,
                    planned.id.get(),
                    planned.frame,
                    left.to_bits(),
                    top.to_bits(),
                ]);
                let (id, over) = (planned.id, planned.over);
                let image = match frames.get(id, key) {
                    Some(image) => image,
                    None => {
                        let part = planned.drawn();
                        let canvas = part.pad.canvas();
                        let (width, height) = (canvas.width() as usize, canvas.height() as usize);
                        let image = Image::new(width, height, canvas.to_rgba())?;
                        frames.keep(id, key, &image);
                        image
                    }
                };
                let depth = match over {
                    Over::Ground => Depth::Nature,
                    Over::Dots => Depth::Weather,
                };
                Some(Sprite {
                    id,
                    depth,
                    at: (left as i32, top as i32),
                    picture: Picture::new(key, image),
                })
            })
            .collect()
    }

    /// The clouds' shade at a moment as a layer over the whole map, under every other part
    /// on the ground, drawn smaller than the map for the terminal to stretch over its cells;
    /// `None` without clouds or held still. Like the parts, each frame is drawn once.
    pub fn shadows(&self, moment: Moment, world: u64) -> Option<Layer> {
        let clouds = self.clouds.as_ref()?;
        let (_, name, index) = SHADOWS;
        let id = PartId::new(name, index as u32);
        let frame = frame_of(moment, CLOUDS);
        let key = picture_key([world, id.get(), frame, SHADOWS_SCALE.to_bits()]);
        let mut frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        let image = match frames.get(id, key) {
            Some(image) => image,
            None => {
                let image = clouds.picture(moment, SHADOWS_SCALE)?.to_image();
                frames.keep(id, key, &image);
                image
            }
        };
        Some(Layer {
            id,
            depth: Depth::Nature,
            picture: Picture::new(key, image),
        })
    }

    /// The box a storm's cloud or its shade fills on the screen.
    fn storm_bounds(&self, flying: &super::Flying, shade: bool) -> [f64; 4] {
        let (run, (ux, uy)) = shade_run();
        let (px, py) = flying.over;
        let (w, h) = (
            f64::from(flying.picture.width()),
            f64::from(flying.picture.height()),
        );
        match flying.storm {
            Storm::Hurricane(_) => {
                let (x, y) = if shade {
                    let fall = hurricane::ALTITUDE * run;
                    self.screen(px + ux * fall, py + uy * fall, 0.0)
                } else {
                    self.screen(px, py, hurricane::ALTITUDE)
                };
                let reach = w.hypot(h) / 2.0 + 4.0;
                [x - reach, y - reach, x + reach, y + reach]
            }
            Storm::Thunder { radius, .. } => {
                if shade {
                    let fall = thunder::ALTITUDE * run;
                    let (x, y) = self.screen(px + ux * fall, py + uy * fall, 0.0);
                    let r = radius * 0.9 + 2.0;
                    return [x - r, y - r, x + r, y + r];
                }
                let (x, y) = self.screen(px, py, thunder::ALTITUDE);
                let (gx, gy) = self.screen(px, py, 0.0);
                let size = thunder::size(radius);
                let cloud = [
                    x - flying.anchor.0,
                    y - flying.anchor.1,
                    x - flying.anchor.0 + w,
                    y - flying.anchor.1 + h,
                ];
                let reach = size * 0.95 + 2.0;
                let glow = [
                    x - reach,
                    y - size * 0.55 - reach,
                    x + reach,
                    y - size * 0.55 + reach,
                ];
                let bolt = [
                    x.min(gx) - size - 8.0,
                    y - 2.0,
                    x.max(gx) + size + 8.0,
                    gy + 6.0,
                ];
                union(union(cloud, glow), bolt)
            }
        }
    }

    /// The frame a storm shows at a moment: a hurricane's turn, a thunderstorm's flashes.
    fn storm_frame(&self, flying: &super::Flying, moment: Moment) -> u64 {
        match (flying.storm, moment) {
            (_, Moment::Still) => u64::MAX,
            (Storm::Hurricane(storm), _) => u64::from(hurricane::frame(&storm, moment)),
            (Storm::Thunder { .. }, _) => frame_of(moment, LIGHTNING),
        }
    }

    /// Something standing on the land: the box it is drawn in, the frame it shows, and how it
    /// is drawn alone.
    fn standing_part<'a>(
        &'a self,
        standing: &'a Standing,
        moment: Moment,
    ) -> ([f64; 4], u64, Draw<'a>) {
        match standing {
            Standing::Tornado(feature) => {
                let r = feature.radius - 4.0;
                let (x, y) = self.screen(feature.x, feature.y, 0.0);
                let area = [
                    x - 1.3 * r - 3.0,
                    y - 2.9 * r - 3.0,
                    x + 1.9 * r + 3.0,
                    y + 0.8 * r + 3.0,
                ];
                let draw = move |pad: &mut Pad| tornado::draw(pad, self, feature, moment);
                (area, frame_of(moment, TORNADO), Box::new(draw))
            }
            Standing::Volcano {
                feature,
                cone,
                smoking,
            } => {
                let (x, y) = self.screen(feature.x, feature.y, 0.0);
                let r = feature.radius;
                let (w, h) = (
                    f64::from(cone.canvas.width()),
                    f64::from(cone.canvas.height()),
                );
                let mut area = [
                    x - cone.foot.0,
                    y - cone.foot.1,
                    x - cone.foot.0 + w,
                    y - cone.foot.1 + h,
                ];
                if *smoking {
                    let top = y - cone.height;
                    area = union(area, [x - r * 1.5, top - r * 4.2, x + r * 4.2, y + r]);
                }
                let place = (feature.x, feature.y);
                let draw = move |pad: &mut Pad| {
                    volcano::draw(pad, self, cone, place, r, *smoking, moment);
                };
                let frame = if *smoking { frame_of(moment, SMOKE) } else { 0 };
                (area, frame, Box::new(draw))
            }
            Standing::Fire { feature, front } => {
                let points = front.iter().map(|&(x, y)| self.screen(x, y, 0.0));
                let [x0, y0, x1, y1] = spread(points, 12.0);
                let (wx, _) = wind();
                let area = [x0 - 24.0, y0 - 70.0, x1 + 90.0 * wx.max(0.5), y1 + 24.0];
                let draw = move |pad: &mut Pad| {
                    fire::draw(pad, self, front, feature.behavior, moment);
                };
                let frame = frame_of(moment, FIRE)
                    .wrapping_mul(64)
                    .wrapping_add(frame_of(moment, SMOKE));
                (area, frame, Box::new(draw))
            }
            Standing::Wonder(wonder) => {
                let (gx, gy) = wonder.placed.ground;
                let (x, y) = self.screen(gx, gy, 0.0);
                let sprite = &wonder.sprite;
                let (w, h) = (
                    f64::from(sprite.canvas.width()),
                    f64::from(sprite.canvas.height()),
                );
                let (left, top) = (x - sprite.ground.0, y - sprite.ground.1);
                let draw = move |pad: &mut Pad| {
                    let brush = pad.brush(Colour::WHITE);
                    pad.draw(&sprite.canvas, left, top, &brush);
                };
                ([left, top, left + w, top + h], 0, Box::new(draw))
            }
        }
    }
}
