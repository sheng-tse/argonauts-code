//! The texture the planet wraps round its globe: the map's ground, with open sea where the map
//! ends, tundra by the ice, and the ice itself joined round the poles.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use super::ice::{Polar, polar_at};
use super::sea::{SeaColours, TUNDRA};
use super::surface::{Marks, Surface};
use crate::world::geo::{LonLat, Plate};
use crate::world::map::fields::{Ground, Rgb, fine, in_rows, mix_rgb, to_bytes};
use crate::world::map::land::Tint;
use crate::world::raster::noise::{fbm3, smooth};

/// The ground wrapped round the globe, pole to pole, row by row from the north.
#[derive(Debug, Clone)]
pub struct Texture {
    pub width: usize,
    pub height: usize,
    /// Three bytes a texel.
    pub rgb: Vec<u8>,
    /// Whether each texel is water, which the sun glints on.
    pub wet: Vec<bool>,
    /// The ground's slope east and north at each texel, for the globe's light.
    pub slope_east: Vec<f32>,
    pub slope_north: Vec<f32>,
}

impl Texture {
    /// The crew's ground as the planet shows it, its sea in the planet's own colours.
    pub fn new(ground: &Ground, tint: Option<&Tint>, marks: Marks<'_>) -> Self {
        let plate = Plate::TEXTURE;
        let (width, height) = (plate.width() as usize, plate.height() as usize);
        let polar = Polar::of(ground);
        let polar = if marks.icebergs {
            polar
        } else {
            polar.without_bergs()
        };
        let surface = Surface {
            ground,
            sea: SeaColours::planet(ground.look),
            tint,
            step_lines: false,
            shade: false,
            cold: super::surface::cold(ground),
            polar: None,
            marks,
        };
        let mut texels = vec![Texel::default(); width * height];
        in_rows(&mut texels, width, |row, out| {
            let lat = FRAC_PI_2 - (row as f64 + 0.5) / height as f64 * PI;
            for (column, texel) in out.iter_mut().enumerate() {
                let lon = (column as f64 + 0.5) / width as f64 * TAU - PI;
                *texel = texel_at(&surface, &polar, lon, lat);
            }
        });
        let mut texture = Self {
            width,
            height,
            rgb: Vec::with_capacity(width * height * 3),
            wet: Vec::with_capacity(width * height),
            slope_east: Vec::with_capacity(width * height),
            slope_north: Vec::with_capacity(width * height),
        };
        for texel in texels {
            texture.rgb.extend(to_bytes(texel.colour));
            texture.wet.push(texel.wet);
            texture.slope_east.push(texel.east as f32);
            texture.slope_north.push(texel.north as f32);
        }
        texture
    }

    /// The texel's colour at a column and row.
    pub fn colour(&self, column: usize, row: usize) -> [u8; 3] {
        let at = (row * self.width + column) * 3;
        [self.rgb[at], self.rgb[at + 1], self.rgb[at + 2]]
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Texel {
    colour: Rgb,
    wet: bool,
    east: f64,
    north: f64,
}

fn texel_at(surface: &Surface<'_>, polar: &Polar, lon: f64, lat: f64) -> Texel {
    let ground = surface.ground;
    let map = Plate::MAP;
    let seed = ground.seed();
    let sphere = (lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin());
    let open_sea = || {
        let (sx, sy, sz) = sphere;
        let deep = 0.06 + 0.22 * fbm3(sx * 2.2, sy * 2.2, sz * 2.2, seed + 3.0, 4);
        mix_rgb(surface.sea.abyss, surface.sea.shelf, deep.clamp(0.0, 0.6))
    };
    let mut texel = Texel::default();
    if lon.abs() < PI && lat.abs() < map.lat_max() {
        let (x, y) = map.pixel(LonLat::new(lon, lat));
        let shown = surface.at(x, y, None);
        texel.colour = shown.colour;
        texel.wet = shown.land < 0.5;
        if texel.wet
            && let Some(sunk) = surface.marks.sunk
            && let Some((k, bank)) = sunk(x, y)
            && k > 0.01
        {
            texel.colour = mix_rgb(texel.colour, bank, k);
        }
        // The map's last few degrees fade into the open sea beyond it.
        let fade = smooth(map.lat_max() - 0.16, map.lat_max(), lat.abs()).max(smooth(
            PI - 0.2,
            PI,
            lon.abs(),
        ));
        if fade > 0.0 {
            texel.colour = mix_rgb(texel.colour, open_sea(), fade);
            if fade > 0.5 {
                texel.wet = true;
            }
        }
        if !texel.wet {
            let k = 1.25 * (1.0 - fade);
            texel.east = -fine(&ground.fine_slope_x, x, y) * k;
            texel.north = fine(&ground.fine_slope_y, x, y) * k;
        }
        let to_cap = if lat > 0.0 {
            polar.cap_north.map_or(9.0, |_| polar.edge_north() - lat)
        } else {
            polar.cap_south.map_or(9.0, |_| polar.edge_south() + lat)
        };
        if !texel.wet && to_cap < 0.14 {
            texel.colour = mix_rgb(texel.colour, TUNDRA, smooth(0.14, 0.02, to_cap) * 0.75);
        }
    } else {
        texel.colour = open_sea();
        texel.wet = true;
    }
    if let Some(ice) = polar_at(polar, lon, lat, sphere, texel.colour, seed) {
        texel.colour = ice.colour;
        if ice.solid {
            texel.wet = false;
        }
        if let Some(slope) = ice.slope {
            texel.east = slope;
            texel.north = 0.0;
        }
    }
    texel
}
