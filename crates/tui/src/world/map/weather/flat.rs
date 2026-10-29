//! Nature seen from straight above, as the planet's texture wraps it and as the wonders keep
//! clear of it: what lies on the ground, what glows on the night side, and the weather in the
//! sky, each a picture over the whole flat map. The planet draws its storms, volcanoes, fires,
//! tornadoes and tsunami moving on the globe itself, so its version leaves their still pictures
//! out.

use std::f64::consts::{PI, TAU};

use super::hurricane;
use crate::world::map::fields::{Ground, HEIGHT, Rgb, WIDTH, byte, mix_rgb};
use crate::world::map::nature::{Kind, Nature, Sunk, Wave, fire_front, lay, scars, through, wind};
use crate::world::raster::noise::{Rng, fbm3, mix, smooth};
use crate::world::raster::{
    Blend, Brush, Canvas, Cap, Colour, Gradient, Join, Path, Pen, Transform,
};

/// Nature from straight above, over the whole flat map.
#[derive(Debug, Clone)]
pub struct FlatLayers {
    /// What lies on the ground.
    pub ground: Canvas,
    /// What glows on the night side: lava, embers and lightning.
    pub glow: Canvas,
    /// The weather: storm clouds, hurricanes, smoke and tornadoes.
    pub sky: Canvas,
}

/// Which of nature's pictures a flat copy keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Everything, as the map shows it held still.
    All,
    /// All but what the planet draws moving: storms, hurricanes, volcanoes' smoke, fires'
    /// smoke, tornadoes' funnels and tsunamis.
    Planet,
}

impl FlatLayers {
    /// Nature on a crew's flat map; where a behavior's land sank under a city, its atoll gives
    /// way.
    pub fn new(ground: &Ground, nature: &Nature, keep: Keep, sunk: Option<Sunk>) -> Self {
        let moving = keep == Keep::Planet;
        let mut land = Canvas::new(WIDTH as u32, HEIGHT as u32);
        land.put_rgba(&scars(ground, nature));
        let mut glow = Canvas::new(WIDTH as u32, HEIGHT as u32);
        let mut sky = Canvas::new(WIDTH as u32, HEIGHT as u32);
        let mut nowhere = Canvas::new(1, 1);
        for feature in &nature.features {
            if matches!(feature.kind, Kind::Wildfire { burning: true, .. }) {
                let front = fire_front(ground, feature);
                let smoke = if moving { &mut nowhere } else { &mut sky };
                fire_front_flat(&mut land, &mut glow, smoke, &front);
            }
        }
        for feature in nature.features.iter().filter(|f| f.stream().is_some()) {
            lay(&mut land, ground, feature);
        }
        for feature in &nature.features {
            match &feature.kind {
                Kind::Volcano { smoking, .. } => {
                    let smoke = if moving { &mut nowhere } else { &mut sky };
                    volcano_flat(
                        &mut land,
                        &mut glow,
                        smoke,
                        (feature.x, feature.y),
                        feature.radius,
                        *smoking,
                        feature.behavior,
                    );
                }
                Kind::Tsunami(wave) if !moving => tsunami_flat(&mut land, ground, wave),
                Kind::Tornado { .. } => {
                    let funnel = if moving { &mut nowhere } else { &mut sky };
                    let track_from = ground.centre[feature.behavior];
                    let r = feature.radius - 4.0;
                    let at = (feature.x, feature.y);
                    tornado_from_above(&mut land, funnel, at, r, feature.behavior, track_from);
                }
                Kind::Atoll if sunk.is_some_and(|sunk| sunk.behavior == feature.behavior) => {}
                Kind::River(_) | Kind::Current(_) => {}
                _ => lay(&mut land, ground, feature),
            }
        }
        if !moving {
            for feature in &nature.features {
                match &feature.kind {
                    Kind::Thunderstorm { .. } => storm_flat(
                        &mut sky,
                        &mut glow,
                        (feature.x, feature.y),
                        feature.radius,
                        feature.behavior as f64,
                    ),
                    Kind::Hurricane(storm) => {
                        let cloud = hurricane::sprite(
                            feature.radius,
                            hurricane::seed(feature.behavior, storm),
                        );
                        hurricane_flat(&mut sky, &cloud, (feature.x, feature.y), storm.south);
                    }
                    _ => {}
                }
            }
        }
        Self {
            ground: land,
            glow,
            sky,
        }
    }
}

/// A burning front from above: a glowing line with its smoke blown downwind, brown near the
/// fire and grey as it spreads.
fn fire_front_flat(land: &mut Canvas, glow: &mut Canvas, sky: &mut Canvas, front: &[(f64, f64)]) {
    let path = through(front);
    let pen = |width: f64| Pen::new(width).cap(Cap::Round).join(Join::Round);
    let bed = Colour::rgba(160, 40, 20, 0.7);
    land.stroke(&path, &pen(5.0), &Brush::new(bed).glow(bed, 6.0));
    land.stroke(&path, &pen(2.2), &Brush::new(Colour::rgb(255, 140, 40)));
    land.stroke(
        &path,
        &pen(0.9),
        &Brush::new(Colour::rgba(255, 230, 150, 0.9)),
    );
    let hot = Colour::rgba(255, 120, 30, 1.0);
    glow.stroke(&path, &pen(4.0), &Brush::new(hot).glow(hot, 8.0));
    let (wx, wy) = wind();
    for k in 0..16_usize {
        let t = k as f64 / 15.0;
        let source = front[(k * 7) % front.len()];
        let along = 2.0 + t * 70.0;
        let (px, py) = (source.0 + wx * along, source.1 + wy * along - t * 8.0);
        let pr = 8.0 + 22.0 * t;
        let a = 0.6 * (1.0 - 0.6 * t);
        let shade = |from: f64, to: f64| mix(from, to, t).trunc() as u8;
        let puff = Gradient::radial(px, py, 0.0, px, py, pr)
            .stop(
                0.0,
                Colour::rgba(
                    shade(120.0, 190.0),
                    shade(100.0, 186.0),
                    shade(86.0, 182.0),
                    a,
                ),
            )
            .stop(1.0, Colour::rgba(160, 156, 150, 0.0));
        let mut round = Path::new();
        round.circle(px, py, pr);
        sky.fill(&round, &Brush::new(puff));
    }
}

/// A volcano from above: a cone shaded round by the light, gullied, its foot fading into the
/// land, its shade soft to the south-east; smoking, lava on its flanks, its crater and lava
/// glowing, and its plume drifting downwind.
#[allow(clippy::too_many_arguments)]
pub fn volcano_flat(
    land: &mut Canvas,
    glow: &mut Canvas,
    sky: &mut Canvas,
    (x, y): (f64, f64),
    r: f64,
    smoking: bool,
    behavior: usize,
) {
    let seed = behavior as f64;
    let (sx, sy) = (x + r * 0.3, y + r * 0.35);
    let shade = Gradient::radial(sx, sy, r * 0.3, sx, sy, r * 1.1)
        .stop(0.0, Colour::rgba(10, 10, 14, 0.28))
        .stop(1.0, Colour::rgba(10, 10, 14, 0.0));
    let mut patch = Path::new();
    patch.circle(sx, sy, r * 1.1);
    land.fill(&patch, &Brush::new(shade));
    let side = (r * 2.0 + 4.0).ceil();
    let half = side / 2.0;
    let mut cone = Canvas::new(side as u32, side as u32);
    let turn = Gradient::conic(-PI * 0.75, half, half)
        .stop(0.0, Colour::rgb(168, 148, 126))
        .stop(0.25, Colour::rgb(112, 96, 82))
        .stop(0.5, Colour::rgb(52, 44, 40))
        .stop(0.75, Colour::rgb(104, 90, 78))
        .stop(1.0, Colour::rgb(168, 148, 126));
    let mut disc = Path::new();
    disc.circle(half, half, r);
    cone.fill(&disc, &Brush::new(turn));
    // The foot fades into the land.
    let fade = Gradient::radial(half, half, r * 0.5, half, half, r)
        .stop(0.0, Colour::rgba(0, 0, 0, 1.0))
        .stop(1.0, Colour::rgba(0, 0, 0, 0.0));
    cone.fill_rect(
        0.0,
        0.0,
        side,
        side,
        &Brush::new(fade).blend(Blend::DestinationIn),
    );
    // Gullies down the flanks read it as a cone.
    let mut rng = Rng::new((seed * 13.0 + 9.0) as u32);
    for k in 0..14 {
        let a = f64::from(k) / 14.0 * TAU + rng.next_f64() * 0.3;
        let lit = (a + PI * 0.75).cos();
        let colour = if lit > 0.0 {
            Colour::rgba(30, 22, 20, 0.35)
        } else {
            Colour::rgba(230, 214, 196, 0.18)
        };
        let mut gully = Path::new();
        gully.move_to(half + a.cos() * r * 0.34, half + a.sin() * r * 0.34);
        gully.line_to(
            half + (a + 0.08).cos() * r * 0.95,
            half + (a + 0.08).sin() * r * 0.95,
        );
        cone.stroke(
            &gully,
            &Pen::new(0.8),
            &Brush::new(colour).blend(Blend::SourceAtop),
        );
    }
    land.draw(&cone, x - half, y - half, &Brush::new(Colour::WHITE));
    let crater = r * 0.3;
    let ring = |grow: f64| {
        let mut path = Path::new();
        path.ellipse(
            x,
            y,
            crater + grow,
            crater * 0.85 + grow,
            0.0,
            0.0,
            7.0,
            false,
        );
        path
    };
    land.stroke(
        &ring(1.2),
        &Pen::new(1.0),
        &Brush::new(Colour::rgba(214, 196, 176, 0.7)),
    );
    if !smoking {
        land.fill(&ring(0.0), &Brush::new(Colour::rgb(36, 30, 28)));
        land.stroke(
            &ring(0.0),
            &Pen::new(0.8),
            &Brush::new(Colour::rgba(190, 170, 150, 0.6)),
        );
        return;
    }
    let mut rng = Rng::new((seed * 31.0 + 1.0) as u32);
    for k in 0..3 {
        let a = 0.5 + f64::from(k) * 1.9 + rng.next_f64() * 0.6;
        let (ex, ey) = (x + a.cos() * r * 0.92, y + a.sin() * r * 0.92);
        let (mx, my) = (
            x + (a + 0.25).cos() * r * 0.55,
            y + (a + 0.25).sin() * r * 0.55,
        );
        let lava = Gradient::linear(x, y, ex, ey)
            .stop(0.0, Colour::rgba(255, 190, 80, 1.0))
            .stop(0.6, Colour::rgba(230, 80, 30, 0.9))
            .stop(1.0, Colour::rgba(120, 30, 20, 0.0));
        let mut flow = Path::new();
        flow.move_to(x + a.cos() * crater, y + a.sin() * crater);
        flow.quad_to(mx, my, ex, ey);
        land.stroke(
            &flow,
            &Pen::new(1.6).cap(Cap::Round),
            &Brush::new(lava.clone()),
        );
        glow.stroke(&flow, &Pen::new(2.4).cap(Cap::Round), &Brush::new(lava));
    }
    let mouth = Gradient::radial(x, y, 0.0, x, y, crater * 1.3)
        .stop(0.0, Colour::rgb(255, 226, 140))
        .stop(0.5, Colour::rgb(255, 110, 40))
        .stop(1.0, Colour::rgb(90, 24, 18));
    land.fill(&ring(0.0), &Brush::new(mouth));
    let warm = Gradient::radial(x, y, 0.0, x, y, r * 1.4)
        .stop(0.0, Colour::rgba(255, 150, 60, 1.0))
        .stop(1.0, Colour::rgba(255, 80, 20, 0.0));
    let mut halo = Path::new();
    halo.circle(x, y, r * 1.4);
    glow.fill(&halo, &Brush::new(warm));
    let (wx, wy) = wind();
    for k in 0..14 {
        let t = f64::from(k) / 13.0;
        let along = 3.0 + t * r * 3.4;
        let side = (rng.next_f64() - 0.5) * r * 0.5 * t;
        let (px, py) = (
            x + wx * along - wy * side,
            y + wy * along + wx * side - t * 4.0,
        );
        let pr = r * (0.22 + 0.62 * t) * (0.8 + 0.4 * rng.next_f64());
        let a = 0.62 * (1.0 - t * 0.75);
        let puff = Gradient::radial(px - pr * 0.25, py - pr * 0.3, 0.0, px, py, pr)
            .stop(0.0, Colour::rgba(222, 218, 214, a))
            .stop(0.6, Colour::rgba(150, 146, 144, a * 0.7))
            .stop(1.0, Colour::rgba(120, 118, 118, 0.0));
        let mut round = Path::new();
        round.circle(px, py, pr);
        sky.fill(&round, &Brush::new(puff));
    }
}

/// A tsunami from straight above: a long white breaking front across its run with a dark trough
/// behind it, and the sea drawn back pale ahead.
fn tsunami_flat(land: &mut Canvas, ground: &Ground, wave: &Wave) {
    let u = 0.72;
    let (x, y) = (
        mix(wave.from.0, wave.to.0, u),
        mix(wave.from.1, wave.to.1, u),
    );
    let (ux, uy) = wave.out;
    let (px, py) = (-uy, ux);
    let half = wave.size * 0.8;
    let shore = super::tsunami::Tsunami::new(ground, 0, wave);
    for (qx, qy) in shore.shore() {
        let mut pale = Path::new();
        pale.circle(qx, qy, 3.0);
        land.fill(&pale, &Brush::new(Colour::rgba(150, 176, 160, 0.4)));
    }
    let front = |land: &mut Canvas, back: f64, colour: Colour, width: f64| {
        let mut line = Path::new();
        line.move_to(x - px * half - ux * back, y - py * half - uy * back);
        line.quad_to(
            x - ux * (back + 6.0),
            y - uy * (back + 6.0),
            x + px * half - ux * back,
            y + py * half - uy * back,
        );
        land.stroke(&line, &Pen::new(width).cap(Cap::Round), &Brush::new(colour));
    };
    front(land, 4.0, Colour::rgba(4, 16, 30, 0.6), 6.0);
    front(land, 0.0, Colour::rgba(46, 118, 146, 0.95), 5.0);
    front(land, -2.5, Colour::rgba(246, 251, 253, 1.0), 2.6);
}

/// A tornado from above: the track it scoured and its shade on the ground, and in the sky the
/// dust at its foot, its grey funnel and the cloud it hangs from.
pub fn tornado_from_above(
    land: &mut Canvas,
    sky: &mut Canvas,
    (x, y): (f64, f64),
    r: f64,
    seed: usize,
    (lx, ly): (f64, f64),
) {
    let mut rng = Rng::new((seed * 17 + 5) as u32);
    let back = (ly - y).atan2(lx - x) + PI + (rng.next_f64() - 0.5) * 0.8;
    let length = 18.0 + r * 2.4;
    let foot = (x, y + r * 0.95);
    let top = y - r * 1.45;
    let lean = (rng.next_f64() - 0.5) * r * 0.5;
    let mut track = Path::new();
    track.move_to(foot.0, foot.1);
    for s in 1..=10 {
        let t = f64::from(s) / 10.0;
        let a = back + (t * 6.0).sin() * 0.25;
        track.line_to(foot.0 + a.cos() * length * t, foot.1 + a.sin() * length * t);
    }
    land.stroke(
        &track,
        &Pen::new(2.0).cap(Cap::Round),
        &Brush::new(Colour::rgba(96, 74, 54, 0.6)),
    );
    let shade = Gradient::linear(foot.0, foot.1, foot.0 + r * 1.6, foot.1 + r * 1.1)
        .stop(0.0, Colour::rgba(10, 10, 14, 0.4))
        .stop(1.0, Colour::rgba(10, 10, 14, 0.0));
    let mut cast = Path::new();
    cast.move_to(foot.0, foot.1);
    cast.line_to(foot.0 + r * 1.6, foot.1 + r * 1.1);
    land.stroke(&cast, &Pen::new(3.0).cap(Cap::Round), &Brush::new(shade));

    let dust = Gradient::radial(foot.0, foot.1, 0.0, foot.0, foot.1, r * 0.8)
        .stop(0.0, Colour::rgba(150, 126, 100, 0.7))
        .stop(1.0, Colour::rgba(150, 126, 100, 0.0));
    let mut cloud = Path::new();
    cloud.ellipse(foot.0, foot.1, r * 0.8, r * 0.45, 0.0, 0.0, 7.0, false);
    sky.fill(&cloud, &Brush::new(dust));
    let steps = 16;
    let centre = |t: f64| x + lean * (PI * t).sin();
    let width = |t: f64| mix(r * 0.62, 0.9, t.powf(0.7));
    let mut funnel = Path::new();
    for s in 0..=steps {
        let t = f64::from(s) / f64::from(steps);
        let cy = mix(top, foot.1, t);
        if s == 0 {
            funnel.move_to(centre(t) - width(t), cy);
        } else {
            funnel.line_to(centre(t) - width(t), cy);
        }
    }
    for s in (0..=steps).rev() {
        let t = f64::from(s) / f64::from(steps);
        funnel.line_to(centre(t) + width(t), mix(top, foot.1, t));
    }
    funnel.close();
    let body = Gradient::linear(x - r * 0.6, 0.0, x + r * 0.6, 0.0)
        .stop(0.0, Colour::rgba(214, 212, 206, 0.95))
        .stop(0.45, Colour::rgba(160, 156, 150, 0.95))
        .stop(1.0, Colour::rgba(84, 80, 78, 0.95));
    sky.fill(&funnel, &Brush::new(body));
    for k in 1..5 {
        let t = f64::from(k) / 5.0;
        let v0 = mix(
            top,
            foot.1,
            (t * f64::from(steps)).round() / f64::from(steps),
        );
        let (cx, hw) = (centre(t), mix(r * 0.62, 0.9, t.powf(0.7)));
        let mut band = Path::new();
        band.move_to(cx - hw, v0 - 1.0);
        band.quad_to(cx, v0 + 1.6, cx + hw, v0 - 1.0);
        sky.stroke(
            &band,
            &Pen::new(0.7),
            &Brush::new(Colour::rgba(60, 58, 58, 0.45)),
        );
    }
    let base = Gradient::radial(x - r * 0.2, top - r * 0.1, 0.0, x, top, r * 1.15)
        .stop(0.0, Colour::rgba(176, 174, 170, 0.95))
        .stop(0.7, Colour::rgba(110, 108, 106, 0.85))
        .stop(1.0, Colour::rgba(90, 88, 88, 0.0));
    let mut hanging = Path::new();
    hanging.ellipse(x, top, r * 1.15, r * 0.5, 0.0, 0.0, 7.0, false);
    sky.fill(&hanging, &Brush::new(base));
}

/// A thunderstorm's cloud from above, as a weather satellite sees one: a bright tower with a
/// crisp edge upwind, its anvil feathered out downwind.
pub fn storm_cloud_from_above(r: f64, seed: f64) -> Canvas {
    let side = (r * 4.4).ceil() as usize;
    let half = side as f64 / 2.0;
    let (wx, wy) = wind();
    let mut density = vec![0.0_f32; side * side];
    for j in 0..side {
        for i in 0..side {
            let (dx, dy) = ((i as f64 + 0.5 - half) / r, (j as f64 + 0.5 - half) / r);
            let (along, across) = (dx * wx + dy * wy, -dx * wy + dy * wx);
            let n = fbm3(dx * 2.6, dy * 2.6, seed, 5.0, 4);
            let streak = fbm3(along * 1.4, across * 6.0, seed + 3.0, 5.0, 3);
            let rr = (along / if along > 0.0 { 1.5 } else { 0.8 }).hypot(across / 0.85);
            let core = smooth(0.98, 0.86, rr + 0.2 * n);
            let plume = if along > 0.0 {
                smooth(1.25, 0.5, rr + 0.3 * n - 0.3 * streak) * 0.7
            } else {
                0.0
            };
            let top = (-((along + 0.25).hypot(across) / 0.32).powi(2)).exp();
            density[j * side + i] = (core.max(plume) + 0.6 * top * core).clamp(0.0, 1.6) as f32;
        }
    }
    let mut rgba = vec![0_u8; side * side * 4];
    for j in 1..side.saturating_sub(1) {
        for i in 1..side.saturating_sub(1) {
            let at = j * side + i;
            let v = f64::from(density[at]);
            if v <= 0.02 {
                continue;
            }
            let gx = f64::from(density[at + 1]) - f64::from(density[at - 1]);
            let gy = f64::from(density[at + side]) - f64::from(density[at - side]);
            // Light from the north-west: slopes facing it are brighter.
            let lit = (0.78 + 0.6 * (gx + gy) + 0.18 * v).clamp(0.35, 1.1);
            let colour: Rgb = mix_rgb(
                [138.0, 146.0, 162.0],
                [252.0, 253.0, 255.0],
                (lit * (0.55 + 0.45 * v.min(1.0))).clamp(0.0, 1.0),
            );
            let o = at * 4;
            rgba[o] = byte(colour[0]);
            rgba[o + 1] = byte(colour[1]);
            rgba[o + 2] = byte(colour[2]);
            rgba[o + 3] = byte(v.clamp(0.0, 1.0) * 0.95 * 255.0);
        }
    }
    Canvas::from_rgba(side as u32, side as u32, &rgba).unwrap_or_else(|| Canvas::new(1, 1))
}

/// A thunderstorm from above: its cloud over its shade, and one short flash, a glow in the tower
/// and a bolt out of its edge, kept clear and bright so "stopped here" reads at a glance.
fn storm_flat(sky: &mut Canvas, glow: &mut Canvas, (x, y): (f64, f64), r: f64, seed: f64) {
    let cloud = storm_cloud_from_above((r + 0.5).floor(), seed);
    let half = f64::from(cloud.width()) / 2.0;
    let shade = Brush::new(Colour::WHITE).alpha(0.3).brightness(0.0);
    sky.draw(&cloud, x - half + 5.0, y - half + 7.0, &shade);
    sky.draw(&cloud, x - half, y - half, &Brush::new(Colour::WHITE));
    let (wx, wy) = wind();
    let (fx, fy) = (x - wx * r * 0.15, y - wy * r * 0.15);
    let flash = Gradient::radial(fx, fy, 0.0, fx, fy, r * 0.5)
        .stop(0.0, Colour::rgba(214, 226, 255, 0.75))
        .stop(1.0, Colour::rgba(190, 210, 255, 0.0));
    let mut lit = Path::new();
    lit.circle(fx, fy, r * 0.5);
    sky.fill(&lit, &Brush::new(flash));
    let mut rng = Rng::new((seed * 19.0 + 7.0) as u32);
    let a = 1.2 + rng.next_f64() * 0.8;
    let (mut bx, mut by) = (x + a.cos() * r * 0.55, y + a.sin() * r * 0.55);
    let mut bolt = Path::new();
    bolt.move_to(bx, by);
    for _ in 0..4 {
        let turn = a + (rng.next_f64() - 0.5) * 1.3;
        bx += turn.cos() * r * 0.24;
        let turn_y = a + (rng.next_f64() - 0.5) * 1.3;
        by += turn_y.sin() * r * 0.24;
        bolt.line_to(bx, by);
    }
    let pen = |width: f64| Pen::new(width).join(Join::Miter);
    sky.stroke(
        &bolt,
        &pen(3.4),
        &Brush::new(Colour::rgba(120, 150, 255, 0.45)),
    );
    sky.stroke(
        &bolt,
        &pen(1.4),
        &Brush::new(Colour::rgba(250, 252, 255, 1.0)),
    );
    glow.stroke(
        &bolt,
        &pen(3.0),
        &Brush::new(Colour::rgba(190, 210, 255, 0.9)),
    );
    let halo = Gradient::radial(fx, fy, 0.0, fx, fy, r * 0.8)
        .stop(0.0, Colour::rgba(190, 210, 255, 0.8))
        .stop(1.0, Colour::rgba(190, 210, 255, 0.0));
    let mut warm = Path::new();
    warm.circle(fx, fy, r * 0.8);
    glow.fill(&warm, &Brush::new(halo));
}

/// A hurricane from above over its shade; the south's arms wind the other way.
fn hurricane_flat(sky: &mut Canvas, cloud: &Canvas, (x, y): (f64, f64), south: bool) {
    let mirror = if south { -1.0 } else { 1.0 };
    let (w, h) = (f64::from(cloud.width()), f64::from(cloud.height()));
    let shade = Transform::IDENTITY
        .translate(x + 6.0, y + 8.0)
        .scale(1.0, mirror);
    let brush = Brush::new(Colour::WHITE)
        .alpha(0.32)
        .brightness(0.0)
        .at(shade);
    sky.draw(cloud, -w / 2.0, -h / 2.0, &brush);
    let body = Transform::IDENTITY.translate(x, y).scale(1.0, mirror);
    sky.draw(
        cloud,
        -w / 2.0,
        -h / 2.0,
        &Brush::new(Colour::WHITE).at(body),
    );
}
