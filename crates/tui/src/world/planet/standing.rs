//! What stands upright on the globe, as the wonders do: at its place on the map, smaller than
//! on the map as the globe is, faded toward the rim like the dots and dark on the night side.

use std::fmt::Debug;

use super::Sun;
use crate::world::geo::{Globe, Plate};
use crate::world::raster::noise::smooth;
use crate::world::raster::{Brush, Canvas, Colour};
use crate::world::scene::{Pointed, picture_key};

/// How tall a wonder of the usual size stands on the map, and on the planet at its usual
/// radius of 240 px, where a dot is about 9 px across.
pub const ON_MAP: f64 = 34.0;
pub const ON_PLANET: f64 = 22.0;

/// How far toward the rim something standing still shows.
const RIM: f64 = 0.12;

/// Something that stands upright on the planet, drawn by whoever draws it on the map.
pub trait Standing: Debug + Send + Sync {
    /// Where it stands on the map, in the map's pixels.
    fn place(&self) -> (f64, f64);

    /// How tall it stands on the map, in pixels.
    fn size(&self) -> f64;

    /// Its picture `size` px tall, lit from `light` (x right, y down the screen, z toward
    /// you), and the point of the picture that stands on the ground.
    fn picture(&self, size: f64, light: [f64; 3]) -> (Canvas, (f64, f64));

    /// What the pointer finds on it.
    fn pointed(&self) -> Pointed;

    /// Tells it from others in the keys of the pictures it is drawn into.
    fn fingerprint(&self) -> u64 {
        let (x, y) = self.place();
        picture_key([x.to_bits(), y.to_bits(), self.size().to_bits()])
    }
}

/// Where something standing shows on a globe: the point it stands on, how tall it is there,
/// how much of it shows toward the rim and how much of the day it is in.
pub struct Stands {
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub shows: f64,
    pub day: f64,
}

/// Where `thing` stands on `globe` while it faces you enough to show.
pub fn stands(thing: &dyn Standing, globe: &Globe, sun: Sun) -> Option<Stands> {
    let (x, y) = thing.place();
    let at = globe.project(Plate::MAP.place(x, y));
    if at.z <= RIM {
        return None;
    }
    let (cx, cy) = globe.centre;
    let mu = sun.facing([
        (at.x - cx) / globe.radius,
        -(at.y - cy) / globe.radius,
        at.z,
    ]);
    Some(Stands {
        x: at.x,
        y: at.y,
        size: ON_PLANET * globe.radius / 240.0 * (thing.size() / ON_MAP),
        shows: smooth(RIM, 0.36, at.z),
        day: smooth(-0.06, 0.12, mu),
    })
}

/// Everything in `standing` drawn upright onto a canvas whose top left is at `origin` of the
/// scene.
pub fn draw(
    canvas: &mut Canvas,
    standing: &[&dyn Standing],
    globe: &Globe,
    sun: Sun,
    origin: (f64, f64),
) {
    // The planet's sun in the pictures' own frame, where y runs down the screen.
    let light = [sun.to[0], -sun.to[1], sun.to[2]];
    for thing in standing {
        let Some(at) = stands(*thing, globe, sun) else {
            continue;
        };
        let (picture, (gx, gy)) = thing.picture(at.size, light);
        let mut brush = Brush::new(Colour::WHITE).alpha(at.shows * (0.4 + 0.6 * at.day));
        if at.day < 0.99 {
            brush = brush.brightness(0.18 + 0.82 * at.day);
        }
        canvas.draw(&picture, at.x - gx - origin.0, at.y - gy - origin.1, &brush);
    }
}

/// The box round something standing where the pointer finds it: a little wider than it is and
/// down to its foot.
pub fn reach(at: &Stands) -> (f32, f32, f32, f32) {
    (
        (at.x - at.size * 0.6) as f32,
        (at.y - at.size * 1.05) as f32,
        (at.x + at.size * 0.6) as f32,
        (at.y + at.size * 0.35) as f32,
    )
}
