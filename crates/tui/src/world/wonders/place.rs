//! Where each wonder stands on the map: on the nearest free ground of its own kind to its
//! behavior, first within 110 px, then 240, then on kindred ground, then on any land, then
//! anywhere free. Free means it covers no dot with its rim and a little air, nothing nature drew
//! on the ground or in the sky, no shoal and no other wonder. On open sea, deeper water wins a
//! little, so a ship stands clear of the coast; Atlantis stands on the very place its land sank
//! from.

use std::f64::consts::PI;

use argonauts_core::behavior_world::{RisenWonder, Wonder};

use super::{Bounds, bounds, size_on_map};
use crate::world::map::fields::{Ground, HEIGHT, Look, WIDTH};
use crate::world::map::nature::Nature;
use crate::world::map::weather::FlatLayers;

/// The ground a wonder stands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terrain {
    /// Open water under all of it.
    Sea,
    /// Water with land close on two opposite sides.
    Strait,
    /// Land with the sea a few steps away.
    Coast,
    /// Rainforest.
    Lush,
    Grass,
    Desert,
    /// High ground.
    Peak,
    /// Any land well inside the coast.
    Any,
    /// Anywhere free at all.
    Free,
}

impl Terrain {
    /// The ground each wonder belongs on.
    pub fn of(wonder: Wonder) -> Self {
        match wonder {
            Wonder::Argo | Wonder::Atlantis => Terrain::Sea,
            Wonder::Symplegades => Terrain::Strait,
            Wonder::Talos | Wonder::Sirens | Wonder::Colossus | Wonder::Pharos => Terrain::Coast,
            Wonder::Fleece | Wonder::Dodona | Wonder::Gardens | Wonder::Artemis => Terrain::Lush,
            Wonder::Dragon | Wonder::Harpies | Wonder::Zeus => Terrain::Peak,
            Wonder::Pyramid | Wonder::Mausoleum => Terrain::Desert,
        }
    }

    /// Ground that will do when a wonder's own kind is not near its behavior, in order.
    pub fn kin(self) -> &'static [Terrain] {
        match self {
            Terrain::Strait => &[Terrain::Sea],
            Terrain::Sea => &[Terrain::Strait],
            Terrain::Coast | Terrain::Peak => &[Terrain::Any],
            Terrain::Lush | Terrain::Desert => &[Terrain::Grass, Terrain::Any],
            Terrain::Grass | Terrain::Any | Terrain::Free => &[],
        }
    }
}

/// A wonder placed on the map.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub wonder: Wonder,
    /// Its place in the list of risen wonders, which its line comes from.
    pub index: usize,
    /// The behavior it stands by.
    pub behavior: usize,
    /// Its ground point on the flat map.
    pub ground: (f64, f64),
    pub size: f64,
    /// The ground it stands on, its own kind or the kindred kind that would do.
    pub terrain: Terrain,
    /// How far it stands from its behavior.
    pub moved: f64,
    /// The box its picture fills on the flat map.
    pub bounds: Bounds,
}

impl Placed {
    /// The middle of its box on the flat map.
    pub fn middle(&self) -> (f64, f64) {
        (
            (self.bounds.x0 + self.bounds.x1) / 2.0,
            (self.bounds.y0 + self.bounds.y1) / 2.0,
        )
    }
}

/// Places the risen wonders on a crew's map, in the order they rose. A gas giant has none yet.
pub fn place(
    ground: &Ground,
    nature: &Nature,
    layers: &FlatLayers,
    risen: &[RisenWonder],
) -> Vec<Placed> {
    if ground.look == Look::Gas || risen.is_empty() {
        return Vec::new();
    }
    let busy = Busy::new(ground, nature, layers);
    let mut placed: Vec<Placed> = Vec::new();
    for (index, rose) in risen.iter().enumerate() {
        let behavior = ground
            .facts
            .behaviors
            .iter()
            .position(|days| days.name == rose.behavior)
            .or(ground.busiest)
            .unwrap_or(0);
        let wonder = rose.wonder;
        let size = size_on_map(wonder);
        let shape = bounds(wonder, size);
        let deep = wonder != Wonder::Atlantis;
        let Some((x, y, terrain, moved)) = spot(
            ground,
            &busy,
            Terrain::of(wonder),
            behavior,
            &placed,
            shape,
            deep,
        ) else {
            continue;
        };
        placed.push(Placed {
            wonder,
            index,
            behavior,
            ground: (x, y),
            size,
            terrain,
            moved,
            bounds: Bounds {
                x0: x + shape.x0,
                x1: x + shape.x1,
                y0: y + shape.y0,
                y1: y + shape.y1,
            },
        });
    }
    placed
}

/// What a wonder must not cover, as a table of sums over the map: every dot with its rim and a
/// little air, every mark of nature as drawn on the ground or in the sky, and every shoal.
struct Busy {
    sums: Vec<i32>,
}

impl Busy {
    fn new(ground: &Ground, nature: &Nature, layers: &FlatLayers) -> Self {
        let mut busy = vec![0_u8; WIDTH * HEIGHT];
        let (laid, sky) = (layers.ground.to_rgba(), layers.sky.to_rgba());
        for (at, slot) in busy.iter_mut().enumerate() {
            if laid[at * 4 + 3] > 40 || sky[at * 4 + 3] > 40 {
                *slot = 1;
            }
        }
        let mut disc = |x: f64, y: f64, r: f64| {
            let rows =
                (y - r).floor().max(0.0) as usize..=((y + r).ceil() as usize).min(HEIGHT - 1);
            for j in rows {
                let columns =
                    (x - r).floor().max(0.0) as usize..=((x + r).ceil() as usize).min(WIDTH - 1);
                for i in columns {
                    let (dx, dy) = (i as f64 - x, j as f64 - y);
                    if dx * dx + dy * dy <= r * r {
                        busy[j * WIDTH + i] = 1;
                    }
                }
            }
        };
        for &(x, y) in &ground.dots {
            disc(f64::from(x), f64::from(y), 8.0);
        }
        for shoal in &nature.shoals {
            disc(shoal.x, shoal.y, 14.0);
        }
        let stride = WIDTH + 1;
        let mut sums = vec![0_i32; stride * (HEIGHT + 1)];
        for y in 0..HEIGHT {
            let mut row = 0;
            for x in 0..WIDTH {
                row += i32::from(busy[y * WIDTH + x]);
                sums[(y + 1) * stride + x + 1] = sums[y * stride + x + 1] + row;
            }
        }
        Self { sums }
    }

    /// How many busy pixels lie in a box.
    fn within(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> i32 {
        let stride = WIDTH + 1;
        let column = |v: f64, f: fn(f64) -> f64| (f(v).max(0.0) as usize).min(WIDTH);
        let row = |v: f64, f: fn(f64) -> f64| (f(v).max(0.0) as usize).min(HEIGHT);
        let (x0, x1) = (column(x0, f64::floor), column(x1, f64::ceil));
        let (y0, y1) = (row(y0, f64::floor), row(y1, f64::ceil));
        self.sums[y1 * stride + x1] - self.sums[y0 * stride + x1] - self.sums[y1 * stride + x0]
            + self.sums[y0 * stride + x0]
    }
}

/// Whether the ground at (x, y) is of a kind, for a wonder whose box from that point is `shape`.
fn ground_is(ground: &Ground, kind: Terrain, x: f64, y: f64, shape: Bounds) -> bool {
    let level = ground.land_at(x, y);
    let climate = || ground.climate.roughened_at(x, y);
    let height = || ground.height_at(x, y);
    let middle = ((shape.x0 + shape.x1) / 2.0, (shape.y0 + shape.y1) / 2.0);
    let corners = [
        (shape.x0, shape.y0),
        (shape.x1, shape.y0),
        (shape.x0, shape.y1),
        (shape.x1, shape.y1),
        middle,
    ];
    let all_wet = || {
        corners
            .iter()
            .all(|(dx, dy)| ground.land_at(x + dx, y + dy) < 0.95)
    };
    // It stands inland, not on the beach.
    let foot_on_land = || {
        ground.land_at(x + shape.x0 * 0.8, y) >= 1.02
            && ground.land_at(x + shape.x1 * 0.8, y) >= 1.02
            && level >= 1.12
    };
    match kind {
        Terrain::Sea => level < 0.7 && all_wet(),
        Terrain::Strait => {
            if level >= 0.85 || !all_wet() {
                return false;
            }
            let hits = |dx: f64, dy: f64| {
                (12..=70)
                    .step_by(4)
                    .any(|d| ground.land_at(x + dx * f64::from(d), y + dy * f64::from(d)) >= 1.0)
            };
            [(1.0, 0.0), (0.0, 1.0), (0.71, 0.71), (0.71, -0.71)]
                .iter()
                .any(|&(dx, dy)| hits(dx, dy) && hits(-dx, -dy))
        }
        Terrain::Coast => {
            if level < 1.04 || height() >= 30.0 {
                return false;
            }
            (0..12).any(|k| {
                let a = f64::from(k) * PI / 6.0;
                [8.0, 13.0, 18.0]
                    .iter()
                    .any(|d| ground.land_at(x + a.cos() * d, y + a.sin() * d) < 0.95)
            })
        }
        Terrain::Lush => foot_on_land() && climate() > 2.62 && height() < 30.0,
        Terrain::Grass => foot_on_land() && climate() >= 0.6 && climate() < 2.42 && height() < 30.0,
        Terrain::Desert => foot_on_land() && climate() < 0.45 && height() < 30.0,
        Terrain::Peak => foot_on_land() && height() >= 12.0,
        Terrain::Free => true,
        Terrain::Any => foot_on_land(),
    }
}

/// The nearest free spot of a wonder's ground to its behavior, with the ground it found and how
/// far it lies from the behavior.
fn spot(
    ground: &Ground,
    busy: &Busy,
    kind: Terrain,
    behavior: usize,
    placed: &[Placed],
    shape: Bounds,
    deep: bool,
) -> Option<(f64, f64, Terrain, f64)> {
    let (ax, ay) = ground.centre[behavior];
    let (pad, edge) = (5.0, 20.0);
    let (width, height) = (WIDTH as f64, HEIGHT as f64);
    let free = |x: f64, y: f64| {
        let (x0, x1) = (x + shape.x0 - pad, x + shape.x1 + pad);
        let (y0, y1) = (y + shape.y0 - pad, y + shape.y1 + pad);
        if x0 < edge || y0 < edge || x1 > width - edge || y1 > height - edge {
            return false;
        }
        if busy.within(x0, y0, x1, y1) > 0 {
            return false;
        }
        placed.iter().all(|other| {
            let b = other.bounds;
            !(x0 < b.x1 + 12.0 && x1 > b.x0 - 12.0 && y0 < b.y1 + 12.0 && y1 > b.y0 - 12.0)
        })
    };
    let mut tries = vec![(kind, 110.0), (kind, 240.0)];
    tries.extend(kind.kin().iter().map(|kin| (*kin, 240.0)));
    tries.push((Terrain::Any, 1e9));
    tries.push((Terrain::Free, 1e9));
    for (kind, reach) in tries {
        let r = f64::min(reach, 1400.0);
        let mut best = None;
        let mut best_score = 1e18;
        let mut y = (ay - r).floor().max(6.0);
        while y <= (height - 6.0).min(ay + r) {
            let mut x = (ax - r).floor().max(6.0);
            while x <= (width - 6.0).min(ax + r) {
                let d = (x - ax).hypot(y - ay);
                if d <= reach {
                    let deeper = if kind == Terrain::Sea && deep {
                        40.0 * ground.land_at(x, y)
                    } else {
                        0.0
                    };
                    let score = d + deeper;
                    if score < best_score && ground_is(ground, kind, x, y, shape) && free(x, y) {
                        best = Some((x, y, kind, d));
                        best_score = score;
                    }
                }
                x += 3.0;
            }
            y += 3.0;
        }
        if best.is_some() {
            return best;
        }
    }
    None
}
