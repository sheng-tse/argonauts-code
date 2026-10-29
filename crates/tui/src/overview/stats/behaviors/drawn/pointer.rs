//! What the pointer and the list mark over a scene: a ring round what the pointer found, where
//! the scene drew it, and a glow under each dot of the behavior picked in the list. Both go on
//! at every scene the screen asks for, at each step of the motion and each move of the pointer,
//! so [`Marks`] keeps the last ring and the glows lit lately, and draws one again only when
//! what it shows changed.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::world::raster::{Brush, Canvas, Cap, Colour, Gradient, Join, Path, Pen};
use crate::world::{Depth, Layer, PartId, Picture, Pointed, Scene, Shape, Sprite, picture_key};

const RING: PartId = PartId::new("pointer ring", 0);
const LIT: PartId = PartId::new("lit dots", 0);
/// How many glows it keeps, the ones lit last: more than the list has rows beside the map, so
/// going up and down them draws each row's glow once.
pub(super) const KEPT_GLOWS: usize = 12;

/// The ring's look: a warm glow, a white ring and a fainter one outside it, `scale` times the
/// size round a dot.
fn ring_at(canvas: &mut Canvas, (x, y): (f64, f64), scale: f64) {
    let glow = Gradient::radial(x, y, 0.0, x, y, 40.0 * scale)
        .stop(0.0, Colour::rgba(255, 240, 210, 0.55))
        .stop(1.0, Colour::rgba(255, 240, 210, 0.0));
    let mut disc = Path::new();
    disc.circle(x, y, 40.0 * scale);
    canvas.fill(&disc, &Brush::new(glow));
    for (radius, width, alpha) in [(20.0, 3.5, 1.0), (28.0, 2.5, 0.45)] {
        let mut ring = Path::new();
        ring.circle(x, y, radius * scale);
        let pen = Pen::new(width * scale);
        canvas.stroke(&ring, &pen, &Brush::new(Colour::WHITE.with_alpha(alpha)));
    }
}

/// A wonder's ring: round its whole box, as a wonder is bigger than a dot.
fn wonder_ring(canvas: &mut Canvas, (x, y): (f64, f64), radius: f64) {
    for (r, width, alpha) in [(radius, 3.0, 1.0), (radius + 6.0, 2.0, 0.4)] {
        let mut ring = Path::new();
        ring.circle(x, y, r);
        canvas.stroke(
            &ring,
            &Pen::new(width),
            &Brush::new(Colour::WHITE.with_alpha(alpha)),
        );
    }
}

/// A river's or a current's trace, along the way it runs.
fn trace(canvas: &mut Canvas, points: &[(f64, f64)], width: f64) {
    let mut path = Path::new();
    for (at, &(x, y)) in points.iter().enumerate() {
        if at == 0 {
            path.move_to(x, y);
        } else {
            path.line_to(x, y);
        }
    }
    let pen = Pen::new(width).cap(Cap::Round).join(Join::Round);
    canvas.stroke(&path, &pen, &Brush::new(Colour::rgba(255, 244, 214, 0.9)));
}

/// How the ring round one mark is drawn.
enum Ringed {
    /// Round a point, `scale` times the size round a dot.
    At((f64, f64), f64),
    /// Round a wonder's box.
    Wonder((f64, f64), f64),
    /// Along a river or a current.
    Trace(Vec<(f64, f64)>, f64),
}

impl Ringed {
    /// The ring round `mark`, found as `pointed`.
    fn of(shape: &Shape, pointed: Pointed) -> Self {
        let at = |x: f32, y: f32| (f64::from(x), f64::from(y));
        match shape {
            Shape::Point { x, y } => {
                let scale = if matches!(pointed, Pointed::Action(_)) {
                    1.0
                } else {
                    0.7
                };
                Ringed::At(at(*x, *y), scale)
            }
            Shape::Disc { x, y, radius }
            | Shape::Ellipse {
                x, y, rx: radius, ..
            } => Ringed::At(at(*x, *y), (f64::from(*radius) / 24.0).clamp(0.5, 1.6)),
            Shape::Rect {
                left,
                top,
                right,
                bottom,
            } => {
                let middle = at((left + right) / 2.0, (top + bottom) / 2.0);
                let (width, height) = (f64::from(right - left), f64::from(bottom - top));
                // The mark reaches 2 px past the wonder's own box on each side.
                Ringed::Wonder(middle, (width - 4.0).hypot(height - 4.0) / 2.0 + 5.0)
            }
            // The mark reaches past the water on both sides; the trace hugs it.
            Shape::Path { points, width } => Ringed::Trace(
                points.iter().map(|&(x, y)| at(x, y)).collect(),
                f64::from(*width) / 2.0 + 2.0,
            ),
        }
    }

    /// Its box on the scene: left, top, right, bottom.
    fn bounds(&self) -> [f64; 4] {
        match self {
            Ringed::At((x, y), scale) => {
                let reach = 40.0 * scale + 2.0;
                [x - reach, y - reach, x + reach, y + reach]
            }
            Ringed::Wonder((x, y), radius) => {
                let reach = radius + 8.0;
                [x - reach, y - reach, x + reach, y + reach]
            }
            Ringed::Trace(points, width) => points.iter().fold(
                [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
                |[left, top, right, bottom], &(x, y)| {
                    [
                        left.min(x - width),
                        top.min(y - width),
                        right.max(x + width),
                        bottom.max(y + width),
                    ]
                },
            ),
        }
    }

    /// A number that changes whenever the ring's look does.
    fn key(&self, pointed: Pointed) -> u64 {
        let numbers: Vec<f64> = match self {
            Ringed::At((x, y), scale) => vec![*x, *y, *scale],
            Ringed::Wonder((x, y), radius) => vec![*x, *y, *radius],
            Ringed::Trace(points, width) => points
                .iter()
                .flat_map(|&(x, y)| [x, y])
                .chain([*width])
                .collect(),
        };
        let mut hasher = DefaultHasher::new();
        format!("{pointed:?}").hash(&mut hasher);
        std::mem::discriminant(self).hash(&mut hasher);
        for number in numbers {
            number.to_bits().hash(&mut hasher);
        }
        picture_key([RING.get(), hasher.finish()])
    }

    /// Draws it with the scene's `(left, top)` at the canvas's corner.
    fn draw(&self, canvas: &mut Canvas, (left, top): (f64, f64)) {
        let moved = |(x, y): (f64, f64)| (x - left, y - top);
        match self {
            Ringed::At(at, scale) => ring_at(canvas, moved(*at), *scale),
            Ringed::Wonder(at, radius) => wonder_ring(canvas, moved(*at), *radius),
            Ringed::Trace(points, width) => {
                let points: Vec<(f64, f64)> = points.iter().map(|at| moved(*at)).collect();
                trace(canvas, &points, *width);
            }
        }
    }
}

/// How the ring round `pointed` goes where `scene` drew it.
fn ringed(scene: &Scene, pointed: Pointed) -> Option<Ringed> {
    let mark = scene
        .hits
        .marks()
        .iter()
        .find(|mark| mark.pointed == pointed)?;
    Some(Ringed::of(&mark.shape, pointed))
}

fn ring_sprite(ringed: &Ringed, key: u64) -> Sprite {
    let [left, top, right, bottom] = ringed.bounds();
    let (left, top) = (left.floor(), top.floor());
    let width = (right.ceil() - left).clamp(1.0, 4096.0) as u32;
    let height = (bottom.ceil() - top).clamp(1.0, 4096.0) as u32;
    let mut canvas = Canvas::new(width, height);
    ringed.draw(&mut canvas, (left, top));
    Sprite {
        id: RING,
        depth: Depth::Pointer,
        at: (left as i32, top as i32),
        picture: Picture::new(key, canvas.to_image()),
    }
}

/// The key of the glow under the `lit` actions' dots, standing where `key` names.
fn lit_picture_key(lit: &[usize], key: u64) -> u64 {
    let mut hashed: Vec<u64> = lit.iter().map(|at| *at as u64).collect();
    hashed.sort_unstable();
    hashed.dedup();
    hashed.extend([key, LIT.get()]);
    picture_key(hashed)
}

/// Where `scene` drew each of the `lit` actions' dots, the first time it drew each.
fn lit_dots(scene: &Scene, lit: &[usize]) -> Vec<(f64, f64)> {
    let lit: HashSet<usize> = lit.iter().copied().collect();
    let mut seen = HashSet::new();
    let mut dots = Vec::new();
    for mark in scene.hits.marks() {
        let (Pointed::Action(at), Shape::Point { x, y }) = (mark.pointed, &mark.shape) else {
            continue;
        };
        let Ok(at) = usize::try_from(at) else {
            continue;
        };
        if lit.contains(&at) && seen.insert(at) {
            dots.push((f64::from(*x), f64::from(*y)));
        }
    }
    dots
}

/// The glow and ring round each of `dots`, over the whole of `scene`, under the picture key
/// `key`.
fn lit_glow(scene: &Scene, dots: &[(f64, f64)], key: u64) -> Option<Layer> {
    let width = u32::try_from(scene.width()).ok()?;
    let height = u32::try_from(scene.height()).ok()?;
    let mut canvas = Canvas::new(width, height);
    for &(x, y) in dots {
        let glow = Gradient::radial(x, y, 0.0, x, y, 16.0)
            .stop(0.0, Colour::rgba(255, 236, 200, 0.5))
            .stop(1.0, Colour::rgba(255, 236, 200, 0.0));
        let mut disc = Path::new();
        disc.circle(x, y, 16.0);
        canvas.fill(&disc, &Brush::new(glow));
        let mut ring = Path::new();
        ring.circle(x, y, 7.0);
        canvas.stroke(&ring, &Pen::new(2.0), &Brush::new(Colour::WHITE));
    }
    Some(Layer {
        id: LIT,
        depth: Depth::Pointer,
        picture: Picture::new(key, canvas.to_image()),
    })
}

/// The marks last laid over a scene, each kept by what it shows, so asking again for the same
/// draws nothing.
#[derive(Debug, Default)]
pub struct Marks(Mutex<Kept>);

#[derive(Debug, Default)]
struct Kept {
    /// The glows lit last, by key, the newest last.
    lit: Vec<(u64, Option<Layer>)>,
    ring: Option<(u64, Sprite)>,
}

impl Marks {
    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A glow under each of the `lit` actions' dots where `scene` drew them, and a bright ring
    /// around each, as a layer over everything; `None` with none lit. It is drawn only when no
    /// glow lit lately has the same dots where they stand, in a scene of the same size.
    pub fn lit(&self, scene: &Scene, lit: &[usize]) -> Option<Layer> {
        if lit.is_empty() {
            return None;
        }
        let dots = lit_dots(scene, lit);
        let mut hasher = DefaultHasher::new();
        (scene.width(), scene.height()).hash(&mut hasher);
        for (x, y) in &dots {
            (x.to_bits(), y.to_bits()).hash(&mut hasher);
        }
        let key = lit_picture_key(lit, hasher.finish());
        let mut kept = self.kept();
        if let Some(at) = kept.lit.iter().position(|(was, _)| *was == key) {
            let again = kept.lit.remove(at);
            let layer = again.1.clone();
            kept.lit.push(again);
            return layer;
        }
        let layer = lit_glow(scene, &dots, key);
        if kept.lit.len() >= KEPT_GLOWS {
            kept.lit.remove(0);
        }
        kept.lit.push((key, layer.clone()));
        layer
    }

    /// The ring round `pointed` where `scene` drew it, as a sprite over everything; `None`
    /// where the scene drew no mark of it. It is drawn again only when its place or shape
    /// changed.
    pub fn ring(&self, scene: &Scene, pointed: Pointed) -> Option<Sprite> {
        let ringed = ringed(scene, pointed)?;
        let key = ringed.key(pointed);
        let mut kept = self.kept();
        if let Some((was, sprite)) = &kept.ring
            && *was == key
        {
            return Some(sprite.clone());
        }
        let sprite = ring_sprite(&ringed, key);
        kept.ring = Some((key, sprite.clone()));
        Some(sprite)
    }

    /// Lets go of what it keeps.
    pub fn forget(&self) {
        *self.kept() = Kept::default();
    }
}
