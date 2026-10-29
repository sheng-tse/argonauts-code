//! Growing the ground's fields from a crew's dots, in the order each needs the last.

use std::f64::consts::PI;

use super::plateau::{MESA_FOOT, MESA_TOP, plateaus};
use super::{
    COARSE, COARSE_HEIGHT, COARSE_WIDTH, FINE, FINE_HEIGHT, FINE_WIDTH, Ground, GroundFacts, LAND,
    Look, map_pixel,
};
use crate::world::map::climate::ClimateFields;
use crate::world::raster::Grid;
use crate::world::raster::noise::{fbm3, smooth, vnoise3};

/// A mountain's spread round each of its dots, in pixels.
const SPREAD: f64 = 16.0 * LAND;
/// How far out the coast reaches round a behavior, in pixels.
const COAST_REACH: f64 = 37.0 * LAND;
/// Fewer actions than this make a gas giant.
const YOUNG: usize = 40;
/// Idle this many days, a crew's world freezes.
const FROZEN_DAYS: u32 = 10;

const G: f64 = COARSE as f64;
const F: f64 = FINE as f64;

pub fn grow(facts: GroundFacts) -> Ground {
    let behaviors = facts.behaviors.len();
    let seed = f64::from(facts.seed);
    let dots: Vec<(f32, f32)> = facts
        .dots
        .iter()
        .map(|dot| {
            let (x, y) = map_pixel(dot.x, dot.y);
            (x as f32, y as f32)
        })
        .collect();
    let mut count = vec![0_u32; behaviors];
    let mut by_group = vec![vec![0_u32; facts.groups]; behaviors];
    let mut group_count = vec![0_u32; facts.groups];
    let mut sum = vec![(0.0_f64, 0.0_f64); behaviors];
    for (dot, &(x, y)) in facts.dots.iter().zip(&dots) {
        count[dot.behavior] += 1;
        if let Some(slot) = by_group[dot.behavior].get_mut(dot.group) {
            *slot += 1;
        }
        if let Some(slot) = group_count.get_mut(dot.group) {
            *slot += 1;
        }
        sum[dot.behavior].0 += f64::from(x);
        sum[dot.behavior].1 += f64::from(y);
    }
    let centre: Vec<(f64, f64)> = (0..behaviors)
        .map(|at| {
            if count[at] > 0 {
                let n = f64::from(count[at]);
                (sum[at].0 / n, sum[at].1 / n)
            } else {
                let (x, y) = facts.behaviors[at].place.unwrap_or((0.5, 0.5));
                map_pixel(x, y)
            }
        })
        .collect();

    let (mut mountains, mut peaks) = mountains(&facts, &dots, &count, &centre);
    let plateaus = plateaus(&facts, &count);
    let mesa = (!plateaus.is_empty()).then(|| {
        let mut mesa = Grid::new(COARSE_WIDTH, COARSE_HEIGHT);
        for plateau in &plateaus {
            let peak = f64::from(count[plateau.behavior]);
            let Some(own) = mountains[plateau.behavior].as_mut() else {
                continue;
            };
            for at in 0..own.values().len() {
                let v = f64::from(own.values()[at]);
                let up = smooth(MESA_FOOT, MESA_TOP, v / peak);
                let raised = v.max(peak * up);
                let all = &mut peaks.values_mut()[at];
                *all = (f64::from(*all) + (raised - v)) as f32;
                own.values_mut()[at] = raised as f32;
                if up > 0.01 {
                    let top = &mut mesa.values_mut()[at];
                    *top = top.max(up as f32);
                }
            }
        }
        mesa
    });

    let coast_pull = coast_pull(&count, &centre);
    let (warp_x, warp_y, height) = warped(&peaks, seed);
    let (land, relief) = land_and_relief(&peaks, &coast_pull, &height, seed);
    let (land_slope, log_height, log_height_dx, log_height_dy) = slopes(&land, &height);
    let fine_relief = fine_relief(&relief, &height, mesa.as_ref(), &warp_x, &warp_y, seed);
    let (fine_slope_x, fine_slope_y) = fine_slopes(&fine_relief);
    let flat_shade = flat_shade(&fine_relief, &fine_slope_x, &fine_slope_y);

    let land_points = land.values().iter().filter(|v| **v >= 1.0).count();
    let land_share = land_points as f64 / land.values().len() as f64;
    let look = if facts.dots.len() < YOUNG {
        Look::Gas
    } else if facts.idle_days >= FROZEN_DAYS {
        Look::Ice
    } else if land_share < 0.3 {
        Look::Ocean
    } else {
        Look::Rock
    };
    let busiest = count
        .iter()
        .enumerate()
        .rev()
        .max_by_key(|(_, count)| **count)
        .map(|(at, _)| at);
    let climate = ClimateFields::new(&facts, &count, &centre, &warp_x, &warp_y);

    Ground {
        facts,
        dots,
        count,
        by_group,
        group_count,
        centre,
        mountains,
        peaks,
        plateaus,
        mesa,
        coast_pull,
        warp_x,
        warp_y,
        height,
        land,
        land_slope,
        log_height,
        log_height_dx,
        log_height_dy,
        relief,
        fine_relief,
        fine_slope_x,
        fine_slope_y,
        flat_shade,
        land_share,
        look,
        busiest,
        climate,
    }
}

/// Each behavior's mountain and all of them together. A mountain's peak is as tall as its
/// count: each dot adds a soft bump, and the bumps are scaled so they sum to the count at the
/// middle of the behavior's dots.
fn mountains(
    facts: &GroundFacts,
    dots: &[(f32, f32)],
    count: &[u32],
    centre: &[(f64, f64)],
) -> (Vec<Option<Grid>>, Grid) {
    let spread = 2.0 * SPREAD * SPREAD;
    let reach = (4.0 * SPREAD / G).ceil();
    let mut raw = vec![0.0_f64; count.len()];
    for (dot, &(x, y)) in facts.dots.iter().zip(dots) {
        let (cx, cy) = centre[dot.behavior];
        let (dx, dy) = (f64::from(x) - cx, f64::from(y) - cy);
        raw[dot.behavior] += (-(dx * dx + dy * dy) / spread).exp();
    }
    let lift: Vec<f64> = (0..count.len())
        .map(|at| {
            if count[at] > 0 {
                f64::from(count[at]) / raw[at]
            } else {
                0.0
            }
        })
        .collect();
    let mut mountains: Vec<Option<Grid>> = vec![None; count.len()];
    let mut peaks = Grid::new(COARSE_WIDTH, COARSE_HEIGHT);
    let (last_x, last_y) = ((COARSE_WIDTH - 1) as f64, (COARSE_HEIGHT - 1) as f64);
    for (dot, &(x, y)) in facts.dots.iter().zip(dots) {
        let (x, y) = (f64::from(x), f64::from(y));
        let (gx, gy) = (x / G, y / G);
        let own =
            mountains[dot.behavior].get_or_insert_with(|| Grid::new(COARSE_WIDTH, COARSE_HEIGHT));
        let rows =
            (gy - reach).floor().max(0.0) as usize..=(gy + reach).ceil().min(last_y) as usize;
        for j in rows {
            let columns =
                (gx - reach).floor().max(0.0) as usize..=(gx + reach).ceil().min(last_x) as usize;
            for k in columns {
                let (dx, dy) = (k as f64 * G - x, j as f64 * G - y);
                let v = lift[dot.behavior] * (-(dx * dx + dy * dy) / spread).exp();
                let at = j * COARSE_WIDTH + k;
                add(&mut own.values_mut()[at], v);
                add(&mut peaks.values_mut()[at], v);
            }
        }
    }
    (mountains, peaks)
}

/// How strongly each coarse point is pulled into the land by the behaviors round it, each by
/// the square root of its count.
fn coast_pull(count: &[u32], centre: &[(f64, f64)]) -> Grid {
    let reach = 2.0 * COAST_REACH * COAST_REACH;
    let mut pull = Grid::new(COARSE_WIDTH, COARSE_HEIGHT);
    for (at, &(cx, cy)) in centre.iter().enumerate() {
        if count[at] == 0 {
            continue;
        }
        let weight = f64::from(count[at]).sqrt();
        for j in 0..COARSE_HEIGHT {
            for k in 0..COARSE_WIDTH {
                let (dx, dy) = (k as f64 * G - cx, j as f64 * G - cy);
                let v = weight * (-(dx * dx + dy * dy) / reach).exp();
                add(&mut pull.values_mut()[j * COARSE_WIDTH + k], v);
            }
        }
    }
    pull
}

/// The ground's warp and the heights read on the warped ground.
fn warped(peaks: &Grid, seed: f64) -> (Grid, Grid, Grid) {
    let mut warp_x = Grid::new(COARSE_WIDTH, COARSE_HEIGHT);
    let mut warp_y = Grid::new(COARSE_WIDTH, COARSE_HEIGHT);
    let mut height = Grid::new(COARSE_WIDTH, COARSE_HEIGHT);
    for j in 0..COARSE_HEIGHT {
        for k in 0..COARSE_WIDTH {
            let (nx, ny) = (k as f64 * G / (55.0 * LAND), j as f64 * G / (55.0 * LAND));
            let wx = LAND
                * (16.0 * fbm3(nx, ny, 7.1, seed + 21.0, 3)
                    + 6.0 * vnoise3(nx * 2.7, ny * 2.7, 1.3, seed + 23.0))
                / G;
            let wy = LAND
                * (16.0 * fbm3(nx, ny, 9.3, seed + 22.0, 3)
                    + 6.0 * vnoise3(nx * 2.7, ny * 2.7, 3.7, seed + 24.0))
                / G;
            warp_x.set(k, j, wx);
            warp_y.set(k, j, wy);
            height.set(
                k,
                j,
                peaks.sample(k as f64 + warp_x.get(k, j), j as f64 + warp_y.get(k, j)),
            );
        }
    }
    (warp_x, warp_y, height)
}

/// The land field, from the mountains and the coast's pull roughened by noise, and the relief.
fn land_and_relief(peaks: &Grid, pull: &Grid, height: &Grid, seed: f64) -> (Grid, Grid) {
    let mut land = Grid::new(COARSE_WIDTH, COARSE_HEIGHT);
    let mut relief = Grid::new(COARSE_WIDTH, COARSE_HEIGHT);
    for j in 0..COARSE_HEIGHT {
        for k in 0..COARSE_WIDTH {
            let (nx, ny) = (k as f64 * G / (90.0 * LAND), j as f64 * G / (90.0 * LAND));
            let coast = fbm3(nx, ny, 0.5, seed, 4);
            let pulled = pull.get(k, j);
            land.set(
                k,
                j,
                (peaks.get(k, j) / 0.45).max(pulled * (1.1 * coast).exp() / 1.9),
            );
            let micro = fbm3(nx * 3.1, ny * 3.1, 2.5, seed + 5.0, 3);
            relief.set(
                k,
                j,
                5.5 * (1.0 + height.get(k, j)).log2() + 2.5 * pulled.min(1.4) + micro,
            );
        }
    }
    (land, relief)
}

/// How fast the land field changes per pixel, and the heights' logarithm with its slopes.
fn slopes(land: &Grid, height: &Grid) -> (Grid, Grid, Grid, Grid) {
    let (w, h) = (COARSE_WIDTH, COARSE_HEIGHT);
    let log_height = Grid::from_fn(w, h, |k, j| height.get(k, j).max(1e-3).log10());
    let mut land_slope = Grid::new(w, h);
    let mut dx = Grid::new(w, h);
    let mut dy = Grid::new(w, h);
    for j in 0..h {
        for k in 0..w {
            let (left, right) = (k.saturating_sub(1), (k + 1).min(w - 1));
            let (up, down) = (j.saturating_sub(1), (j + 1).min(h - 1));
            let (across, along) = ((right - left) as f64 * G, (down - up) as f64 * G);
            land_slope.set(
                k,
                j,
                ((land.get(right, j) - land.get(left, j)) / across)
                    .hypot((land.get(k, down) - land.get(k, up)) / along),
            );
            dx.set(
                k,
                j,
                (log_height.get(right, j) - log_height.get(left, j)) / across,
            );
            dy.set(
                k,
                j,
                (log_height.get(k, down) - log_height.get(k, up)) / along,
            );
        }
    }
    (land_slope, log_height, dx, dy)
}

/// The relief on the fine grid. Mountains grow rugged with height: ridges and gullies start at
/// about 5 actions and are full by 30, where bare rock begins, so a busy behavior reads as a
/// massif and a quiet one as a smooth hill. The ridges are 16 and 8 px apart, coarse enough to
/// read at the terminal's own size. A plateau's top keeps almost none. They change the light
/// only, never where the land is.
fn fine_relief(
    relief: &Grid,
    height: &Grid,
    mesa: Option<&Grid>,
    warp_x: &Grid,
    warp_y: &Grid,
    seed: f64,
) -> Grid {
    let mut fine = Grid::from_fn(FINE_WIDTH, FINE_HEIGHT, |k, j| {
        relief.sample(k as f64 * F / G, j as f64 * F / G)
    });
    for j in 0..FINE_HEIGHT {
        for k in 0..FINE_WIDTH {
            let (x, y) = (k as f64 * F, j as f64 * F);
            let (gx, gy) = (x / G, y / G);
            let log = height.sample(gx, gy).max(1e-3).log10();
            let flat = mesa.map_or(1.0, |mesa| {
                let up = mesa.sample(gx + warp_x.sample(gx, gy), gy + warp_y.sample(gx, gy));
                1.0 - 0.92 * smooth(0.02, 0.15, up)
            });
            let amp = 5.0 * smooth(0.6, 1.5, log) * flat;
            if amp <= 0.01 {
                continue;
            }
            let wx = x / 16.0 + 0.7 * vnoise3(x / 40.0, y / 40.0, 1.7, seed + 31.0);
            let wy = y / 16.0 + 0.7 * vnoise3(x / 40.0, y / 40.0, 4.1, seed + 32.0);
            let r1 = 1.0 - vnoise3(wx, wy, 0.3, seed + 33.0).abs();
            let r2 = 1.0 - vnoise3(wx * 2.1, wy * 2.1, 0.9, seed + 34.0).abs();
            add(
                &mut fine.values_mut()[j * FINE_WIDTH + k],
                amp * (r1 * r1 + 0.45 * r2 * r2 - 0.75),
            );
        }
    }
    fine
}

fn fine_slopes(fine: &Grid) -> (Grid, Grid) {
    let (w, h) = (FINE_WIDTH, FINE_HEIGHT);
    let x = Grid::from_fn(w, h, |k, j| {
        (fine.get((k + 1).min(w - 1), j) - fine.get(k.saturating_sub(1), j)) / (2.0 * F)
    });
    let y = Grid::from_fn(w, h, |k, j| {
        (fine.get(k, (j + 1).min(h - 1)) - fine.get(k, j.saturating_sub(1))) / (2.0 * F)
    });
    (x, y)
}

/// The flat map's light: four lights round the north-west, each weighted by how obliquely it
/// meets the slope (as in Mark's multidirectional shading), half and half with the usual
/// north-west light, then darkened by how little open sky each place sees.
fn flat_shade(fine: &Grid, slope_x: &Grid, slope_y: &Grid) -> Grid {
    let azimuths = [225.0_f64, 270.0, 315.0, 360.0].map(|degrees| degrees * PI / 180.0);
    let lights = azimuths.map(|az| light(az, 35.0));
    let key = light(315.0 * PI / 180.0, 42.0);
    let near = fine.blurred(5);
    let far = fine.blurred(15);
    let dot = |n: [f64; 3], l: [f64; 3]| n[0] * l[0] + n[1] * l[1] + n[2] * l[2];
    Grid::from_fn(FINE_WIDTH, FINE_HEIGHT, |k, j| {
        let (zx, zy) = (slope_x.get(k, j), slope_y.get(k, j));
        let len = (zx * zx + zy * zy + 1.0).sqrt();
        let normal = [-zx / len, -zy / len, 1.0 / len];
        let aspect = (-zx).atan2(zy);
        let (mut num, mut den) = (0.0, 0.0);
        for (az, light) in azimuths.iter().zip(&lights) {
            let s = (aspect - az).sin();
            let weight = s * s + 0.02;
            num += weight * dot(normal, *light).max(0.0);
            den += weight;
        }
        let many = num / den / lights[0][2];
        let keyed = dot(normal, key).max(0.0) / key[2];
        let lit = 1.0 + 1.15 * (0.5 * many + 0.5 * keyed - 1.0);
        let here = fine.get(k, j);
        let open = (1.0 - 0.018 * (near.get(k, j) - here) - 0.009 * (far.get(k, j) - here))
            .clamp(0.6, 1.1);
        lit.clamp(0.4, 1.42) * open
    })
}

/// A light from `azimuth` radians round from north and `altitude` degrees up, as a unit
/// vector with x east, y south and z up.
fn light(azimuth: f64, altitude: f64) -> [f64; 3] {
    let alt = altitude * PI / 180.0;
    [
        azimuth.sin() * alt.cos(),
        -azimuth.cos() * alt.cos(),
        alt.sin(),
    ]
}

/// Adds to a stored value as a field of 32-bit floats does, rounding after each add.
fn add(slot: &mut f32, v: f64) {
    *slot = (f64::from(*slot) + v) as f32;
}
