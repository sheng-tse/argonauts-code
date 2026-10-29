//! The light on the land: the sun and the open sky on each slope, with the fine relief of what
//! grows there. Crowns, dunes and broken rock are laid out in screen rows, where each place
//! lands for the height the land stands at, so a slope the view stretches still shows round
//! crowns rather than streaks.

use std::f64::consts::TAU;

use super::{FORESHORTEN, Relief, STANDING, sun, top_row};
use crate::world::map::climate::rock_grain;
use crate::world::map::fields::{COARSE, Ground, HEIGHT, WIDTH, in_rows};
use crate::world::map::land::cold;
use crate::world::raster::noise::{fbm3, hash3, mix, smooth, vnoise3};

/// The sunlight's colour, and the sky's, cooler, so a lee slope reads as shade and not as a hole.
const SUN_COLOUR: [f64; 3] = [1.07, 1.0, 0.88];
const SKY_COLOUR: [f64; 3] = [0.78, 0.92, 1.18];
/// How much light comes from the open sky and from the sun, and how much sunlit ground bounces
/// back into the shade, so a steep lee slope keeps about a third of the lit value.
const SKY: f64 = 0.5;
const DIRECT: f64 = 0.53;
const BOUNCE: f64 = 0.13;

/// The light on each pixel of the map, for the land standing or lying flat.
#[derive(Debug, Clone)]
pub struct Light {
    /// Whether it was worked out for the land standing.
    pub standing: bool,
    /// Red, green and blue light on each pixel, with what grows there.
    pub light: Vec<[f32; 3]>,
    /// The bare ground's own light, for marks laid over what grows.
    pub bare: Vec<[f32; 3]>,
    /// How dark the canopy or the grass makes each pixel, 1 for none.
    pub texture: Vec<f32>,
}

impl Light {
    /// The light for the land standing `stand` px a unit of height, worked out for standing or
    /// lying flat, whichever is nearer.
    pub fn new(ground: &Ground, relief: &Relief, stand: f64) -> Self {
        let standing = stand >= STANDING / 2.0;
        let stand = if standing { STANDING } else { 0.0 };
        let mut pixels = vec![Pixel::default(); WIDTH * HEIGHT];
        in_rows(&mut pixels, WIDTH, |y, row| {
            for (x, out) in row.iter_mut().enumerate() {
                *out = light_at(ground, relief, stand, x, y);
            }
        });
        Self {
            standing,
            light: pixels.iter().map(|p| p.light).collect(),
            bare: pixels.iter().map(|p| p.bare).collect(),
            texture: pixels.iter().map(|p| p.texture).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Pixel {
    light: [f32; 3],
    bare: [f32; 3],
    texture: f32,
}

/// One tree crown under a point, from a jittered grid of `size` px cells with crowns of
/// `radius` px, a `density` of the cells holding one: its slope for the light, its height 0 to
/// 1, and which tree it is for its own shade. `None` between the crowns.
struct Crown {
    sx: f64,
    sy: f64,
    height: f64,
    which: f64,
}

fn crown(x: f64, y: f64, size: f64, radius: f64, density: f64, seed: f64) -> Option<Crown> {
    let (ci, cj) = ((x / size).floor(), (y / size).floor());
    let mut best = 1e9;
    let (mut bx, mut by, mut which) = (0.0, 0.0, 0.0);
    for dj in [-1.0, 0.0, 1.0] {
        for di in [-1.0, 0.0, 1.0] {
            let (i, j) = (ci + di, cj + dj);
            if density < 1.0 && hash3(i, j, 1.0, seed) > density {
                continue;
            }
            let cx = (i + 0.15 + 0.7 * hash3(i, j, 2.0, seed)) * size;
            let cy = (j + 0.15 + 0.7 * hash3(i, j, 3.0, seed)) * size;
            let d2 = (x - cx).powi(2) + (y - cy).powi(2);
            if d2 < best {
                best = d2;
                bx = x - cx;
                by = y - cy;
                which = hash3(i, j, 4.0, seed);
            }
        }
    }
    let d = best.sqrt();
    if d >= radius {
        return None;
    }
    let height = (1.0 - (d / radius).powi(2)).sqrt();
    let k = 1.0 / (radius * height.max(0.35));
    Some(Crown {
        sx: bx * k,
        sy: by * k,
        height,
        which,
    })
}

fn light_at(ground: &Ground, relief: &Relief, stand: f64, x: usize, y: usize) -> Pixel {
    let sun = sun();
    let seed = ground.seed();
    let g = COARSE as f64;
    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
    let (gx, gy) = (px / g, py / g);
    let land = (relief.coast.get(x, y) + 0.5).clamp(0.0, 1.0);
    let screen_y = top_row() + FORESHORTEN * py - stand * relief.standing.get(x, y);
    let height = &relief.lit_height;
    let (left, right) = (x.saturating_sub(1), (x + 1).min(WIDTH - 1));
    let (up, down) = (y.saturating_sub(1), (y + 1).min(HEIGHT - 1));
    let bare_x = -(height.get(right, y) - height.get(left, y)) / (right - left) as f64;
    let bare_y = -(height.get(x, down) - height.get(x, up)) / (down - up) as f64;
    let (mut nx, mut ny) = (bare_x, bare_y);
    let (mut shaded, mut texture) = (1.0, 1.0);
    if land > 0.01 {
        let noise = ground.climate.edge_noise.sample(px / 2.0, py / 2.0);
        let wet = ground.climate.field.sample(gx, gy) + 0.55 * noise;
        let log = ground.height.sample(gx, gy).max(1e-3).log10() + 0.05 * noise;
        let grain = rock_grain(px, py);
        let rock = smooth(1.36, 1.56, log + grain);
        let snow = smooth(1.92, 2.06, log + grain);
        let open = (1.0 - rock) * (1.0 - snow) * (1.0 - (cold(ground) * 2.0).min(1.0));
        let forest = smooth(2.42, 2.58, wet) * open;
        let rainforest = smooth(3.42, 3.58, wet);
        let savanna = smooth(0.42, 0.58, wet) * (1.0 - smooth(1.42, 1.58, wet)) * open;
        let meadow = smooth(1.42, 1.58, wet) * (1.0 - smooth(2.42, 2.58, wet)) * open;
        let desert = (1.0 - smooth(0.42, 0.58, wet)) * open;
        if forest > 0.02 {
            let tree = if rainforest > 0.5 {
                crown(px, screen_y, 2.6, 1.95, 1.0, seed)
            } else {
                crown(px, screen_y, 3.4, 1.8, 0.9, seed + 1.0)
            };
            match tree {
                Some(tree) => {
                    nx += 0.95 * forest * tree.sx;
                    ny += 0.95 * forest * tree.sy;
                    shaded = mix(1.0, 0.8 + 0.22 * tree.height, forest);
                    texture = mix(1.0, 0.88 + 0.24 * tree.which, forest);
                }
                None => {
                    let gap = if rainforest > 0.5 { 0.58 } else { 0.66 };
                    shaded = mix(1.0, gap, forest);
                    texture = mix(1.0, 0.86, forest);
                }
            }
        }
        // A savanna's acacias: wide flat crowns, far apart, dark on the straw.
        if savanna > 0.02
            && let Some(tree) = crown(px, screen_y, 8.0, 2.3, 0.16, seed + 2.0)
        {
            nx += 0.7 * savanna * tree.sx;
            ny += 0.5 * savanna * tree.sy;
            texture = mix(texture, 0.52 + 0.1 * tree.which, savanna);
            shaded *= mix(1.0, 0.86 + 0.12 * tree.height, savanna);
        }
        // A meadow: open grass, a rare shrub, the grass's own fine ruffle.
        if meadow > 0.02 {
            texture = match crown(px, screen_y, 9.0, 1.3, 0.05, seed + 3.0) {
                Some(_) => mix(texture, 0.78, meadow),
                None => mix(
                    texture,
                    1.0 + 0.05 * vnoise3(px / 1.8, screen_y / 1.8, 5.0, 11.0),
                    meadow,
                ),
            };
        }
        // Dunes across the wind, long on their windward side and steep on their lee.
        if desert > 0.02 {
            let wave = fbm3(px / 60.0, py / 60.0, 2.9, 62.0, 2);
            let angle = 0.62 + 0.5 * wave;
            let (ux, uy) = (angle.cos(), angle.sin());
            let phase = (px * ux + screen_y * uy) / (7.0 + 3.0 * wave)
                + 1.6 * fbm3(px / 38.0, py / 38.0, 2.2, 61.0, 3);
            let ridge = phase - phase.floor();
            let dune = (ridge * TAU + 0.9 * (ridge * TAU).sin()).sin()
                * 0.2
                * desert
                * smooth(0.1, 0.5, fbm3(px / 24.0, py / 24.0, 4.4, 63.0, 2) + 0.35);
            nx -= dune * ux;
            ny -= dune * uy;
        }
        // A plateau's top lies flat, not broken.
        let flat = 1.0 - 0.75 * smooth(0.84, 1.0, ground.mesa_at(gx, gy));
        if rock > 0.02 {
            nx += 0.55 * rock * flat * vnoise3(px / 2.4, screen_y / 2.4, 7.0, 3.0);
            ny += 0.55 * rock * flat * vnoise3(px / 2.4, screen_y / 2.4, 9.0, 3.0);
        }
    }
    let lit_height = height.get(x, y);
    let in_shade = relief.shade.get(x, y);
    let open_sky = (1.0
        - 0.02 * (relief.near.get(x, y) - lit_height)
        - 0.008 * (relief.far.get(x, y) - lit_height))
        .clamp(0.6, 1.08);
    let light = |nx: f64, ny: f64, open: f64| {
        let length = (nx * nx + ny * ny + 1.0).sqrt();
        let (mx, my, mz) = (nx / length, ny / length, 1.0 / length);
        let direct =
            (mx * sun[0] + my * sun[1] + mz * sun[2]).max(0.0) / sun[2] * (1.0 - 0.68 * in_shade);
        let sky = (0.58 + 0.42 * mz) * open;
        [0, 1, 2].map(|ch| {
            (SKY * sky * SKY_COLOUR[ch]
                + DIRECT * direct * SUN_COLOUR[ch]
                + BOUNCE * open * SUN_COLOUR[ch]) as f32
        })
    };
    let bare = {
        let length = (bare_x * bare_x + bare_y * bare_y + 1.0).sqrt();
        let direct = ((bare_x * sun[0] + bare_y * sun[1] + sun[2]) / length).max(0.0) / sun[2]
            * (1.0 - 0.68 * in_shade);
        let sky = (0.58 + 0.42 / length) * open_sky;
        [0, 1, 2].map(|ch| {
            (SKY * sky * SKY_COLOUR[ch]
                + DIRECT * direct * SUN_COLOUR[ch]
                + BOUNCE * open_sky * SUN_COLOUR[ch]) as f32
        })
    };
    Pixel {
        light: light(nx, ny, open_sky * shaded),
        bare,
        texture: texture as f32,
    }
}
