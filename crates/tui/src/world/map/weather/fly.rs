//! Where storm clouds and hurricanes fly on the stood-up map. They fly above the dots, so each
//! is moved, once, to the nearest spot within 150 px of where nature placed it at which its
//! drawn pixels cover no dot, no wonder, nothing lying flat on the ground and no other cloud;
//! weather keeps off the polar ice, and a hurricane keeps to its own hemisphere, off the
//! equator and over the sea where it can.

use std::f64::consts::TAU;

use super::Weather;
use crate::base::draw::geo::Plate;
use crate::world::map::fields::{Ground, HEIGHT, WIDTH};
use crate::world::map::land::Polar;
use crate::world::map::nature::EQUATOR_GAP;
use crate::world::raster::Canvas;

/// A box on the screen something already holds, and what covering it costs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Taken {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub cost: f64,
}

/// A cloud to fly: its picture, the point of it that stands over its place, how high it flies,
/// and for a hurricane, which hemisphere it keeps to.
pub struct Flier<'a> {
    pub picture: &'a Canvas,
    pub anchor: (f64, f64),
    pub altitude: f64,
    pub hurricane: Option<bool>,
}

/// Dots counted in buckets of the screen, for finding the few a cloud might cover.
pub struct DotBuckets {
    at: Vec<(f32, f32)>,
    buckets: Vec<Vec<usize>>,
    across: usize,
    down: usize,
}

const BUCKET: f64 = 24.0;

impl DotBuckets {
    pub fn new(at: &[(f32, f32)]) -> Self {
        let across = (WIDTH as f64 / BUCKET).ceil() as usize;
        let down = (HEIGHT as f64 / BUCKET).ceil() as usize;
        let mut buckets = vec![Vec::new(); across * down];
        for (i, &(x, y)) in at.iter().enumerate() {
            let column = ((f64::from(x) / BUCKET) as i64).clamp(0, across as i64 - 1) as usize;
            let row = ((f64::from(y) / BUCKET) as i64).clamp(0, down as i64 - 1) as usize;
            buckets[row * across + column].push(i);
        }
        Self {
            at: at.to_vec(),
            buckets,
            across,
            down,
        }
    }
}

/// The spot a cloud flies over, the nearest within 150 px of `place` that covers least, and
/// the box it then holds on the screen.
pub fn fly(
    weather: &Weather,
    ground: &Ground,
    polar: &Polar,
    flier: &Flier<'_>,
    place: (f64, f64),
    taken: &[Taken],
    dots: &DotBuckets,
) -> Option<((f64, f64), Taken)> {
    let (width, height) = (
        f64::from(flier.picture.width()),
        f64::from(flier.picture.height()),
    );
    let alpha: Vec<u8> = flier
        .picture
        .to_rgba()
        .chunks_exact(4)
        .map(|pixel| pixel[3])
        .collect();
    let on_ice = |x: f64, y: f64| polar.covers(Plate::MAP.place(x, y).lat);
    let middle = HEIGHT as f64 / 2.0;
    let mut best = None;
    let mut best_cost = 1e18;
    let mut r: f64 = 0.0;
    while r <= 150.0 {
        let n = ((r / 5.0 + 0.5).floor() as usize).max(1);
        for k in 0..n {
            let angle = k as f64 / n as f64 * TAU;
            let (mx, my) = (place.0 + angle.cos() * r, place.1 + angle.sin() * r);
            let (sx, sy) = weather.standing_screen(mx, my, flier.altitude);
            let (x0, y0) = (sx - flier.anchor.0, sy - flier.anchor.1);
            let (x1, y1) = (x0 + width, y0 + height);
            if x0 < 4.0 || y0 < 4.0 || x1 > WIDTH as f64 - 4.0 || y1 > HEIGHT as f64 - 4.0 {
                continue;
            }
            if on_ice(mx, my + 18.0) || on_ice(mx, my - 18.0) {
                continue;
            }
            if let Some(south) = flier.hurricane
                && ((my - middle).abs() < EQUATOR_GAP || (my > middle) != south)
            {
                continue;
            }
            let at_sea = ground.on_sea(mx, my);
            let mut cost = r + if flier.hurricane.is_some() && !at_sea {
                60.0
            } else {
                0.0
            };
            for b in taken {
                if x0 < b.x1 + 6.0 && x1 > b.x0 - 6.0 && y0 < b.y1 + 6.0 && y1 > b.y0 - 6.0 {
                    cost += b.cost;
                }
            }
            if cost >= best_cost {
                continue;
            }
            let row_of = |v: f64| ((v / BUCKET).trunc().max(0.0) as usize).min(dots.down - 1);
            let column_of = |v: f64| ((v / BUCKET).trunc().max(0.0) as usize).min(dots.across - 1);
            let (first_row, last_row) = (row_of(y0 - 6.0), row_of(y1 + 6.0));
            let (first_column, last_column) = (column_of(x0 - 6.0), column_of(x1 + 6.0));
            let mut row = first_row;
            while row <= last_row && cost < best_cost {
                for column in first_column..=last_column {
                    for &i in &dots.buckets[row * dots.across + column] {
                        let (px, py) = (f64::from(dots.at[i].0), f64::from(dots.at[i].1));
                        for (du, dv) in
                            [(0.0, 0.0), (-5.0, 0.0), (5.0, 0.0), (0.0, -5.0), (0.0, 5.0)]
                        {
                            let u = (px + du - x0 + 0.5).floor();
                            let v = (py + dv - y0 + 0.5).floor();
                            if u >= 0.0 && v >= 0.0 && u < width && v < height {
                                let at = v as usize * width as usize + u as usize;
                                if alpha[at] > 60 {
                                    cost += 1000.0;
                                    break;
                                }
                            }
                        }
                    }
                }
                row += 1;
            }
            if cost < best_cost {
                best_cost = cost;
                best = Some((
                    (mx, my),
                    Taken {
                        x0,
                        y0,
                        x1,
                        y1,
                        cost: 4000.0,
                    },
                ));
            }
        }
        r += 6.0;
    }
    best
}
