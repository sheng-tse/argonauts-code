//! Placing marks by their behaviors: each is set on ground of the kind it needs, near its
//! behavior, clear of the dots and of the marks placed before it. What lies flat (a lake, a
//! basin's floor, an oasis) is set on its own behavior's land, where the stood-up map doesn't
//! stretch it.

use crate::world::map::fields::{COARSE, COARSE_HEIGHT, COARSE_WIDTH, Ground, HEIGHT, WIDTH};
use crate::world::map::land::coast_at;
use crate::world::map::relief::{FORESHORTEN, STANDING};
use crate::world::raster::noise::smooth;

/// Pixels a side of the cells the dots are counted in.
const DOT_CELL: usize = 8;

/// The ground a mark needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// Land well inside the coast.
    Land,
    /// Open sea.
    Sea,
    /// Low, flat land; where none is near, the flattest there is, then any land.
    Low,
    /// Land under 14 actions high.
    Lowish,
}

/// A mark already placed: where, how far it reaches, and whether it lies flat on the ground.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub flat: bool,
}

/// What marks are placed among: the dots counted in cells, the landmasses, and the marks placed
/// so far.
#[derive(Debug, Clone)]
pub struct Placer {
    dot_cells: Vec<u16>,
    cells_across: usize,
    cells_down: usize,
    /// Which landmass each coarse point is on, 0 at sea.
    landmass: Vec<u32>,
    /// Each behavior's own landmass: the one under most of its dots.
    home: Vec<u32>,
    pub placed: Vec<Placed>,
}

impl Placer {
    pub fn new(ground: &Ground) -> Self {
        let cells_across = WIDTH.div_ceil(DOT_CELL);
        let cells_down = HEIGHT.div_ceil(DOT_CELL);
        let mut dot_cells = vec![0_u16; cells_across * cells_down];
        for &(x, y) in &ground.dots {
            let column = ((x / DOT_CELL as f32) as usize).min(cells_across - 1);
            let row = ((y / DOT_CELL as f32) as usize).min(cells_down - 1);
            let cell = &mut dot_cells[row * cells_across + column];
            *cell = cell.wrapping_add(1);
        }
        let landmass = landmasses(ground);
        let mut placer = Self {
            dot_cells,
            cells_across,
            cells_down,
            landmass,
            home: Vec::new(),
            placed: Vec::new(),
        };
        placer.home = (0..ground.behaviors())
            .map(|behavior| placer.home_of(ground, behavior))
            .collect();
        placer
    }

    pub fn add(&mut self, placed: Placed) {
        self.placed.push(placed);
    }

    /// How many dots lie in the cells within `radius` of a pixel.
    pub fn dots_near(&self, x: f64, y: f64, radius: f64) -> u32 {
        let cell = DOT_CELL as f64;
        let first = |v: f64| ((v - radius) / cell).trunc().max(0.0) as usize;
        let last =
            |v: f64, cells: usize| (((v + radius) / cell).trunc().max(0.0) as usize).min(cells - 1);
        let mut sum = 0;
        for row in first(y)..=last(y, self.cells_down) {
            for column in first(x)..=last(x, self.cells_across) {
                sum += u32::from(self.dot_cells[row * self.cells_across + column]);
            }
        }
        sum
    }

    /// The landmass under a pixel, 0 at sea.
    pub fn landmass_at(&self, x: f64, y: f64) -> u32 {
        let g = COARSE as f64;
        let index = |v: f64, size: usize| (round(v / g).max(0.0) as usize).min(size - 1);
        self.landmass[index(y, COARSE_HEIGHT) * COARSE_WIDTH + index(x, COARSE_WIDTH)]
    }

    /// A behavior's own landmass, the one under most of its dots; 0 when none is.
    pub fn home(&self, behavior: usize) -> u32 {
        self.home.get(behavior).copied().unwrap_or(0)
    }

    fn home_of(&self, ground: &Ground, behavior: usize) -> u32 {
        let mut tally: Vec<(u32, u32)> = Vec::new();
        for (dot, &(x, y)) in ground.facts.dots.iter().zip(&ground.dots) {
            if dot.behavior != behavior {
                continue;
            }
            let mass = self.landmass_at(f64::from(x), f64::from(y));
            if mass == 0 {
                continue;
            }
            match tally.iter_mut().find(|(m, _)| *m == mass) {
                Some((_, n)) => *n += 1,
                None => tally.push((mass, 1)),
            }
        }
        let mut best = (0, 0);
        for (mass, n) in tally {
            if n > best.1 {
                best = (mass, n);
            }
        }
        best.0
    }

    /// The best spot for a mark of `radius` px by a behavior: rings of 24 spots further and
    /// further out, scored by being near, clear of dots and earlier marks, on the ground wanted,
    /// and by `bias`. A spot `bias` scores endlessly low is never chosen.
    #[allow(clippy::too_many_arguments)]
    pub fn spot(
        &self,
        ground: &Ground,
        behavior: usize,
        radius: f64,
        want: Want,
        far: f64,
        keep: f64,
        bias: &dyn Fn(f64, f64) -> f64,
    ) -> Option<(f64, f64)> {
        let (cx, cy) = ground.centre[behavior];
        let mut best = None;
        let mut best_score = -1e9;
        for out in [20.0, 32.0, 48.0, 68.0, 92.0, 120.0] {
            let distance = (radius + out) * far;
            for k in 0..24 {
                let angle = f64::from(k) * std::f64::consts::PI / 12.0 + 0.13;
                let (x, y) = (cx + angle.cos() * distance, cy + angle.sin() * distance);
                if x < keep + 6.0
                    || y < keep + 6.0
                    || x > WIDTH as f64 - keep - 6.0
                    || y > HEIGHT as f64 - keep - 6.0
                {
                    continue;
                }
                let level = ground.land_at(x, y);
                if want == Want::Land {
                    let at_edge = (0..8).any(|q| {
                        let a = f64::from(q) * 0.785;
                        ground.land_at(x + a.cos() * radius, y + a.sin() * radius) < 1.02
                    });
                    if level < 1.15 || at_edge {
                        continue;
                    }
                }
                if want == Want::Sea && level > 0.6 {
                    continue;
                }
                let mut score =
                    -distance * 0.03 - f64::from(self.dots_near(x, y, radius + 6.0)) * 2.2;
                if want == Want::Low || want == Want::Lowish {
                    let slope = slope_at(ground, x, y)
                        .max(slope_at(ground, x, y + radius * 0.6))
                        .max(slope_at(ground, x, y - radius * 0.6));
                    let height = ground.height_at(x, y);
                    let steep = if want == Want::Low {
                        slope > 0.3 || height > 6.0
                    } else {
                        height > 14.0
                    };
                    if level < 1.2 || steep {
                        continue;
                    }
                    score -= (1.0 + height).log10() * 6.0 + 70.0 * slope;
                    // What lies flat lies wholly on the land, not only at its middle.
                    score -= 6.0 * f64::from(wet_rim(ground, x, y, radius));
                }
                score += bias(x, y);
                for placed in self.placed.iter().filter(|p| p.radius != 0.0) {
                    let gap = (placed.x - x).hypot(placed.y - y) - placed.radius - radius;
                    if gap < 10.0 {
                        score -= (10.0 - gap) * 4.0;
                    }
                }
                if score > best_score {
                    best_score = score;
                    best = Some((x, y));
                }
            }
        }
        best.or_else(|| match want {
            Want::Low => self.spot(ground, behavior, radius, Want::Lowish, far, keep, bias),
            Want::Lowish => self.spot(ground, behavior, radius, Want::Land, far, keep, bias),
            Want::Land | Want::Sea => None,
        })
    }

    /// A spot for something that lies flat by a behavior: on its own landmass, where the
    /// stood-up map stretches it by a fifth or less, never on another flat mark; near first,
    /// then further out, then allowing a little more stretch.
    pub fn flat_spot(
        &self,
        ground: &Ground,
        behavior: usize,
        radius: f64,
        far: f64,
        extra: Option<&dyn Fn(f64, f64) -> f64>,
    ) -> Option<(f64, f64)> {
        let home = self.home(behavior);
        let clear = |x: f64, y: f64| {
            self.placed
                .iter()
                .filter(|placed| placed.flat)
                .all(|placed| (placed.x - x).hypot(placed.y - y) - placed.radius - radius >= 4.0)
        };
        for (tolerance, reach) in [
            (0.2, far * 0.75),
            (0.2, far),
            (0.2, far * 1.5),
            (0.3, far * 1.5),
        ] {
            let bias = |x: f64, y: f64| {
                let stretched = (stretch_at(ground, x, y, radius) - 1.0).abs() > tolerance;
                let elsewhere = home != 0 && self.landmass_at(x, y) != home;
                if stretched || elsewhere || !clear(x, y) {
                    f64::NEG_INFINITY
                } else {
                    extra.map_or(0.0, |extra| extra(x, y))
                }
            };
            if let Some(spot) = self.spot(ground, behavior, radius, Want::Low, reach, radius, &bias)
            {
                return Some(spot);
            }
        }
        None
    }
}

/// How steep the ground is at a pixel, in units of height a pixel.
pub fn slope_at(ground: &Ground, x: f64, y: f64) -> f64 {
    let g = COARSE as f64;
    let relief = &ground.relief;
    let across = relief.sample((x + 4.0) / g, y / g) - relief.sample((x - 4.0) / g, y / g);
    let down = relief.sample(x / g, (y + 4.0) / g) - relief.sample(x / g, (y - 4.0) / g);
    across.hypot(down) / 8.0
}

/// How many of 8 points round a spot's rim lie at sea.
pub fn wet_rim(ground: &Ground, x: f64, y: f64, radius: f64) -> u32 {
    (0..8)
        .filter(|q| {
            let a = f64::from(*q) * 0.785;
            ground.land_at(x + a.cos() * radius * 0.85, y + a.sin() * radius * 0.85) < 1.02
        })
        .count() as u32
}

/// How much the stood-up map stretches a disc of `radius` down the screen, 1 where it doesn't:
/// ground rising away from the viewer is drawn taller, ground falling away shorter.
pub fn stretch_at(ground: &Ground, x: f64, y: f64, radius: f64) -> f64 {
    let g = COARSE as f64;
    let standing = |x: f64, y: f64| {
        smooth(-0.5, 16.0, coast_at(ground, x, y)) * ground.relief.sample(x / g, y / g)
    };
    1.0 - (STANDING / FORESHORTEN) * (standing(x, y + radius) - standing(x, y - radius))
        / (2.0 * radius)
}

/// Rounds half up, as the landmass lookup rounds a point to its nearest grid point.
fn round(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// Each coarse point's landmass, numbered in the order a scan from the top left first meets
/// them, joined across and down.
fn landmasses(ground: &Ground) -> Vec<u32> {
    let (w, h) = (COARSE_WIDTH, COARSE_HEIGHT);
    let land = ground.land.values();
    let mut mass = vec![0_u32; w * h];
    let mut next = 0;
    let mut stack = Vec::new();
    for start in 0..mass.len() {
        if mass[start] != 0 || land[start] < 1.0 {
            continue;
        }
        next += 1;
        mass[start] = next;
        stack.push(start);
        while let Some(at) = stack.pop() {
            let (i, j) = (at % w, at / w);
            let neighbours = [
                (i + 1 < w).then(|| at + 1),
                (i > 0).then(|| at - 1),
                (j + 1 < h).then(|| at + w),
                (j > 0).then(|| at - w),
            ];
            for q in neighbours.into_iter().flatten() {
                if mass[q] == 0 && land[q] >= 1.0 {
                    mass[q] = next;
                    stack.push(q);
                }
            }
        }
    }
    mass
}
