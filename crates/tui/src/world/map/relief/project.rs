//! The picture: the lit land stood up in plan oblique, drawn column by column from the near edge
//! to the far one, each place filling the rows its slope opens on the screen, as heightfield
//! terrain is drawn but without perspective. It is drawn at twice the size each way and
//! averaged, and keeps for each pixel which row of the flat map it shows, so the pointer finds
//! the place behind a mountain's face.

use std::sync::Arc;

use super::{FORESHORTEN, LitGround, Relief, top_row};
use crate::world::geo::Plate;
use crate::world::map::fields::{HEIGHT, WIDTH, byte, in_rows};
use crate::world::raster::noise::{mix, smooth};
use crate::world::raster::{Canvas, Grid};
use crate::world::scene::Under;

/// The stood-up land as a picture, and the flat map's row each of its pixels shows.
#[derive(Debug, Clone)]
pub struct Projected {
    /// How far the land stands, px a unit of height; 0 lies flat.
    pub stand: f64,
    pub picture: Canvas,
    /// For each pixel, row by row, the flat map's row it shows, or -1 past the map's far edge,
    /// where the band above it is the same open sea or ice.
    pub rows: Arc<[f32]>,
}

/// One sample of the drawing: its colour and the flat map's row it shows, kept as 32-bit
/// floats as the drawing's buffers hold them.
#[derive(Debug, Clone, Copy)]
struct Sample {
    colour: [f32; 3],
    row: f32,
}

impl Default for Sample {
    fn default() -> Self {
        Self {
            colour: [0.0; 3],
            row: -1.0,
        }
    }
}

impl Projected {
    /// The land standing `stand` px a unit of height, drawn at `fine` samples a pixel each way:
    /// 2 for a picture that stays, 1 for a frame of the land rising or settling.
    pub fn new(relief: &Relief, lit: &LitGround, stand: f64, fine: usize) -> Self {
        let fine = fine.max(1);
        let (width, height) = (WIDTH * fine, HEIGHT * fine);
        let layers = (stand > 0.0)
            .then_some(relief.cliff.as_ref())
            .flatten()
            .map(|cliff| (cliff, lightness(lit).blurred(3)));
        // Column by column, each column's samples kept together from the top.
        let mut samples = vec![Sample::default(); width * height];
        in_rows(&mut samples, height, |column, out| {
            let x = (column as f64 + 0.5) / fine as f64;
            draw_column(relief, lit, stand, fine, x, layers.as_ref(), out);
        });

        let mut rgba = vec![0_u8; WIDTH * HEIGHT * 4];
        let mut rows = vec![-1.0_f32; WIDTH * HEIGHT];
        let share = 1.0 / (fine * fine) as f64;
        let middle = fine / 2;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let mut sum = [0.0; 3];
                for j in 0..fine {
                    for i in 0..fine {
                        let sample = &samples[(x * fine + i) * height + y * fine + j];
                        for (total, channel) in sum.iter_mut().zip(sample.colour) {
                            *total += f64::from(channel);
                        }
                    }
                }
                let at = (y * WIDTH + x) * 4;
                for (out, total) in rgba[at..at + 3].iter_mut().zip(sum) {
                    *out = byte(total * share);
                }
                rgba[at + 3] = 255;
                rows[y * WIDTH + x] = samples[(x * fine + middle) * height + y * fine + middle].row;
            }
        }
        let picture = Canvas::from_rgba(WIDTH as u32, HEIGHT as u32, &rgba)
            .unwrap_or_else(|| Canvas::new(WIDTH as u32, HEIGHT as u32));
        Self {
            stand,
            picture,
            rows: rows.into(),
        }
    }

    /// The flat map's row a pixel shows, or `None` past the map's far edge.
    pub fn row_at(&self, x: f64, y: f64) -> Option<f64> {
        let column = (x as i64).clamp(0, WIDTH as i64 - 1) as usize;
        let row = (y as i64).clamp(0, HEIGHT as i64 - 1) as usize;
        let shown = self.rows[row * WIDTH + column];
        (shown >= 0.0).then_some(f64::from(shown))
    }

    /// What lies under the pointer where no mark answers: the ground behind each pixel.
    pub fn under(&self) -> Under {
        Under::Relief {
            plate: Plate::MAP,
            width: WIDTH,
            rows: Arc::clone(&self.rows),
        }
    }
}

/// The lit ground's lightness, the mean of its channels, for a plateau's layers.
fn lightness(lit: &LitGround) -> Grid {
    let mut grid = Grid::new(WIDTH, HEIGHT);
    for (slot, colour) in grid.values_mut().iter_mut().zip(&lit.colour) {
        *slot = ((f64::from(colour[0]) + f64::from(colour[1]) + f64::from(colour[2])) / 3.0) as f32;
    }
    grid
}

/// The lit ground's colour at a point of the map, read smoothly between pixels.
fn colour_at(lit: &LitGround, x: f64, y: f64) -> [f64; 3] {
    let fx = (x - 0.5).clamp(0.0, WIDTH as f64 - 1.001);
    let fy = (y - 0.5).clamp(0.0, HEIGHT as f64 - 1.001);
    let (i, j) = (fx as usize, fy as usize);
    let (dx, dy) = (fx - i as f64, fy - j as f64);
    let a = j * WIDTH + i;
    let (b, c, d) = (a + 1, a + WIDTH, a + WIDTH + 1);
    let at = |index: usize, ch: usize| f64::from(lit.colour[index][ch]);
    [0, 1, 2].map(|ch| {
        (at(a, ch) * (1.0 - dx) + at(b, ch) * dx) * (1.0 - dy)
            + (at(c, ch) * (1.0 - dx) + at(d, ch) * dx) * dy
    })
}

/// One column of samples, from the near edge to the far one. A place higher on the screen than
/// any nearer one opens the rows between, which take the colours between the two places.
fn draw_column(
    relief: &Relief,
    lit: &LitGround,
    stand: f64,
    fine: usize,
    x: f64,
    layers: Option<&(&Grid, Grid)>,
    out: &mut [Sample],
) {
    let height = out.len();
    let fine_f = fine as f64;
    let at = |grid: &Grid, x: f64, y: f64| grid.sample(x - 0.5, y - 0.5);
    let top = top_row();
    let mut filled = height;
    let mut prev_screen = height as f64;
    let (mut prev_y, mut prev_height, mut prev_cliff) = (HEIGHT as f64, 0.0, 0.0);
    let mut prev = colour_at(lit, x, HEIGHT as f64 - 0.01);
    for s in (0..HEIGHT * fine).rev() {
        let y = (s as f64 + 0.5) / fine_f;
        let h = if stand > 0.0 {
            at(&relief.standing, x, y)
        } else {
            0.0
        };
        let screen = (top + FORESHORTEN * y - stand * h) * fine_f;
        let cliff = layers.map_or(0.0, |(cliff, _)| at(cliff, x, y));
        let current = colour_at(lit, x, y);
        if screen < filled as f64 {
            let first = (screen - 0.5).ceil().max(0.0) as usize;
            for r in (first..filled).rev() {
                let t = if prev_screen - screen > 1e-6 {
                    ((prev_screen - (r as f64 + 0.5)) / (prev_screen - screen)).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let mut colour = [0, 1, 2].map(|ch| mix(prev[ch], current[ch], t));
                if let Some((_, lightness)) = layers {
                    let k = mix(prev_cliff, cliff, t);
                    // A plateau's layers, rust and buff by height, in the light that falls there.
                    if k > 0.01 {
                        let band =
                            smooth(0.2, 0.8, 0.5 + 0.5 * (mix(prev_height, h, t) * 1.5).sin());
                        let light = at(lightness, x, mix(prev_y, y, t));
                        let tones = [
                            mix(1.25, 1.32, band),
                            mix(0.78, 1.06, band),
                            mix(0.58, 0.82, band),
                        ];
                        colour = [0, 1, 2].map(|ch| mix(colour[ch], light * tones[ch], k));
                    }
                }
                out[r] = Sample {
                    colour: colour.map(|channel| channel as f32),
                    row: mix(prev_y, y, t) as f32,
                };
            }
            filled = first;
        }
        prev_screen = screen;
        prev_y = y;
        prev_height = h;
        prev_cliff = cliff;
        prev = current;
    }
    // Past the map's far edge, the pole: the top row carried on, the same open sea or ice.
    let far = colour_at(lit, x, 0.5).map(|channel| channel as f32);
    for sample in &mut out[..filled] {
        *sample = Sample {
            colour: far,
            row: -1.0,
        };
    }
}
