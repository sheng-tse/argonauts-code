//! The cosmos as the screen asks for it: one level at a time, as a scene of pictures stacked over
//! the map's cells and what the pointer finds where. What lies still is drawn once into one
//! picture per size; what moves is drawn in small pictures over it, each step's kept so a loop
//! replays what the screen already holds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use argonauts_core::behavior_world::WorldFacts;

use super::model::{StarAction, Universe};
use super::motion::{self, Group};
use super::paint::Paint;
use super::shapes::{self, Frame, Shapes};
use super::sky;
use crate::world::raster::Canvas;
use crate::world::{Depth, Image, PartId, Picture, Pointed, Scene, Sprite, picture_key};

/// The three levels of the cosmos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    /// A galaxy for each maker round your planet.
    Universe,
    /// One maker's galaxy, by its index in the universe.
    Galaxy(usize),
    /// One model's system, by its star's index in the universe.
    System(usize),
}

/// What the screen asks the cosmos to draw.
#[derive(Debug, Clone)]
pub struct View {
    pub level: Level,
    /// The picture's size in pixels: the map's cells times the cell size.
    pub width: usize,
    pub height: usize,
    /// Everything holds its still pose, as motion reduced or off asks.
    pub still: bool,
    /// What the pointer rests on or the keys picked, ringed.
    pub ringed: Option<Pointed>,
    /// Your planet for the universe's middle, as the planet level draws it at
    /// [`home_radius`](super::home_radius), centred in its picture.
    pub home: Option<Picture>,
}

impl View {
    pub fn new(level: Level, width: usize, height: usize) -> Self {
        View {
            level,
            width,
            height,
            still: false,
            ringed: None,
            home: None,
        }
    }
}

/// The pictures kept for one size of one level: what holds still at its still pose or while
/// the rest moves, each with its sky or bare for a zoom to lay over the skies it fades between.
#[derive(Default)]
pub(super) struct Kept {
    pub still: Option<Image>,
    pub moving: Option<Image>,
    pub bare_still: Option<Image>,
    pub bare_moving: Option<Image>,
    /// What lies in front of the moving bodies, where a level draws in depth.
    pub front: Option<Image>,
}

impl Kept {
    /// The picture kept for a still or moving level, with its sky or bare.
    pub fn slot(&mut self, still: bool, sky: bool) -> &mut Option<Image> {
        match (still, sky) {
            (true, true) => &mut self.still,
            (false, true) => &mut self.moving,
            (true, false) => &mut self.bare_still,
            (false, false) => &mut self.bare_moving,
        }
    }
}

/// The earlier months' clusters on the sky behind your planet, as one picture places them.
#[derive(Debug, Clone)]
pub struct SkyClusters(Vec<sky::Cluster>);

impl SkyClusters {
    /// What the pointer finds at (x, y) of the picture: an earlier month's cluster, or the far
    /// stars.
    pub fn at(&self, x: f32, y: f32) -> Pointed {
        let back = sky::cluster_at(&self.0, f64::from(x), f64::from(y));
        Pointed::Sky(back.and_then(|back| u32::try_from(back).ok()))
    }
}

/// The cosmos of one world: what there is, where it sits, and the pictures drawn of it so far.
pub struct Cosmos {
    pub(super) universe: Universe,
    pub(super) shapes: Shapes,
    /// Tells this cosmos's pictures from another's.
    pub(super) identity: u64,
    pub(super) paint: Paint,
    pub(super) skies: HashMap<SkyKey, Canvas>,
    /// The sky behind your planet as a picture, and its months' clusters, by size: the planet
    /// asks for both on every step.
    pub(super) sky_pictures: HashMap<(usize, usize), (Image, Arc<SkyClusters>)>,
    pub(super) layers: HashMap<(Level, usize, usize), Kept>,
    /// Pictures of moving things by their keys.
    pub(super) frames: HashMap<u64, Image>,
    /// The levels and skies drawn for zooms.
    pub(super) bares: super::zoom::Bares,
    /// Where each galaxy sits in the universe, by the picture's size: worked out once, as a
    /// scene of the universe asks for it at every step.
    layouts: Mutex<Layouts>,
}

/// Each galaxy's frame in the universe, by the width and the height of the picture, as bits.
type Layouts = HashMap<(u64, u64), Arc<[Frame]>>;

/// A sky's size, the band of the galaxy it is seen from, and whether it keeps the far field
/// alone.
type SkyKey = (usize, usize, Option<[u8; 3]>, bool);

/// Moving pictures kept at most; past it the oldest go, a whole loop's worth at a time.
const FRAMES_KEPT: usize = 2_000;

impl Cosmos {
    /// `colours` holds each model's colour in the order of the world's models, as every tab
    /// draws it, and `actions` the map's actions that belong to them (see
    /// [`star_actions`](super::star_actions)).
    pub fn new(facts: &WorldFacts, colours: &[[u8; 3]], actions: Vec<StarAction>) -> Self {
        let universe = Universe::new(facts, colours, actions);
        let shapes = Shapes::new(&universe, &facts.models);
        let identity = identity_of(&universe, &shapes);
        Cosmos {
            universe,
            shapes,
            identity,
            paint: Paint::default(),
            skies: HashMap::new(),
            sky_pictures: HashMap::new(),
            layers: HashMap::new(),
            frames: HashMap::new(),
            bares: Default::default(),
            layouts: Mutex::default(),
        }
    }

    /// Each galaxy's frame in the universe on a picture `width` × `height` px.
    pub(super) fn layout(&self, width: f64, height: f64) -> Arc<[Frame]> {
        let mut layouts = self.layouts.lock().unwrap_or_else(PoisonError::into_inner);
        let made = layouts
            .entry((width.to_bits(), height.to_bits()))
            .or_insert_with(|| {
                shapes::universe_layout(&self.universe, &self.shapes, width, height).into()
            });
        Arc::clone(made)
    }

    pub fn universe(&self) -> &Universe {
        &self.universe
    }

    /// The scene of one level at `t` seconds since the world opened.
    pub fn scene(&mut self, view: &View, t: f64) -> Scene {
        self.level_scene(view, t, true)
    }

    /// The scene of one level with its sky, or without it for a zoom to lay over the skies it
    /// fades between.
    pub(super) fn level_scene(&mut self, view: &View, t: f64, sky: bool) -> Scene {
        let mut scene = match view.level {
            Level::Universe => self.universe_scene(view, t, sky),
            Level::Galaxy(maker) => self.galaxy_scene(view, maker, t, sky),
            Level::System(star) => self.system_scene(view, star, t, sky),
        };
        if view.still {
            scene.moves = false;
        }
        scene
    }

    /// The universe's sky with its earlier months, as the planet level and the zoom out to the
    /// universe show it behind your planet.
    pub fn sky(&mut self, width: usize, height: usize) -> Image {
        self.sky_picture(width, height).0
    }

    /// What the pointer finds on the sky behind your planet: an earlier month's cluster, or the
    /// far stars.
    pub fn sky_at(&mut self, width: usize, height: usize, x: f32, y: f32) -> Pointed {
        self.sky_clusters(width, height).at(x, y)
    }

    /// The earlier months' clusters on the sky behind your planet, on a picture `width` ×
    /// `height` px, kept to find what the pointer rests on without the universe at hand.
    pub fn sky_clusters(&mut self, width: usize, height: usize) -> Arc<SkyClusters> {
        self.sky_picture(width, height).1
    }

    /// The sky behind your planet and its clusters, worked out once for each size.
    fn sky_picture(&mut self, width: usize, height: usize) -> (Image, Arc<SkyClusters>) {
        if let Some(kept) = self.sky_pictures.get(&(width, height)) {
            return kept.clone();
        }
        let image = self.sky_canvas(width, height, None, false).to_image();
        let (w, h) = (width as f64, height as f64);
        let layout = self.layout(w, h);
        let clusters = Arc::new(SkyClusters(sky::clusters(&self.universe, &layout, w, h)));
        let kept = (image, clusters);
        self.sky_pictures.insert((width, height), kept.clone());
        kept
    }

    /// A level's sky: the universe's with its months, a galaxy's and a system's the far field
    /// alone, a system's with its galaxy's band across it.
    pub(super) fn sky_canvas(
        &mut self,
        width: usize,
        height: usize,
        band: Option<[u8; 3]>,
        far_only: bool,
    ) -> &Canvas {
        let key = (width, height, band, far_only);
        if !self.skies.contains_key(&key) {
            let (w, h) = (width as f64, height as f64);
            let clusters = if far_only {
                Vec::new()
            } else {
                let layout = self.layout(w, h);
                sky::clusters(&self.universe, &layout, w, h)
            };
            let canvas = sky::sky(&clusters, width as u32, height as u32, band);
            self.skies.insert(key, canvas);
        }
        &self.skies[&key]
    }

    /// The picture of a moving thing at one step, drawn the first time it is asked for.
    pub(super) fn frame(&mut self, key: u64, draw: impl FnOnce(&mut Self) -> Image) -> Picture {
        if let Some(image) = self.frames.get(&key) {
            return Picture::new(key, image.clone());
        }
        if self.frames.len() >= FRAMES_KEPT {
            self.frames.clear();
        }
        let image = draw(self);
        self.frames.insert(key, image.clone());
        Picture::new(key, image)
    }

    /// Adds a picture for each group of moving bodies as they are at `t`: a copy of what lies
    /// still under the group's box, with `draw` laying each member over it, given where the
    /// box's top left is and where in its loop the group is.
    pub(super) fn add_groups(
        &mut self,
        scene: &mut Scene,
        still: &Image,
        groups: &[Group],
        t: f64,
        tag: u64,
        draw: impl Fn(&mut Cosmos, &mut Canvas, usize, (f64, f64), f64),
    ) {
        let (width, height) = (scene.width(), scene.height());
        for (index, group) in groups.iter().enumerate() {
            let Some((left, top, w, h)) = group.bounds.pixels(width, height) else {
                continue;
            };
            let looped = motion::looped(group, t);
            let step = motion::step_of(t) % u64::from(group.cycle);
            let key = self.key([tag, index as u64, step, width as u64, height as u64]);
            let picture = self.frame(key, |cosmos| {
                let mut canvas = crop(still, (left, top, w, h));
                for &member in &group.members {
                    draw(
                        cosmos,
                        &mut canvas,
                        member,
                        (f64::from(left), f64::from(top)),
                        looped,
                    );
                }
                canvas.to_image()
            });
            scene.add_sprite(Sprite {
                id: PartId::new("cosmos-moving", index as u32),
                depth: Depth::Dots,
                at: (left as i32, top as i32),
                picture,
            });
        }
    }

    /// Each group's members drawn as they are at `t` straight onto a copy of what lies still,
    /// for a picture without its sky: a group's own picture there would lay the light of what
    /// lies still under it over that light again.
    pub(super) fn draw_groups(
        &mut self,
        still: &Image,
        groups: &[Group],
        t: f64,
        draw: impl Fn(&mut Cosmos, &mut Canvas, usize, (f64, f64), f64),
    ) -> Image {
        let Some(mut canvas) = Canvas::from_image(still) else {
            return still.clone();
        };
        for group in groups {
            let looped = motion::looped(group, t);
            for &member in &group.members {
                draw(self, &mut canvas, member, (0.0, 0.0), looped);
            }
        }
        canvas.to_image()
    }

    /// A key for one of this cosmos's pictures.
    pub(super) fn key(&self, parts: impl IntoIterator<Item = u64>) -> u64 {
        picture_key(std::iter::once(self.identity).chain(parts))
    }
}

/// A copy of the part of a picture in a box of left, top, width and height.
pub(super) fn crop(image: &Image, (left, top, width, height): (u32, u32, u32, u32)) -> Canvas {
    let (left, top) = (left as usize, top as usize);
    let (width, height) = (width as usize, height as usize);
    let mut bytes = Vec::with_capacity(width * height * 4);
    for row in top..(top + height).min(image.height()) {
        let start = (row * image.width() + left) * 4;
        let end = (row * image.width() + (left + width).min(image.width())) * 4;
        bytes.extend_from_slice(&image.pixels()[start..end]);
    }
    Canvas::from_rgba(width as u32, height as u32, &bytes).unwrap_or_else(|| Canvas::new(1, 1))
}

/// A number standing for everything a cosmos draws from, so two cosmoses' pictures never share a
/// key.
fn identity_of(universe: &Universe, shapes: &Shapes) -> u64 {
    let text = format!("{universe:?}{shapes:?}");
    picture_key(text.bytes().map(u64::from))
}
