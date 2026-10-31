//! Getting the world to the terminal: its scenes' pictures kept, placed and freed, and the clock
//! that moves them only while the world shows.
//!
//! A terminal that takes the kitty protocol keeps every picture it is sent, by id, and shows each
//! part of a scene through a placement of its own. A new scene then costs only what changed in
//! it: a sprite that moved is its placement sent again, well under a hundred bytes; a frame
//! swapped for one already kept is the new placement and the old one taken away; a part that went
//! is its placement taken away. Pictures stay kept once they leave the screen, up to a limit, so
//! coming back to one sends nothing.
//!
//! [`clock`] says when the world may move and wake the app, [`power`] reads how the computer is
//! powered for it, and [`worker`] draws and packs pictures off the frame.

pub mod clock;
pub mod power;
pub mod worker;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::{Mutex, MutexGuard, Once, PoisonError, TryLockError};

use ratatui::layout::Rect;

use crate::base::mascot::graphics::{self, Crop, Placement};
use crate::base::mascot::png;

use super::super::base::draw::scene::{Depth, Image, Part, PartId, Scaled, Scene, picture_key};
use super::raster::{Brush, Canvas, Colour};
use worker::Worker;

/// How many ids one stage names its pictures from, counting up from its first.
const IDS: u32 = 1 << 16;
/// How many bytes of pixels the world keeps at most at the usual cell of 18 × 42 px, four a
/// pixel as a terminal holds them: room for a ground, the sprites' frames and a cloudy crew's
/// whole planet turn, its ground, clouds and dots each step.
const KEPT_BYTES: usize = 128 << 20;
/// The least it keeps, at the smallest cells.
const FEWEST_KEPT_BYTES: usize = 64 << 20;
/// The most it keeps, at the biggest cells: well inside the 320 MB that kitty and Ghostty keep
/// pictures in, so the terminal never drops one the stage counts on.
const MOST_KEPT_BYTES: usize = 256 << 20;
/// How far apart the depths stack, so a part added at one depth moves none at another.
const DEPTH_STEP: i32 = 1 << 16;

/// Where a scene shows on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    /// The cells it covers.
    pub cells: Rect,
    /// Pixels a cell.
    pub cell: (u16, u16),
}

/// The pictures a terminal keeps for one scene's place on screen, such as a map's, and where
/// each part of its last scene shows.
#[derive(Debug)]
pub struct Stage {
    first_id: u32,
    next_id: u32,
    /// Bytes of pixels it keeps in the terminal at most; when `None`, as many as
    /// [`kept_bytes`] gives the cells it shows in.
    limit: Option<usize>,
    /// The depth its backmost part sits at.
    lowest: i32,
    /// By picture key.
    kept: HashMap<u64, Kept>,
    shown: BTreeMap<PartId, Shown>,
    /// Each part's placement number, kept for as long as the stage is, so a part placed again
    /// replaces itself.
    placements: HashMap<PartId, u32>,
    /// Counts every use of a kept picture, so the one used longest ago goes first.
    uses: u64,
    /// Packs the pictures the terminal doesn't keep yet, off the frame, each into the escapes
    /// that send it, so a frame only copies their bytes.
    packing: Worker<Packed>,
    /// The id each picture being packed goes under, by picture key, which no other picture
    /// takes meanwhile.
    packing_ids: HashMap<u64, u32>,
    /// Pictures packed and not yet sent, by picture key.
    packed: HashMap<u64, Packed>,
}

/// A picture packed for the terminal: the escapes that send it under `id`.
#[derive(Debug)]
struct Packed {
    id: u32,
    escapes: Vec<u8>,
    /// Bytes of pixels the terminal keeps for it.
    bytes: usize,
}

#[derive(Debug, Clone, Copy)]
struct Kept {
    id: u32,
    bytes: usize,
    used: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shown {
    /// The cell it starts from.
    at: (u16, u16),
    placement: Placement,
}

impl Stage {
    /// A stage naming its pictures from `first_id`, clear of the ids other pictures in the same
    /// terminal use, which packs its new pictures on a thread of its own.
    pub fn new(first_id: u32) -> Self {
        Self {
            first_id,
            next_id: first_id,
            limit: None,
            lowest: 1,
            kept: HashMap::new(),
            shown: BTreeMap::new(),
            placements: HashMap::new(),
            uses: 0,
            packing: Worker::threaded(),
            packing_ids: HashMap::new(),
            packed: HashMap::new(),
        }
    }

    /// Packs each new picture the moment a scene needs it, so every scene shows whole at once.
    pub fn packing_in_line(mut self) -> Self {
        self.packing = Worker::in_line();
        self
    }

    /// Whether pictures a scene needs are still being packed, so it shows them on a later
    /// frame.
    pub fn is_packing(&self) -> bool {
        self.packing.busy()
    }

    /// What the last packing to panic said, once.
    pub fn take_failure(&self) -> Option<String> {
        self.packing.take_failure()
    }

    /// Stacks every part under the text, and under cells with a background colour too, so text
    /// drawn over the pictures, selected text included, still shows: for pictures behind words,
    /// such as a conversation's bubbles.
    pub fn beneath_text(mut self) -> Self {
        self.lowest = i32::MIN + 1;
        self
    }

    /// Keeps at most `bytes` of pixels in the terminal.
    pub fn with_limit(mut self, bytes: usize) -> Self {
        self.limit = Some(bytes);
        self
    }

    /// Whether any part shows.
    pub fn is_showing(&self) -> bool {
        !self.shown.is_empty()
    }

    /// Whether the terminal keeps any of its pictures.
    pub fn keeps_pictures(&self) -> bool {
        !self.kept.is_empty()
    }

    /// The ids of the pictures the terminal keeps for it.
    pub fn kept_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.kept.values().map(|kept| kept.id)
    }

    /// The escapes that make the screen show `scene` over `spot`, from what it shows now: the
    /// pictures it doesn't keep yet, then each part placed, moved or swapped, and the parts that
    /// went taken away. Each placement moves the cursor to its cell first; nothing goes out
    /// when nothing changed.
    ///
    /// A picture still being packed holds its part as it was, or off the screen if it is new,
    /// until a later call finds it packed.
    pub fn show(&mut self, scene: &Scene, spot: Spot) -> Vec<u8> {
        let stack = scene.stack();
        for part in &stack {
            let sent = Sent::of(part, scene, spot);
            let wanted = geometry(part, scene, spot).is_some();
            if wanted && !self.is_kept_or_packing(sent.key) {
                let id = self.take_id();
                self.packing_ids.insert(sent.key, id);
                let bytes = sent.bytes();
                self.packing.draw(sent.key, move |cancel| {
                    let image = sent.pixels()?;
                    let png = png::encode_quickly_unless(image.rgba(), || cancel.cancelled())?;
                    Some(Packed {
                        id,
                        escapes: graphics::store(&png, id),
                        bytes,
                    })
                });
            }
        }
        // Idle before the take, every packing asked for has ended and is taken now, so an id
        // left over is one whose packing gave nothing, as one stopped part way does: free again.
        let idle = !self.packing.busy();
        for (key, packed) in self.packing.take() {
            self.packing_ids.remove(&key);
            self.packed.insert(key, packed);
        }
        if idle {
            self.packing_ids.clear();
        }
        let mut out = Escapes::default();
        let mut seen = HashSet::new();
        let mut at_depth: HashMap<Depth, i32> = HashMap::new();
        for part in stack {
            let index = at_depth.entry(part.depth()).or_default();
            let z = self.lowest + depth_rank(part.depth()) * DEPTH_STEP + *index;
            *index += 1;
            let Some((at, mut placement)) = geometry(&part, scene, spot) else {
                continue;
            };
            let id = part.id();
            let sent = Sent::of(&part, scene, spot);
            let Some(image) = self.keep(&sent, &mut out.bytes) else {
                if self.shown.contains_key(&id) {
                    seen.insert(id);
                }
                continue;
            };
            placement.image = image;
            placement.placement = self.placement_of(id);
            placement.z = z;
            seen.insert(id);
            let wanted = Shown { at, placement };
            let was = self.shown.insert(id, wanted);
            if was == Some(wanted) {
                continue;
            }
            out.move_to(at);
            out.bytes.extend(graphics::place(&placement));
            // A placement belongs to its picture, so a swapped one goes from the old picture.
            if let Some(was) = was.filter(|was| was.placement.image != placement.image) {
                out.bytes.extend(remove(&was));
            }
        }
        let gone: Vec<PartId> = self
            .shown
            .keys()
            .filter(|id| !seen.contains(id))
            .copied()
            .collect();
        for id in gone {
            if let Some(was) = self.shown.remove(&id) {
                out.bytes.extend(remove(&was));
            }
        }
        // Packed for a scene the motion has gone past: sent all the same, so the terminal keeps
        // it for when the motion comes back to it, as it would have been sent in time. Dropped
        // instead, it would be packed again each time and come late again.
        for (key, packed) in std::mem::take(&mut self.packed) {
            self.store(key, packed, &mut out.bytes);
        }
        let limit = self.limit.unwrap_or_else(|| {
            let (width, height) = (f64::from(spot.cell.0), f64::from(spot.cell.1));
            kept_bytes(width * height / (18.0 * 42.0))
        });
        self.keep_within_limit(limit, &mut out.bytes);
        out.bytes
    }

    /// The escapes that take every part off the screen, keeping their pictures for when the
    /// scene comes back. Pictures still being packed are given up.
    pub fn hide(&mut self) -> Vec<u8> {
        self.packing.stop();
        self.packing_ids.clear();
        self.packed.clear();
        std::mem::take(&mut self.shown)
            .values()
            .flat_map(remove)
            .collect()
    }

    /// The escapes that take every part off the screen and free every picture the terminal
    /// keeps for it.
    pub fn free(&mut self) -> Vec<u8> {
        let mut out = self.hide();
        let ids: Vec<u32> = self.kept_ids().collect();
        let mut kept = kept_anywhere();
        for id in &ids {
            kept.remove(id);
        }
        out.extend(graphics::forget(ids));
        self.kept.clear();
        out
    }

    fn is_kept_or_packing(&self, key: u64) -> bool {
        self.kept.contains_key(&key)
            || self.packing_ids.contains_key(&key)
            || self.packed.contains_key(&key)
    }

    /// The id the terminal keeps `picture` under, with the escapes that send it first when it
    /// isn't kept yet. `None` while it is still being packed.
    fn keep(&mut self, sent: &Sent, out: &mut Vec<u8>) -> Option<u32> {
        self.uses += 1;
        if let Some(kept) = self.kept.get_mut(&sent.key) {
            kept.used = self.uses;
            return Some(kept.id);
        }
        let packed = self.packed.remove(&sent.key)?;
        Some(self.store(sent.key, packed, out))
    }

    /// Sends `packed`, the picture under `key`, for the terminal to keep: its id.
    fn store(&mut self, key: u64, packed: Packed, out: &mut Vec<u8>) -> u32 {
        let Packed { id, escapes, bytes } = packed;
        out.extend(escapes);
        kept_anywhere().insert(id);
        free_on_panic();
        self.kept.insert(
            key,
            Kept {
                id,
                bytes,
                used: self.uses,
            },
        );
        id
    }

    /// The next id no picture kept, being packed or packed has, from the stage's own.
    fn take_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            let last = self.first_id.saturating_add(IDS - 1);
            self.next_id = if id >= last { self.first_id } else { id + 1 };
            let taken = self.kept.values().any(|kept| kept.id == id)
                || self.packing_ids.values().any(|packing| *packing == id)
                || self.packed.values().any(|packed| packed.id == id);
            if !taken {
                return id;
            }
        }
    }

    fn placement_of(&mut self, part: PartId) -> u32 {
        let next = u32::try_from(self.placements.len() + 1).unwrap_or(u32::MAX);
        *self.placements.entry(part).or_insert(next)
    }

    /// Frees the pictures used longest ago that no part shows, while the kept ones pass the
    /// limit.
    fn keep_within_limit(&mut self, limit: usize, out: &mut Vec<u8>) {
        let mut total: usize = self.kept.values().map(|kept| kept.bytes).sum();
        if total <= limit {
            return;
        }
        let showing: HashSet<u32> = self
            .shown
            .values()
            .map(|shown| shown.placement.image)
            .collect();
        let mut idle: Vec<(u64, Kept)> = self
            .kept
            .iter()
            .filter(|(_, kept)| !showing.contains(&kept.id))
            .map(|(key, kept)| (*key, *kept))
            .collect();
        idle.sort_by_key(|(_, kept)| kept.used);
        for (key, kept) in idle {
            if total <= limit {
                break;
            }
            total -= kept.bytes;
            self.kept.remove(&key);
            kept_anywhere().remove(&kept.id);
            out.extend(graphics::forget([kept.id]));
        }
    }
}

/// How many bytes of pixels the world keeps at most for pictures `scale` times the area they
/// have at the usual cell: as much more as bigger cells make each picture, within the least and
/// the most a terminal can be asked to keep.
pub fn kept_bytes(scale: f64) -> usize {
    let bytes = KEPT_BYTES as f64 * scale.max(0.0);
    (bytes as usize).clamp(FEWEST_KEPT_BYTES, MOST_KEPT_BYTES)
}

/// Every picture a stage keeps in the terminal right now, so a panic can free them on its way
/// out.
static KEPT: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

fn kept_anywhere() -> MutexGuard<'static, BTreeSet<u32>> {
    KEPT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// From the first picture kept on: a panic frees every picture the stages keep before the
/// terminal goes back to the shell, since nothing would free them after. The tests' own panics
/// leave the terminal they run in alone.
fn free_on_panic() {
    static INSTALL: Once = Once::new();
    if cfg!(test) {
        return;
    }
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if worker::panicking_in_a_drawing() {
                previous(info);
                return;
            }
            let escapes = escapes_on_panic();
            if !escapes.is_empty() {
                let mut out = std::io::stdout();
                let _ = out.write_all(&escapes);
                let _ = out.flush();
            }
            previous(info);
        }));
    });
}

/// The escapes that free every picture kept, unless the panic came while the list was being
/// changed on another thread.
fn escapes_on_panic() -> Vec<u8> {
    let ids: Vec<u32> = match KEPT.try_lock() {
        Ok(kept) => kept.iter().copied().collect(),
        Err(TryLockError::Poisoned(kept)) => kept.into_inner().iter().copied().collect(),
        Err(TryLockError::WouldBlock) => Vec::new(),
    };
    graphics::forget(ids)
}

/// A number that changes whenever what `scene` shows does: its pictures and where each stands.
/// A terminal that takes only whole pictures gets one again only when it changes.
pub fn look(scene: &Scene) -> u64 {
    let size = [scene.width() as u64, scene.height() as u64];
    let parts = scene.stack().into_iter().flat_map(|part| {
        let ((x, y), (width, height)) = match part {
            Part::Layer(_) => ((0, 0), (0, 0)),
            Part::Sprite(sprite) => (sprite.at, (0, 0)),
            Part::Scaled(scaled) => (scaled.at, scaled.size),
        };
        [
            part.id().get(),
            part.picture().key,
            u64::from(x as u32),
            u64::from(y as u32),
            u64::from(width) << 32 | u64::from(height),
        ]
    });
    picture_key(size.into_iter().chain(parts))
}

fn remove(shown: &Shown) -> Vec<u8> {
    graphics::remove(shown.placement.image, shown.placement.placement)
}

fn depth_rank(depth: Depth) -> i32 {
    match depth {
        Depth::Sky => 0,
        Depth::Ground => 1,
        Depth::Nature => 2,
        Depth::Weather => 3,
        Depth::Dots => 4,
        Depth::Pointer => 5,
    }
}

/// Where `part` of `scene` shows over `spot`: the cell it starts from and its placement, still
/// without its picture, number or depth. `None` when none of it falls inside the scene.
///
/// A layer fills the spot's cells, whatever cell size it was drawn for. A sprite shows cut where
/// it runs past the scene's edge; in a scene drawn for another cell size it is stretched as
/// much as the scene is, and stands where its point falls once the scene is stretched over the
/// cells. A scaled part is stretched over its box, see [`scaled_geometry`].
fn geometry(part: &Part<'_>, scene: &Scene, spot: Spot) -> Option<((u16, u16), Placement)> {
    let origin = (spot.cells.x, spot.cells.y);
    let sprite = match part {
        Part::Layer(_) => {
            let cells = Some((spot.cells.width, spot.cells.height));
            return Some((
                origin,
                Placement {
                    cells,
                    ..unplaced()
                },
            ));
        }
        Part::Sprite(sprite) => sprite,
        Part::Scaled(scaled) => return scaled_geometry(scaled, scene, spot),
    };
    let (cell_width, cell_height) = (i64::from(spot.cell.0.max(1)), i64::from(spot.cell.1.max(1)));
    let across = i64::from(spot.cells.width) * cell_width;
    let down = i64::from(spot.cells.height) * cell_height;
    let (scene_width, scene_height) = (to_i64(scene.width()).max(1), to_i64(scene.height()).max(1));
    // From here on in the spot's own pixels: the sprite's corner and size, and the part of it
    // inside the scene.
    let stretched = |at: i32, pixels: i64, of: i64| (i64::from(at) * pixels).div_euclid(of);
    let (x, y) = (
        stretched(sprite.at.0, across, scene_width),
        stretched(sprite.at.1, down, scene_height),
    );
    let (width, height) = shown_size(&sprite.picture.image, scene, spot);
    let (width, height) = (to_i64(width), to_i64(height));
    let (left, top) = ((-x).max(0), (-y).max(0));
    let (right, bottom) = ((across - x).min(width), (down - y).min(height));
    if left >= right || top >= bottom {
        return None;
    }
    let (px, py) = (x + left, y + top);
    let (column, row) = (px / cell_width, py / cell_height);
    if column >= i64::from(spot.cells.width) || row >= i64::from(spot.cells.height) {
        return None;
    }
    let at = (
        origin.0 + u16::try_from(column).ok()?,
        origin.1 + u16::try_from(row).ok()?,
    );
    let offset = (
        u16::try_from(px % cell_width).ok()?,
        u16::try_from(py % cell_height).ok()?,
    );
    let whole = (left, top, right, bottom) == (0, 0, width, height);
    let crop = (!whole).then(|| Crop {
        x: to_u32(left),
        y: to_u32(top),
        width: to_u32(right - left),
        height: to_u32(bottom - top),
    });
    Some((
        at,
        Placement {
            offset,
            crop,
            ..unplaced()
        },
    ))
}

/// Where a scaled part shows over `spot`. A terminal stretches a picture over whole cells, from
/// any pixel of the first: so the part of the box inside the scene starts at its own pixel and
/// spans as many cells as it is wide and tall, rounded, never past the spot's edge. A box
/// rounded a little bigger or smaller than its own stretches its picture as much, which a zoom
/// moving through it never shows.
fn scaled_geometry(scaled: &Scaled, scene: &Scene, spot: Spot) -> Option<((u16, u16), Placement)> {
    let ([left, top, right, bottom], from) = scaled.cut(scene.width(), scene.height())?;
    let (cell_width, cell_height) = (f64::from(spot.cell.0.max(1)), f64::from(spot.cell.1.max(1)));
    let across = f64::from(spot.cells.width) * cell_width;
    let down = f64::from(spot.cells.height) * cell_height;
    // From the scene's pixels to the spot's.
    let wide = across / scene.width().max(1) as f64;
    let tall = down / scene.height().max(1) as f64;
    let (x, y) = ((left * wide).round(), (top * tall).round());
    let columns = ((right - left) * wide / cell_width)
        .round()
        .min(((across - x) / cell_width).floor());
    let rows = ((bottom - top) * tall / cell_height)
        .round()
        .min(((down - y) / cell_height).floor());
    if columns < 1.0 || rows < 1.0 {
        return None;
    }
    let (column, row) = ((x / cell_width).floor(), (y / cell_height).floor());
    let at = (
        spot.cells.x + u16::try_from(column as i64).ok()?,
        spot.cells.y + u16::try_from(row as i64).ok()?,
    );
    let offset = (
        u16::try_from((x - column * cell_width) as i64).ok()?,
        u16::try_from((y - row * cell_height) as i64).ok()?,
    );
    let image = &scaled.picture.image;
    let whole = (from.x, from.y, from.width, from.height) == (0, 0, image.width(), image.height());
    let crop = (!whole).then(|| Crop {
        x: to_u32(to_i64(from.x)),
        y: to_u32(to_i64(from.y)),
        width: to_u32(to_i64(from.width)),
        height: to_u32(to_i64(from.height)),
    });
    let cells = Some((
        u16::try_from(columns as i64).ok()?,
        u16::try_from(rows as i64).ok()?,
    ));
    Some((
        at,
        Placement {
            offset,
            crop,
            cells,
            ..unplaced()
        },
    ))
}

fn unplaced() -> Placement {
    Placement::new(0, 0)
}

/// A sprite's size in a spot's pixels: as much bigger or smaller than its own as the spot's
/// pixels are than the scene's, a pixel at least.
fn shown_size(image: &Image, scene: &Scene, spot: Spot) -> (usize, usize) {
    let shown = |own: usize, cells: u16, cell: u16, of: usize| {
        let pixels = usize::from(cells) * usize::from(cell.max(1));
        let of = of.max(1);
        ((own * pixels + of / 2) / of).max(1)
    };
    (
        shown(image.width(), spot.cells.width, spot.cell.0, scene.width()),
        shown(
            image.height(),
            spot.cells.height,
            spot.cell.1,
            scene.height(),
        ),
    )
}

/// A part's picture as a spot shows it. A sprite of a scene drawn for another cell size goes
/// out resized as much as the scene is stretched, under a key of its own, so its size keeps
/// with the layers stretched over the cells.
struct Sent {
    key: u64,
    image: Image,
    /// The size it goes out at.
    size: (usize, usize),
}

impl Sent {
    fn of(part: &Part<'_>, scene: &Scene, spot: Spot) -> Self {
        let picture = part.picture();
        let own = (picture.image.width(), picture.image.height());
        // A layer and a scaled part go out at their own pixels, which the terminal stretches.
        let size = match part {
            Part::Layer(_) | Part::Scaled(_) => own,
            Part::Sprite(_) => shown_size(&picture.image, scene, spot),
        };
        let key = match size == own {
            true => picture.key,
            false => picture_key([picture.key, size.0 as u64, size.1 as u64]),
        };
        Self {
            key,
            image: picture.image.clone(),
            size,
        }
    }

    /// Its pixels at the size it goes out at, resized where that isn't its own.
    fn pixels(&self) -> Option<Image> {
        if (self.image.width(), self.image.height()) == self.size {
            return Some(self.image.clone());
        }
        let own = Canvas::from_image(&self.image)?;
        let (width, height) = self.size;
        let mut resized = Canvas::new(u32::try_from(width).ok()?, u32::try_from(height).ok()?);
        let white = Brush::new(Colour::WHITE);
        resized.draw_sized(&own, 0.0, 0.0, width as f64, height as f64, &white);
        Some(resized.to_image())
    }

    /// Bytes of pixels the terminal keeps for it.
    fn bytes(&self) -> usize {
        self.size.0 * self.size.1 * 4
    }
}

fn to_i64(pixels: usize) -> i64 {
    i64::try_from(pixels).unwrap_or(i64::MAX)
}

fn to_u32(pixels: i64) -> u32 {
    u32::try_from(pixels).unwrap_or(u32::MAX)
}

/// Escapes with the cursor moved only where it isn't already.
#[derive(Debug, Default)]
struct Escapes {
    bytes: Vec<u8>,
    cursor: Option<(u16, u16)>,
}

impl Escapes {
    fn move_to(&mut self, at: (u16, u16)) {
        if self.cursor == Some(at) {
            return;
        }
        let mut text = String::new();
        // Writing to a string can't fail.
        let _ = write!(
            text,
            "\x1b[{};{}H",
            u32::from(at.1) + 1,
            u32::from(at.0) + 1
        );
        self.bytes.extend(text.into_bytes());
        self.cursor = Some(at);
    }
}

#[cfg(test)]
mod tests;
