//! The land's colour under its light, seen straight down: what grows, what nature lays on it,
//! the water's depth, lit by the sun and the open sky, with the coast's hills mirrored a little
//! in the water below them.

use super::{FORESHORTEN, Light, Relief, STANDING};
use crate::world::map::fields::{Ground, HEIGHT, Look, WIDTH, in_rows};
use crate::world::map::land::{Marks, Polar, Surface, Tint};
use crate::world::raster::noise::{mix, vnoise3};

/// How far down the water mirrors the land north of it, in pixels.
const MIRROR_REACH: usize = 44;

/// The lit ground, red, green and blue from 0 to 255 for each pixel of the map, unrounded.
#[derive(Debug, Clone)]
pub struct LitGround {
    pub colour: Vec<[f32; 3]>,
}

impl LitGround {
    pub fn new(
        ground: &Ground,
        relief: &Relief,
        light: &Light,
        tint: Option<&Tint>,
        marks: Marks<'_>,
        polar: Option<&Polar>,
    ) -> Self {
        // A gas giant has no polar ice.
        let polar = polar.filter(|_| ground.look != Look::Gas);
        let surface = Surface {
            tint,
            marks,
            ..Surface::map(ground, polar)
        };
        let mut colour = vec![[0.0_f32; 3]; WIDTH * HEIGHT];
        in_rows(&mut colour, WIDTH, |y, row| {
            for (x, out) in row.iter_mut().enumerate() {
                *out = lit_at(&surface, relief, light, x, y);
            }
        });
        if let Some(sunk) = marks.sunk {
            for (at, out) in colour.iter_mut().enumerate() {
                let (x, y) = (at % WIDTH, at / WIDTH);
                let sea = relief.sea.get(x, y);
                if sea < 0.5 {
                    continue;
                }
                if let Some((k, bank)) = sunk(x as f64, y as f64) {
                    let water = 1.0 - 0.32 * relief.shade.get(x, y);
                    for ch in 0..3 {
                        out[ch] = mix(f64::from(out[ch]), bank[ch] * water, k * sea) as f32;
                    }
                }
            }
        }
        let mirrored = mirror(relief, &colour);
        Self { colour: mirrored }
    }
}

fn lit_at(surface: &Surface<'_>, relief: &Relief, light: &Light, x: usize, y: usize) -> [f32; 3] {
    let at = y * WIDTH + x;
    let shown = surface.at(x as f64 + 0.5, y as f64 + 0.5, Some(relief.coast.get(x, y)));
    let wet = 1.0 - shown.land;
    let alpha =
        |layer: Option<&[u8]>| layer.map_or(0.0, |rgba| f64::from(rgba[at * 4 + 3]) / 255.0);
    let scar = alpha(surface.marks.scar);
    // A mark laid on the ground, such as a lake or a marsh, hides the crowns under it.
    let laid = (alpha(surface.marks.ground) - scar).max(0.0);
    let texture = mix(f64::from(light.texture[at]), 1.0, laid);
    let water = 1.0 - 0.32 * relief.shade.get(x, y);
    [0, 1, 2].map(|ch| {
        let grown = f64::from(light.light[at][ch]);
        let mut lit = if laid > 0.0 {
            mix(grown, f64::from(light.bare[at][ch]), laid)
        } else {
            grown
        };
        // A burn scar keeps at least 0.6 of the light, so ash on a lee slope never reads as a
        // hole.
        if scar > 0.0 {
            lit = mix(lit, lit.max(0.6), scar);
        }
        let colour = shown.colour[ch];
        mix(colour * texture * lit, colour * water, wet) as f32
    })
}

/// The water mirrors the land just north of it, as seen from the south: a place `d` px further
/// north shows here if it stands at least `d` times the foreshortening tall. Faint, and broken by
/// ripples.
fn mirror(relief: &Relief, colour: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let k = FORESHORTEN / STANDING;
    let mut out = colour.to_vec();
    in_rows(&mut out, WIDTH, |y, row| {
        if y == 0 {
            return;
        }
        for (x, pixel) in row.iter_mut().enumerate() {
            let sea = relief.sea.get(x, y);
            if sea < 0.6 {
                continue;
            }
            let wave = 1.1
                * (y as f64 * 1.7 + 3.0 * vnoise3(x as f64 / 6.0, y as f64 / 2.0, 4.0, 9.0)).sin();
            let ripple = (wave + 0.5).floor() as i64;
            let column = (x as i64 + ripple).clamp(0, WIDTH as i64 - 1) as usize;
            for d in 1..=MIRROR_REACH.min(y) {
                let above = y - d;
                if relief.sea.get(column, above) > 0.5 {
                    continue;
                }
                if relief.standing.get(column, above) >= d as f64 * k {
                    let fade = 0.3 * (1.0 - d as f64 / MIRROR_REACH as f64) * sea;
                    let shown = colour[above * WIDTH + column];
                    for ch in 0..3 {
                        pixel[ch] =
                            mix(f64::from(pixel[ch]), f64::from(shown[ch]) * 0.62, fade) as f32;
                    }
                    break;
                }
            }
        }
    });
    out
}
