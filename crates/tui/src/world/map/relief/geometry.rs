//! The relief's shape, worked out once per crew: how far inside the coast each pixel is, the
//! height that sets where it stands on the screen, the finer height the light sees, the
//! shadows the low sun casts, and a plateau's cliffs.

use super::{FORESHORTEN, sun, top_row};
use crate::world::map::fields::{COARSE, Ground, HEIGHT, WIDTH, fine as read_fine, in_rows};
use crate::world::map::land::coast_at;
use crate::world::raster::Grid;
use crate::world::raster::noise::smooth;

/// The crew's relief, one value a pixel of the map in each field.
#[derive(Debug, Clone)]
pub struct Relief {
    /// How far inside the coast each pixel lies, negative at sea.
    pub coast: Grid,
    /// The height that sets where a place stands on the screen: the smooth relief, brought
    /// down to the sea within 16 px of the coast.
    pub standing: Grid,
    /// The height the light sees, with the ridges of busy mountains.
    pub lit_height: Grid,
    /// How deep each place lies in the shade a mountain casts, 0 to 1.
    pub shade: Grid,
    /// The lit height blurred near and far, for how much open sky each place sees.
    pub near: Grid,
    pub far: Grid,
    /// How much of each pixel is sea.
    pub sea: Grid,
    /// How much each pixel is a plateau's cliff, for its layers; `None` without a plateau.
    pub cliff: Option<Grid>,
}

impl Relief {
    pub fn new(ground: &Ground) -> Self {
        let g = COARSE as f64;
        let smoothed = ground.relief.blurred(1);
        let mut columns = vec![[0.0_f32; 3]; WIDTH * HEIGHT];
        in_rows(&mut columns, WIDTH, |y, row| {
            for (x, out) in row.iter_mut().enumerate() {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let coast = coast_at(ground, px, py);
                let up = smooth(-0.5, 16.0, coast);
                *out = [
                    coast as f32,
                    (up * smoothed.sample(px / g, py / g)) as f32,
                    (up * read_fine(&ground.fine_relief, px, py)) as f32,
                ];
            }
        });
        let field = |channel: usize| {
            let mut grid = Grid::new(WIDTH, HEIGHT);
            for (slot, values) in grid.values_mut().iter_mut().zip(&columns) {
                *slot = values[channel];
            }
            grid
        };
        let coast = field(0);
        let standing = field(1);
        let lit_height = field(2);
        let cliff = ground.mesa.as_ref().map(|_| {
            Grid::from_fn(WIDTH, HEIGHT, |x, y| {
                let m = ground.mesa_at((x as f64 + 0.5) / g, (y as f64 + 0.5) / g);
                if m > 0.02 {
                    smooth(0.03, 0.16, m) * (1.0 - smooth(0.82, 0.97, m))
                } else {
                    0.0
                }
            })
        });
        let swept = sweep(&lit_height);
        let shade = Grid::from_fn(WIDTH, HEIGHT, |x, y| {
            smooth(0.4, 3.0, swept.get(x, y) - lit_height.get(x, y))
        });
        let sea = Grid::from_fn(WIDTH, HEIGHT, |x, y| {
            1.0 - (coast.get(x, y) + 0.5).clamp(0.0, 1.0)
        });
        Self {
            near: lit_height.blurred(6),
            far: lit_height.blurred(20),
            coast,
            standing,
            lit_height,
            shade,
            sea,
            cliff,
        }
    }

    /// Where a place of the flat map lands on the screen, `alt` pixels above the ground, with the
    /// land standing `stand` px a unit of height (0 lies it flat).
    pub fn screen(&self, x: f64, y: f64, stand: f64, alt: f64) -> (f64, f64) {
        (
            x,
            top_row() + FORESHORTEN * y - stand * (self.standing_at(x, y) + alt),
        )
    }

    /// The standing height at a point of the map, read smoothly between pixels.
    pub fn standing_at(&self, x: f64, y: f64) -> f64 {
        self.standing.sample(x - 0.5, y - 0.5)
    }
}

/// The highest ray of sunlight over each place, carried along the sunlight in one pass the way a
/// horizon map is made, falling as it goes.
fn sweep(height: &Grid) -> Grid {
    let sun = sun();
    let k = sun[1] / sun[0];
    let drop = sun[2] / sun[0].hypot(sun[1]) * 1.0_f64.hypot(k);
    let mut swept = Grid::new(WIDTH, HEIGHT);
    for y in 0..HEIGHT {
        swept.set(0, y, height.get(0, y));
    }
    let last = HEIGHT - 1;
    for x in 1..WIDTH {
        for y in 0..HEIGHT {
            let along = y as f64 - k;
            let j = along.floor();
            let prev = if j >= 0.0 {
                let f = along - j;
                let j = j as usize;
                swept.get(x - 1, j) * (1.0 - f) + swept.get(x - 1, (j + 1).min(last)) * f
            } else {
                -1e9
            };
            let here = height.get(x, y);
            swept.set(
                x,
                y,
                if here > prev - drop {
                    here
                } else {
                    prev - drop
                },
            );
        }
    }
    swept
}
