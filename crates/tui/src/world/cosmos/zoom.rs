//! The zoom between two levels, 0.7 s each way: the outer level grows round what you went into
//! while the inner one grows out of it and takes over, each over its own sky. And the zoom from
//! your planet out to the universe: the planet shrinks to its place in the middle while the
//! universe grows in round it.
//!
//! A zoom crops and scales pictures the screen keeps instead of sending a new picture a step.
//! Each level is drawn once without its sky, the one left as it was when the zoom began and
//! the one arrived at as it will be when the zoom ends, and every step stretches those over the
//! skies, which hold still. A terminal can neither fade a picture nor turn one, so each level
//! shows whole while it shows, the inner one over the outer, and the inner level's sky takes
//! over a little before halfway. With motion off there is no zoom: the screen shows the level
//! at once; reduced motion zooms with what moves in the levels held still.

use std::collections::HashMap;

use super::galaxy::AT_GALAXY;
use super::model::StarKind;
use super::motion;
use super::paint::{GIANT, core_radius};
use super::scenes::{Cosmos, Level, View};
use super::shapes;
use crate::world::raster::noise::{mix, smooth};
use crate::world::{Depth, Image, Layer, PartId, Picture, Scaled, Scene};

/// How far into a zoom, eased, the inner level's sky takes over from the outer one's.
const INNER_SKY: f64 = 0.4;
/// How far into a zoom the outer level shows, and from how far the inner one does: between
/// the two both show, the inner over the outer.
const OUTER_UNTIL: f64 = 0.5;
const INNER_FROM: f64 = 0.25;
/// From your planet the universe shows once it would have faded more than this far in.
const UNIVERSE_SHOWS: f64 = 0.3;
/// Levels and skies drawn for zooms that a cosmos keeps at most, room for a zoom's two of
/// each and some; past it they all go.
const BARES_KEPT: usize = 6;

const SKY: PartId = PartId::new("cosmos-zoom", 0);
const OUTER: PartId = PartId::new("cosmos-zoom", 1);
const INNER: PartId = PartId::new("cosmos-zoom", 2);
const PLANET: PartId = PartId::new("cosmos-zoom", 3);

/// When a zoom's two levels are drawn, in seconds since the world opened: the level it leaves
/// as it was when the zoom began, and the one it arrives at as it will be when the zoom ends,
/// so neither jumps where the zoom meets it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Times {
    pub from: f64,
    pub to: f64,
}

impl Times {
    /// Both levels as they are at `t`.
    pub fn at(t: f64) -> Self {
        Self { from: t, to: t }
    }
}

/// Your planet as one picture for the zoom between it and the universe to shrink or grow:
/// where its middle falls in the picture and its radius there, both in the picture's pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeShot {
    pub picture: Picture,
    pub centre: (f64, f64),
    pub radius: f64,
}

/// The levels and skies a cosmos has drawn for zooms, by picture key.
#[derive(Debug, Default)]
pub(super) struct Bares(HashMap<u64, Image>);

/// How one level's picture sits inside the next one in: the point of the outer picture that
/// becomes the inner one's middle, and how much bigger the inner level draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anchor {
    outer: (f64, f64),
    inner: (f64, f64),
    scale: f64,
}

/// How deep a level lies: the universe outermost.
fn depth(level: Level) -> u8 {
    match level {
        Level::Universe => 0,
        Level::Galaxy(_) => 1,
        Level::System(_) => 2,
    }
}

/// A level as two numbers for a picture's key: how deep it lies and what it shows.
fn level_key(level: Level) -> [u64; 2] {
    let focus = match level {
        Level::Universe => 0,
        Level::Galaxy(maker) => maker,
        Level::System(star) => star,
    };
    [depth(level).into(), focus as u64]
}

/// `picture` grown `scale` times with its `point` on `centre`.
fn stretched(
    id: PartId,
    depth: Depth,
    picture: Picture,
    point: (f64, f64),
    centre: (f64, f64),
    scale: f64,
) -> Scaled {
    let (width, height) = (
        picture.image.width() as f64 * scale,
        picture.image.height() as f64 * scale,
    );
    let (left, top) = (centre.0 - point.0 * scale, centre.1 - point.1 * scale);
    Scaled {
        id,
        depth,
        at: (left.round() as i32, top.round() as i32),
        size: (
            width.round().max(1.0) as u32,
            height.round().max(1.0) as u32,
        ),
        picture,
    }
}

impl Cosmos {
    /// One frame of the zoom from the level `from` shows to the one `to` shows, `k` of the way
    /// from 0 to 1, each level drawn at its time in `times`. Past its end it is `to`'s own
    /// scene, as it is between levels no zoom joins.
    pub fn zoom(&mut self, from: &View, to: &View, k: f64, times: Times) -> Scene {
        let next_to = depth(from.level).abs_diff(depth(to.level)) == 1;
        if k >= 1.0 || !next_to {
            return self.scene(to, times.to);
        }
        let (width, height) = (to.width.max(1), to.height.max(1));
        let inward = depth(to.level) > depth(from.level);
        let ((outer, outer_t), (inner, inner_t)) = if inward {
            ((from, times.from), (to, times.to))
        } else {
            ((to, times.to), (from, times.from))
        };
        let (w, h) = (width as f64, height as f64);
        let Some(anchor) = self.anchor(outer.level, inner.level, w, h, outer_t) else {
            return self.scene(to, times.to);
        };
        // How far in, from 0 at the outer level to 1 at the inner.
        let inside = if inward { k.max(0.0) } else { 1.0 - k.max(0.0) };
        let eased = smooth(0.0, 1.0, inside);
        let grown = anchor.scale.powf(eased);
        let centre = (
            mix(anchor.outer.0, anchor.inner.0, eased),
            mix(anchor.outer.1, anchor.inner.1, eased),
        );
        let mut scene = Scene::new(width, height);
        let sky = if eased < INNER_SKY {
            outer.level
        } else {
            inner.level
        };
        scene.add_layer(Layer {
            id: SKY,
            depth: Depth::Sky,
            picture: self.sky_of(sky, width, height),
        });
        if eased <= OUTER_UNTIL {
            let picture = self.bare(outer, width, height, outer_t);
            let part = stretched(OUTER, Depth::Ground, picture, anchor.outer, centre, grown);
            scene.add_scaled(part);
        }
        if eased >= INNER_FROM {
            let picture = self.bare(inner, width, height, inner_t);
            let scale = grown / anchor.scale;
            let part = stretched(INNER, Depth::Nature, picture, anchor.inner, centre, scale);
            scene.add_scaled(part);
        }
        scene.moves = true;
        scene
    }

    /// One frame at `t` of the zoom from your planet out to the universe, `z` of the way from
    /// the planet level at 1 to the universe at 0: over the universe's sky, its galaxies grown
    /// round the middle once they show, and `home`, your planet as the planet level drew it,
    /// shrunk from there to its place in the universe.
    pub fn zoom_from_planet(
        &mut self,
        width: usize,
        height: usize,
        home: &HomeShot,
        z: f64,
        t: f64,
    ) -> Scene {
        let (width, height) = (width.max(1), height.max(1));
        let (w, h) = (width as f64, height as f64);
        let middle = (w / 2.0, h / 2.0);
        let small = shapes::home_radius(w, h);
        let eased = smooth(0.0, 1.0, z.clamp(0.0, 1.0));
        let scale = (home.radius / small).max(1.0).powf(eased);
        let mut scene = Scene::new(width, height);
        scene.add_layer(Layer {
            id: SKY,
            depth: Depth::Sky,
            picture: self.sky_layer(width, height),
        });
        if (1.0 - eased).powf(1.4) > UNIVERSE_SHOWS {
            // Your planet is drawn over it at its own size, so the universe leaves it out.
            let universe = View::new(Level::Universe, width, height);
            let picture = self.bare(&universe, width, height, t);
            let part = stretched(OUTER, Depth::Ground, picture, middle, middle, scale);
            scene.add_scaled(part);
        }
        let radius = small * scale;
        scene.add_scaled(stretched(
            PLANET,
            Depth::Dots,
            home.picture.clone(),
            home.centre,
            middle,
            radius / home.radius.max(1.0),
        ));
        scene.moves = z > 0.0;
        scene
    }

    /// The universe's sky with its earlier months, as the planet level and the zooms to and
    /// from the universe show it, under one key for each size.
    pub fn sky_layer(&mut self, width: usize, height: usize) -> Picture {
        let key = self.key([33, 0, 0, width as u64, height as u64]);
        Picture::new(key, self.sky(width, height))
    }

    /// A level's own sky as a picture: the universe's, a galaxy's far field, or a system's
    /// with its galaxy's band.
    fn sky_of(&mut self, level: Level, width: usize, height: usize) -> Picture {
        let band = match level {
            Level::Universe => return self.sky_layer(width, height),
            Level::Galaxy(_) => None,
            Level::System(star) => self
                .universe
                .stars
                .get(star)
                .map(|s| self.universe.makers[s.maker].rgb),
        };
        let band_key = band.map_or(0, |[r, g, b]| {
            1 << 24 | u64::from(r) << 16 | u64::from(g) << 8 | u64::from(b)
        });
        let key = self.key([33, 1, band_key, width as u64, height as u64]);
        if let Some(image) = self.bares.0.get(&key) {
            return Picture::new(key, image.clone());
        }
        let image = self.sky_canvas(width, height, band, true).to_image();
        self.keep_bare(key, image.clone());
        Picture::new(key, image)
    }

    /// A level as its view shows it at `t` but without its sky or a ring, for a zoom to
    /// stretch over the skies: drawn once and kept for the zoom's steps.
    fn bare(&mut self, view: &View, width: usize, height: usize, t: f64) -> Picture {
        let [depth, focus] = level_key(view.level);
        let step = if view.still {
            u64::MAX
        } else {
            motion::step_of(t)
        };
        let home = view.home.as_ref().map_or(0, |home| home.key);
        let key = self.key([32, depth, focus, step, home, width as u64, height as u64]);
        if let Some(image) = self.bares.0.get(&key) {
            return Picture::new(key, image.clone());
        }
        let view = View {
            width,
            height,
            ringed: None,
            ..view.clone()
        };
        let image = self.level_scene(&view, t, false).flatten();
        self.keep_bare(key, image.clone());
        Picture::new(key, image)
    }

    fn keep_bare(&mut self, key: u64, image: Image) {
        if self.bares.0.len() >= BARES_KEPT {
            self.bares.0.clear();
        }
        self.bares.0.insert(key, image);
    }

    /// Where an inner level's picture sits in the outer one's.
    fn anchor(
        &self,
        outer: Level,
        inner: Level,
        width: f64,
        height: f64,
        t: f64,
    ) -> Option<Anchor> {
        match (outer, inner) {
            (Level::Universe, Level::Galaxy(maker)) => {
                let layout = self.layout(width, height);
                let o = *layout.get(maker)?;
                let f = self.galaxy_frame(maker, width, height);
                Some(Anchor {
                    outer: (o.x, o.y),
                    inner: (f.x, f.y),
                    scale: f.r / o.r,
                })
            }
            (Level::Galaxy(maker), Level::System(star)) => {
                let s = self.universe.stars.get(star)?;
                if s.maker != maker {
                    return None;
                }
                let f = self.galaxy_frame(maker, width, height);
                let at = self.galaxy_place(maker, f, star, Some(t))?;
                let system = self.system_frame(star, width, height);
                let swell = if s.kind == StarKind::RedGiant {
                    GIANT
                } else {
                    1.0
                };
                let drawn = core_radius(s.actions) * AT_GALAXY * 0.62 * swell;
                Some(Anchor {
                    outer: at,
                    inner: (system.cx, system.cy),
                    scale: system.star / drawn,
                })
            }
            _ => None,
        }
    }
}
