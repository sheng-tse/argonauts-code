//! What a map view hands the screen: a scene of pictures stacked in depth over the map's cells,
//! and what the pointer finds where. The screen keeps each picture it has sent and places it
//! again by its key, so a scene that changes only where a sprite stands costs a small move, not
//! a new picture.

use std::fmt;
use std::sync::Arc;

use crate::base::mascot::png::{self, Area, Rgba};

use super::geo::{Globe, LonLat, Plate};

/// A picture the world drew, as straight-alpha RGBA pixels. Cloning one shares its pixels, so
/// scenes and the frames kept for them hold no copies.
#[derive(Clone, PartialEq, Eq)]
pub struct Image(Arc<Rgba>);

impl Image {
    /// `pixels` as rows of red, green, blue and alpha from the top left, or `None` when there
    /// aren't `width` × `height` of them.
    pub fn new(width: usize, height: usize, pixels: Vec<u8>) -> Option<Self> {
        let wanted = width.checked_mul(height)?.checked_mul(4)?;
        (pixels.len() == wanted).then(|| {
            Self(Arc::new(Rgba {
                width,
                height,
                pixels,
            }))
        })
    }

    pub fn transparent(width: usize, height: usize) -> Self {
        Self(Arc::new(Rgba::transparent(width, height)))
    }

    /// The picture in a PNG of 8-bit RGBA without interlacing, as the crate's own pictures and
    /// its encoder write them. Anything else is `None`.
    pub fn from_png(bytes: &[u8]) -> Option<Self> {
        png::decode(bytes).map(|rgba| Self(Arc::new(rgba)))
    }

    pub fn width(&self) -> usize {
        self.0.width
    }

    pub fn height(&self) -> usize {
        self.0.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.0.pixels
    }

    /// The picture as a PNG, packed fast rather than small, for a terminal to show.
    pub fn to_png(&self) -> Vec<u8> {
        png::encode_quickly(&self.0)
    }

    /// The part `width` × `height` from (`left`, `top`), cut where it runs past the picture.
    pub fn cropped(&self, left: usize, top: usize, width: usize, height: usize) -> Self {
        let left = left.min(self.width());
        let top = top.min(self.height());
        let width = width.min(self.width() - left);
        let height = height.min(self.height() - top);
        let row = self.width() * 4;
        let mut pixels = Vec::with_capacity(width * height * 4);
        for y in top..top + height {
            let start = y * row + left * 4;
            pixels.extend_from_slice(&self.pixels()[start..start + width * 4]);
        }
        Self::new(width, height, pixels).unwrap_or_else(|| Self::transparent(1, 1))
    }

    /// The smallest part of the picture that holds every pixel it shows, and where that part's
    /// top left lies in it: a pixel and its own corner for a picture that shows none.
    pub fn trimmed(&self) -> (Self, (usize, usize)) {
        let (width, height) = (self.width(), self.height());
        let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
        for (index, pixel) in self.pixels().chunks_exact(4).enumerate() {
            if pixel[3] == 0 {
                continue;
            }
            let (x, y) = (index % width, index / width);
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
        if right <= left || bottom <= top {
            return (Self::transparent(1, 1), (0, 0));
        }
        (
            self.cropped(left, top, right - left, bottom - top),
            (left, top),
        )
    }

    pub(crate) fn rgba(&self) -> &Rgba {
        &self.0
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Image({} × {})", self.width(), self.height())
    }
}

/// A picture and the key it goes by. Two pictures with one key show the same pixels, so the
/// screen sends each key once and places it as often as scenes ask for it. A key covers the size
/// too: the same drawing at another size takes another key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub key: u64,
    pub image: Image,
}

impl Picture {
    pub fn new(key: u64, image: Image) -> Self {
        Self { key, image }
    }
}

/// A key for a picture, made from the numbers its drawing depends on, such as what it shows, its
/// frame and its size. The same numbers give the same key in every run.
pub fn picture_key(parts: impl IntoIterator<Item = u64>) -> u64 {
    parts
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .fold(FNV_OFFSET, fnv_step)
}

/// The name a layer or sprite keeps from one scene to the next, so the screen moves its picture
/// or swaps it for another rather than placing a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PartId(u64);

impl PartId {
    /// The `index`th part called `name`, the same in every run.
    pub const fn new(name: &str, index: u32) -> Self {
        let bytes = name.as_bytes();
        let mut hash = FNV_OFFSET;
        let mut at = 0;
        while at < bytes.len() {
            hash = fnv_step(hash, bytes[at]);
            at += 1;
        }
        let index = index.to_le_bytes();
        let mut at = 0;
        while at < index.len() {
            hash = fnv_step(hash, index[at]);
            at += 1;
        }
        Self(hash)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

const fn fnv_step(hash: u64, byte: u8) -> u64 {
    (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3)
}

/// How deep a picture sits, from the back. Pictures at one depth stack in the order they were
/// added, layers before sprites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Depth {
    /// Behind everything: the sky and its far stars.
    Sky,
    /// The ground: land, relief, sea and ice, or the planet's turned picture.
    Ground,
    /// What lies on the ground: water, nature and the wonders.
    Nature,
    /// What passes over the ground: storms, clouds and their shadows.
    Weather,
    /// Your actions' dots, and the bodies of the universe.
    Dots,
    /// The ring round what the pointer found.
    Pointer,
}

/// A picture that fills the scene, such as the ground or the planet's frame. The screen fits it
/// to the map's cells, so one drawn for an earlier cell size still covers them until it is
/// drawn again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub id: PartId,
    pub depth: Depth,
    pub picture: Picture,
}

/// A small picture at a point of the scene, such as a storm or the pointer's ring, shown at its
/// own size and cut where it runs past the scene's edge. Moving one, or swapping its picture for
/// one the screen already holds, is the cheapest change a scene can make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sprite {
    pub id: PartId,
    pub depth: Depth,
    /// Its top left, in the scene's pixels. It may start outside the scene.
    pub at: (i32, i32),
    pub picture: Picture,
}

/// A picture stretched over a box of the scene, as a zoom grows or shrinks a level the screen
/// already holds: the terminal scales the picture it keeps, so each step of the zoom is a
/// placement and no new picture. The box may run past the scene's edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scaled {
    pub id: PartId,
    pub depth: Depth,
    /// Its box's top left, in the scene's pixels.
    pub at: (i32, i32),
    /// Its box's width and height, in the scene's pixels.
    pub size: (u32, u32),
    pub picture: Picture,
}

impl Scaled {
    /// What of it falls inside a scene `width` × `height` px: the part of its box there, as its
    /// left, top, right and bottom in the scene's pixels, and the part of its picture that
    /// fills it. `None` when none of it does.
    pub fn cut(&self, width: usize, height: usize) -> Option<([f64; 4], Area)> {
        let image = self.picture.image.rgba();
        let (x, y) = (f64::from(self.at.0), f64::from(self.at.1));
        let (w, h) = (f64::from(self.size.0), f64::from(self.size.1));
        if w < 1.0 || h < 1.0 || image.width == 0 || image.height == 0 {
            return None;
        }
        let (left, top) = (x.max(0.0), y.max(0.0));
        let (right, bottom) = ((x + w).min(width as f64), (y + h).min(height as f64));
        if right - left < 1.0 || bottom - top < 1.0 {
            return None;
        }
        // The picture's pixels under that part of the box, every one it touches.
        let (own_width, own_height) = (image.width as f64, image.height as f64);
        let first = |at: f64, from: f64, size: f64, own: f64| {
            (((at - from) / size * own).floor() as usize).min(own as usize - 1)
        };
        let last = |at: f64, from: f64, size: f64, own: f64| {
            (((at - from) / size * own).ceil() as usize).clamp(1, own as usize)
        };
        let (from_x, from_y) = (first(left, x, w, own_width), first(top, y, h, own_height));
        let (to_x, to_y) = (
            last(right, x, w, own_width).max(from_x + 1),
            last(bottom, y, h, own_height).max(from_y + 1),
        );
        let area = Area {
            x: from_x,
            y: from_y,
            width: to_x - from_x,
            height: to_y - from_y,
        };
        Some(([left, top, right, bottom], area))
    }
}

/// One picture of a scene's stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part<'a> {
    Layer(&'a Layer),
    Sprite(&'a Sprite),
    Scaled(&'a Scaled),
}

impl Part<'_> {
    pub fn id(&self) -> PartId {
        match self {
            Part::Layer(layer) => layer.id,
            Part::Sprite(sprite) => sprite.id,
            Part::Scaled(scaled) => scaled.id,
        }
    }

    pub fn depth(&self) -> Depth {
        match self {
            Part::Layer(layer) => layer.depth,
            Part::Sprite(sprite) => sprite.depth,
            Part::Scaled(scaled) => scaled.depth,
        }
    }

    pub fn picture(&self) -> &Picture {
        match self {
            Part::Layer(layer) => &layer.picture,
            Part::Sprite(sprite) => &sprite.picture,
            Part::Scaled(scaled) => &scaled.picture,
        }
    }
}

/// What a map shows at one moment: pictures stacked in depth over its cells, `width` × `height`
/// pixels (the cells times the cell size), and what the pointer finds where. Motion is one scene
/// after another, each naming the frames it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    width: usize,
    height: usize,
    layers: Vec<Layer>,
    sprites: Vec<Sprite>,
    scaled: Vec<Scaled>,
    pub hits: HitMap,
    /// Something in it moves, so the screen keeps its clock running and asks for the next scene.
    pub moves: bool,
}

impl Scene {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            layers: Vec::new(),
            sprites: Vec::new(),
            scaled: Vec::new(),
            hits: HitMap::default(),
            moves: false,
        }
    }

    /// One picture as the whole scene, as a map drawn as a single picture hands it over.
    pub fn single(picture: Picture) -> Self {
        let mut scene = Self::new(picture.image.width(), picture.image.height());
        scene.add_layer(Layer {
            id: PartId::new("picture", 0),
            depth: Depth::Ground,
            picture,
        });
        scene
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn add_layer(&mut self, layer: Layer) {
        self.layers.push(layer);
    }

    pub fn add_sprite(&mut self, sprite: Sprite) {
        self.sprites.push(sprite);
    }

    pub fn add_scaled(&mut self, scaled: Scaled) {
        self.scaled.push(scaled);
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// The scene without its parts at `depth`, as a terminal that takes only whole pictures
    /// is sent it without the pointer's ring, which would make every move a new picture.
    pub fn without(&self, depth: Depth) -> Self {
        Self {
            layers: self
                .layers
                .iter()
                .filter(|layer| layer.depth != depth)
                .cloned()
                .collect(),
            sprites: self
                .sprites
                .iter()
                .filter(|sprite| sprite.depth != depth)
                .cloned()
                .collect(),
            scaled: self
                .scaled
                .iter()
                .filter(|scaled| scaled.depth != depth)
                .cloned()
                .collect(),
            ..self.clone()
        }
    }

    pub fn sprites(&self) -> &[Sprite] {
        &self.sprites
    }

    pub fn scaled(&self) -> &[Scaled] {
        &self.scaled
    }

    /// Every picture from the back to the front, in the order the screen places them.
    pub fn stack(&self) -> Vec<Part<'_>> {
        let mut parts: Vec<Part<'_>> = self
            .layers
            .iter()
            .map(Part::Layer)
            .chain(self.sprites.iter().map(Part::Sprite))
            .chain(self.scaled.iter().map(Part::Scaled))
            .collect();
        // Stable, so each depth keeps the order above.
        parts.sort_by_key(Part::depth);
        parts
    }

    /// The scene drawn into one picture, for a terminal that takes only whole pictures.
    pub fn flatten(&self) -> Image {
        let mut out = Rgba::transparent(self.width, self.height);
        for part in self.stack() {
            match part {
                Part::Layer(layer) => {
                    let image = layer.picture.image.rgba();
                    if (image.width, image.height) == (self.width, self.height) {
                        draw_over(&mut out, image, (0, 0));
                    } else {
                        let fitted = png::fitted(
                            image,
                            whole(image.width, image.height),
                            self.width,
                            self.height,
                            whole(self.width, self.height),
                        );
                        draw_over(&mut out, &fitted, (0, 0));
                    }
                }
                Part::Sprite(sprite) => draw_over(&mut out, sprite.picture.image.rgba(), sprite.at),
                Part::Scaled(scaled) => {
                    let Some(([left, top, right, bottom], from)) =
                        scaled.cut(self.width, self.height)
                    else {
                        continue;
                    };
                    let (x, y) = (left.round() as usize, top.round() as usize);
                    let width = (right.round() as usize).saturating_sub(x).max(1);
                    let height = (bottom.round() as usize).saturating_sub(y).max(1);
                    let image = scaled.picture.image.rgba();
                    let fitted = png::fitted(image, from, width, height, whole(width, height));
                    draw_over(&mut out, &fitted, (x as i32, y as i32));
                }
            }
        }
        Image(Arc::new(out))
    }
}

fn whole(width: usize, height: usize) -> Area {
    Area {
        x: 0,
        y: 0,
        width,
        height,
    }
}

/// `top` laid over `under` with its top left at `at`, both straight alpha.
fn draw_over(under: &mut Rgba, top: &Rgba, at: (i32, i32)) {
    for row in 0..top.height {
        let Some(y) = shifted(at.1, row, under.height) else {
            continue;
        };
        for column in 0..top.width {
            let Some(x) = shifted(at.0, column, under.width) else {
                continue;
            };
            let from = (row * top.width + column) * 4;
            let to = (y * under.width + x) * 4;
            let source = &top.pixels[from..from + 4];
            let source_alpha = u32::from(source[3]);
            if source_alpha == 0 {
                continue;
            }
            let target = &mut under.pixels[to..to + 4];
            let target_alpha = u32::from(target[3]);
            // Alphas scaled to 255 × 255, so the sums stay whole numbers.
            let kept = target_alpha * (255 - source_alpha);
            let alpha = source_alpha * 255 + kept;
            for channel in 0..3 {
                let mixed = u32::from(source[channel]) * source_alpha * 255
                    + u32::from(target[channel]) * kept;
                target[channel] = ((mixed + alpha / 2) / alpha) as u8;
            }
            target[3] = ((alpha + 127) / 255) as u8;
        }
    }
}

/// The pixel `offset` past `start`, when it falls inside `0..size`.
fn shifted(start: i32, offset: usize, size: usize) -> Option<usize> {
    let at = i64::from(start) + i64::try_from(offset).ok()?;
    usize::try_from(at).ok().filter(|&at| at < size)
}

/// What the pointer found: what a hover line names and a ring marks. Each index points into the
/// list kept by the part of the world that drew the mark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pointed {
    /// An action: its dot on the map or the planet, or its star in the universe or a galaxy, by
    /// its index among the world's actions.
    Action(u32),
    /// A feature of nature or weather, such as a river, a hurricane, an iceberg or the clouds, by
    /// its index in the world's list of them.
    Feature(u32),
    /// A wonder, by its index among the wonders the world shows.
    Wonder(u32),
    /// The bare ground or sea where no mark answers: its place on the planet, or `None` where
    /// the picture shows open sea at no one place, as beyond the land stood up in relief.
    Ground(Option<LonLat>),
    /// The sky: the cluster of far stars an earlier month left, by how many months back, or the
    /// far stars as a whole.
    Sky(Option<u32>),
    /// Your own planet, seen from the universe.
    Home,
    /// A maker's galaxy, by its index in the universe.
    Galaxy(u32),
    /// A model's star, by its index in the universe; its system opens from it.
    Star(u32),
    /// Anything else the universe draws, such as a planet, a moon, a belt, a flare, a comet or a
    /// bridge between galaxies, by its index in the universe's list of them.
    Body(u32),
}

/// The outline of something the pointer can find, in the scene's pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// A point, such as an action's dot, found within its mark's reach.
    Point {
        x: f32,
        y: f32,
    },
    Disc {
        x: f32,
        y: f32,
        radius: f32,
    },
    /// A box, such as a wonder's, by its edges.
    Rect {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    },
    /// A line through points, such as a river, `width` px across.
    Path {
        points: Vec<(f32, f32)>,
        width: f32,
    },
    /// An ellipse turned `angle` radians clockwise, such as a galaxy's disc seen aslant.
    Ellipse {
        x: f32,
        y: f32,
        rx: f32,
        ry: f32,
        angle: f32,
    },
}

impl Shape {
    /// How far a point lies outside the shape, in pixels: 0 or less inside it. An ellipse's
    /// distance is measured along its shorter radius.
    pub fn distance(&self, x: f32, y: f32) -> f32 {
        match self {
            Shape::Point { x: px, y: py } => (x - px).hypot(y - py),
            Shape::Disc {
                x: cx,
                y: cy,
                radius,
            } => (x - cx).hypot(y - cy) - radius,
            Shape::Rect {
                left,
                top,
                right,
                bottom,
            } => {
                let dx = (left - x).max(x - right);
                let dy = (top - y).max(y - bottom);
                if dx <= 0.0 && dy <= 0.0 {
                    dx.max(dy)
                } else {
                    dx.max(0.0).hypot(dy.max(0.0))
                }
            }
            Shape::Path { points, width } => {
                let nearest = match points.as_slice() {
                    [] => f32::INFINITY,
                    [only] => (x - only.0).hypot(y - only.1),
                    _ => points
                        .windows(2)
                        .map(|pair| to_segment((x, y), pair[0], pair[1]))
                        .fold(f32::INFINITY, f32::min),
                };
                nearest - width / 2.0
            }
            Shape::Ellipse {
                x: cx,
                y: cy,
                rx,
                ry,
                angle,
            } => {
                let (sin, cos) = angle.sin_cos();
                let (dx, dy) = (x - cx, y - cy);
                let along = (dx * cos + dy * sin) / rx;
                let across = (dy * cos - dx * sin) / ry;
                (along.hypot(across) - 1.0) * rx.min(*ry)
            }
        }
    }
}

fn to_segment(point: (f32, f32), from: (f32, f32), to: (f32, f32)) -> f32 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx * dx + dy * dy;
    let t = if length > 0.0 {
        (((point.0 - from.0) * dx + (point.1 - from.1) * dy) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (point.0 - (from.0 + t * dx)).hypot(point.1 - (from.1 + t * dy))
}

/// Something drawn where the pointer can find it.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub shape: Shape,
    /// How far outside its shape it still answers, in pixels.
    pub reach: f32,
    /// Lower tiers answer first: an action's dot right under the pointer can beat the river it
    /// sits on, and the river a dot further off, by adding the dot twice.
    pub tier: u8,
    pub pointed: Pointed,
}

impl Mark {
    /// A mark answering only inside its shape, in the first tier.
    pub fn new(shape: Shape, pointed: Pointed) -> Self {
        Self {
            shape,
            reach: 0.0,
            tier: 0,
            pointed,
        }
    }

    pub fn reach(mut self, pixels: f32) -> Self {
        self.reach = pixels;
        self
    }

    pub fn tier(mut self, tier: u8) -> Self {
        self.tier = tier;
        self
    }
}

/// What answers where no mark reaches.
#[derive(Clone, Default, PartialEq)]
pub enum Under {
    #[default]
    Nothing,
    /// The flat map over the whole scene, its plate sized as the scene: the ground at the place
    /// under the point.
    Map(Plate),
    /// The map with its land stood up, which moves the ground up the screen but never across:
    /// for each pixel of the scene, row by row, the row of the flat map it shows, or a negative
    /// number where it shows open sea at no one place.
    Relief {
        plate: Plate,
        width: usize,
        rows: Arc<[f32]>,
    },
    /// A globe: the ground on its disc, and the sky round it.
    Globe(Globe),
    /// The sky everywhere, as behind the universe.
    Sky,
}

impl Under {
    fn find(&self, x: f32, y: f32) -> Option<Pointed> {
        let (x, y) = (f64::from(x), f64::from(y));
        match self {
            Under::Nothing => None,
            Under::Map(plate) => Some(Pointed::Ground(Some(plate.place(x, y)))),
            Under::Relief { plate, width, rows } => {
                let column = pixel_index(x)?;
                let row = pixel_index(y)?;
                if column >= *width {
                    return None;
                }
                let shown = *rows.get(row.checked_mul(*width)?.checked_add(column)?)?;
                let place = (shown >= 0.0).then(|| plate.place(x, f64::from(shown)));
                Some(Pointed::Ground(place))
            }
            Under::Globe(globe) => Some(match globe.surface_at(x, y) {
                Some(surface) => Pointed::Ground(Some(surface.place)),
                None => Pointed::Sky(None),
            }),
            Under::Sky => Some(Pointed::Sky(None)),
        }
    }
}

impl fmt::Debug for Under {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Under::Nothing => f.write_str("Nothing"),
            Under::Map(plate) => f.debug_tuple("Map").field(plate).finish(),
            // A row for every pixel is too many to print.
            Under::Relief { plate, width, rows } => f
                .debug_struct("Relief")
                .field("plate", plate)
                .field("width", width)
                .field("rows", &rows.len())
                .finish(),
            Under::Globe(globe) => f.debug_tuple("Globe").field(globe).finish(),
            Under::Sky => f.write_str("Sky"),
        }
    }
}

fn pixel_index(at: f64) -> Option<usize> {
    // The float-to-int cast saturates, and a negative or NaN point lies outside every row.
    (at >= 0.0).then(|| at.floor() as usize)
}

/// What the pointer can find in a scene, in the scene's pixels: marks laid over whatever lies
/// under them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HitMap {
    marks: Vec<Mark>,
    under: Under,
}

impl HitMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, mark: Mark) {
        self.marks.push(mark);
    }

    pub fn set_under(&mut self, under: Under) {
        self.under = under;
    }

    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    pub fn under(&self) -> &Under {
        &self.under
    }

    /// What the pointer finds at a point: of the marks that reach it, the lowest tier, then the
    /// nearest, then the first added; else what lies under the marks.
    pub fn find(&self, x: f32, y: f32) -> Option<Pointed> {
        let mut best: Option<(u8, f32, &Mark)> = None;
        for mark in &self.marks {
            let distance = mark.shape.distance(x, y);
            if distance > mark.reach {
                continue;
            }
            let better = best.is_none_or(|(tier, nearest, _)| {
                mark.tier < tier || (mark.tier == tier && distance < nearest)
            });
            if better {
                best = Some((mark.tier, distance, mark));
            }
        }
        match best {
            Some((_, _, mark)) => Some(mark.pointed),
            None => self.under.find(x, y),
        }
    }
}

#[cfg(test)]
mod tests;
