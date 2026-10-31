//! The wonders drawn where they rose: each from one rare thing in your agents' records.
//!
//! A wonder stands by the behavior its record is about, on the nearest ground of its own kind:
//! a ship on open water, rocks in a strait, a pyramid in desert. It covers no dot, nothing of
//! nature's and no other wonder. Nothing names a wonder until the pointer finds it, and one
//! that has not risen is never drawn. On the planet the same wonders stand upright at their
//! map places, lit by its sun.
//!
//! Each is drawn in three passes, its shade, its dark rim and its lit body, then the glow of
//! any fire it holds, so it reads as standing on the land at the map's own small size.

mod art;
mod hand;
mod place;

#[cfg(test)]
mod tests;

pub use art::Detail;
pub use place::{Placed, Terrain, place};

use argonauts_core::behaviors::behavior_world::{RisenWonder, Wonder};

use crate::world::map::fields::{Ground, Look};
use crate::world::map::nature::{NatureFacts, Sunk};
use crate::world::raster::{Brush, Canvas, Colour};
use hand::{Hand, Pass};

/// How far below its box's middle a wonder's ground line lies, in sizes.
pub const GROUND: f64 = 0.3;

/// A wonder's size on the map, in pixels: a dot is about 9 px across.
pub const ON_MAP: f64 = 34.0;
/// Atlantis is a whole city of rings lying flat on the water, so it is drawn larger, to span
/// four cells and more at the terminal's size.
pub const ATLANTIS_ON_MAP: f64 = 84.0;

/// The light the wonders are drawn and measured in on the flat map: from the north-west, 42°
/// up, as a unit vector with x right, y down the screen and z up.
pub fn key_light() -> [f64; 3] {
    let azimuth = 315.0_f64.to_radians();
    let altitude = 42.0_f64.to_radians();
    [
        azimuth.sin() * altitude.cos(),
        -azimuth.cos() * altitude.cos(),
        altitude.sin(),
    ]
}

/// The land that sank where Atlantis rose: one of your busiest behaviors gone unused while your
/// crew kept working. Its atoll gives way to the city, over a pale bank under the water.
pub fn sunk(ground: &Ground, facts: &NatureFacts, risen: &[RisenWonder]) -> Option<Sunk> {
    if ground.look == Look::Gas {
        return None;
    }
    let atlantis = risen.iter().find(|rose| rose.wonder == Wonder::Atlantis)?;
    let behavior = ground
        .facts
        .behaviors
        .iter()
        .position(|days| days.name == atlantis.behavior)?;
    Some(Sunk {
        behavior,
        before: facts.of(behavior).last_period,
    })
}

/// A wonder's size on the map.
pub fn size_on_map(wonder: Wonder) -> f64 {
    if wonder == Wonder::Atlantis {
        ATLANTIS_ON_MAP
    } else {
        ON_MAP
    }
}

/// Draws `wonder` `size` px big with the middle of its box at (x, y), lit from `light`.
pub fn draw(canvas: &mut Canvas, wonder: Wonder, (x, y): (f64, f64), size: f64, light: [f64; 3]) {
    if size.is_nan() || size <= 0.0 {
        return;
    }
    let detail = Detail::at(size);
    let pad = (size * 0.6).ceil();
    let side = (size + 2.0 * pad).ceil();
    let (left, top) = (x - side / 2.0, y - side / 2.0);
    let ground = (side / 2.0, side / 2.0 + GROUND * size);
    let blur = ((size / 48.0).max(0.5) * 100.0).round() / 100.0;
    let rim = if size >= 56.0 { 0.42 } else { 0.55 };
    for (pass, alpha) in [(Pass::Cast, 0.3), (Pass::Rim, rim), (Pass::Body, 1.0)] {
        let mut layer = Canvas::new(side as u32, side as u32);
        let mut hand = Hand::new(&mut layer, ground, size, light, pass);
        art::draw(&mut hand, wonder, detail);
        let mut brush = Brush::new(Colour::WHITE).alpha(alpha);
        if pass == Pass::Cast {
            brush = brush.blur(blur);
        }
        canvas.draw(&layer, left, top, &brush);
    }
    let mut hand = Hand::new(canvas, (x, y + GROUND * size), size, light, Pass::Glow);
    art::draw(&mut hand, wonder, detail);
}

/// A wonder drawn alone, for standing it on the stood-up map or the planet: its picture and the
/// point of it that stands on the ground.
#[derive(Debug, Clone)]
pub struct Sprite {
    pub canvas: Canvas,
    pub ground: (f64, f64),
}

pub fn sprite(wonder: Wonder, size: f64, light: [f64; 3]) -> Sprite {
    let side = (size * 2.6).ceil();
    let middle = side / 2.0;
    let mut canvas = Canvas::new(side as u32, side as u32);
    draw(&mut canvas, wonder, (middle, middle), size, light);
    Sprite {
        canvas,
        ground: (middle, middle + GROUND * size),
    }
}

/// The box a wonder's picture fills at a size, its shade and rim included, from its ground
/// point: measured by drawing it once, so a wide ship and a tall tower each keep clear of
/// exactly what they would cover.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
}

pub fn bounds(wonder: Wonder, size: f64) -> Bounds {
    let drawn = sprite(wonder, size, key_light());
    let (width, height) = (drawn.canvas.width(), drawn.canvas.height());
    let rgba = drawn.canvas.to_rgba();
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
    let mut any = false;
    for j in 0..height {
        for i in 0..width {
            if rgba[((j * width + i) * 4 + 3) as usize] > 24 {
                any = true;
                x0 = x0.min(i);
                x1 = x1.max(i);
                y0 = y0.min(j);
                y1 = y1.max(j);
            }
        }
    }
    if !any {
        return Bounds {
            x0: -size / 2.0,
            x1: size / 2.0,
            y0: -size,
            y1: 0.0,
        };
    }
    let (middle, ground) = drawn.ground;
    Bounds {
        x0: f64::from(x0) - middle,
        x1: f64::from(x1) + 1.0 - middle,
        y0: f64::from(y0) - ground,
        y1: f64::from(y1) + 1.0 - ground,
    }
}

/// A wonder's picture for the line under the map: two terminal cells, 72 × 84 pixels, the wonder
/// on a tile of the ground it belongs on, so the tile says where on the map to look for it.
pub fn icon(wonder: Wonder) -> Canvas {
    use crate::world::map::climate::{Climate, ROCK};
    use crate::world::map::fields::Rgb;
    use crate::world::map::weather::icon as nature_icon;
    use crate::world::raster::{Path, Transform};

    let rgb = |c: Rgb| Colour::rgb(c[0] as u8, c[1] as u8, c[2] as u8);
    let sea = Colour::rgb(20, 56, 90);
    let forest = rgb(Climate::Forest.rgb());
    let (base, edge) = match Terrain::of(wonder) {
        Terrain::Sea => (sea, None),
        Terrain::Strait => (sea, Some(forest)),
        Terrain::Coast => (forest, Some(sea)),
        Terrain::Lush => (forest, None),
        Terrain::Desert => (rgb(Climate::Desert.rgb()), None),
        Terrain::Peak => (rgb(ROCK), None),
        Terrain::Grass | Terrain::Any | Terrain::Free => (rgb(Climate::Grassland.rgb()), None),
    };
    let mut canvas = Canvas::new(nature_icon::WIDTH, nature_icon::HEIGHT);
    canvas.with_clip(&nature_icon::tile(), Transform::IDENTITY, |canvas| {
        canvas.fill_rect(4.0, 10.0, 64.0, 64.0, &Brush::new(base));
        match (Terrain::of(wonder), edge) {
            (Terrain::Coast, Some(edge)) => {
                let mut shore = Path::new();
                shore.move_to(4.0, 62.0);
                shore.quad_to(36.0, 54.0, 68.0, 66.0);
                shore.line_to(68.0, 74.0);
                shore.line_to(4.0, 74.0);
                canvas.fill(&shore, &Brush::new(edge));
            }
            (Terrain::Strait, Some(edge)) => {
                for x in [4.0, 68.0] {
                    let mut side = Path::new();
                    side.ellipse(x, 42.0, 12.0, 40.0, 0.0, 0.0, 7.0, false);
                    canvas.fill(&side, &Brush::new(edge));
                }
            }
            _ => {}
        }
    });
    let b = bounds(wonder, 60.0);
    let k = f64::min(58.0 / (b.x1 - b.x0), 58.0 / (b.y1 - b.y0));
    let size = 60.0 * k;
    let gx = 36.0 - (b.x0 + b.x1) / 2.0 * k;
    let gy = 42.0 - (b.y0 + b.y1) / 2.0 * k;
    draw(
        &mut canvas,
        wonder,
        (gx, gy - GROUND * size),
        size,
        key_light(),
    );
    canvas
}
