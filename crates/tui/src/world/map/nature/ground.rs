//! What nature lays on the ground, drawn into the ground's own picture so the light falls on it:
//! the skirts of floods and burn scars, a burning front's embers, rivers and currents, lakes,
//! fault lines, atolls, the tracks tornadoes scoured and the land's own kinds; and under the
//! water, the shoals' sandbanks.

use std::f64::consts::PI;

use super::water::{CurrentRun, current_runs};
use super::{Feature, Kind, Nature, Shoal, Stream, wind};
use crate::world::map::fields::{COARSE, Ground, HEIGHT, Rgb, WIDTH, byte, mix_rgb};
use crate::world::map::land::{Marks, Sunk as LandSunk, UnderWater, blob};
use crate::world::raster::noise::{Rng, fbm3, mix, smooth, vnoise3};
use crate::world::raster::{Brush, Canvas, Colour, Gradient, Path, Pen, Transform};

/// A behavior's land sunk under the sea, where a city rose in its place: a pale bank under the
/// water as wide as its mountain was last period, and its atoll gives way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sunk {
    pub behavior: usize,
    /// Its actions in the period before.
    pub before: u32,
}

/// The marks nature lays on a crew's ground, ready for the ground to draw in.
pub struct GroundMarks {
    /// Everything laid on the ground, straight-alpha RGBA over the whole map, scars included.
    ground: Option<Vec<u8>>,
    /// The floods' and burn scars' skirts alone.
    scar: Option<Vec<u8>>,
    under_water: Box<dyn Fn(f64, f64, Rgb) -> Rgb + Send + Sync>,
    /// The sunk land's bank under the water, where one sank.
    bank: Option<Box<BankAt>>,
    shown: bool,
}

/// How much of the sunk land's bank shows at a pixel, and its colour.
type BankAt = dyn Fn(f64, f64) -> Option<(f64, Rgb)> + Send + Sync;

/// The pale bank of sunk land under the water.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bank {
    x: f64,
    y: f64,
    spread: f64,
}

impl Bank {
    /// How much of the bank shows at a pixel, and its colour; `None` well clear of it.
    fn at(&self, x: f64, y: f64) -> Option<(f64, Rgb)> {
        let reach = 3.0 * self.spread;
        if x < (self.x - reach).trunc()
            || x >= self.x + reach
            || y < (self.y - reach).trunc()
            || y >= self.y + reach
        {
            return None;
        }
        let d2 = ((x - self.x).powi(2) + (y - self.y).powi(2)) / (2.0 * self.spread * self.spread);
        let k = smooth(1.6, 0.2, d2) * 0.9 * (0.8 + 0.2 * vnoise3(x / 9.0, y / 9.0, 3.0, 77.0));
        let colour = mix_rgb(SHALLOW, SURF, smooth(1.2, 0.0, d2) * 0.8);
        Some((k, colour))
    }
}

/// The map's shallows and surf, which the sunk bank is lit like.
const SHALLOW: Rgb = [20.0, 64.0, 88.0];
const SURF: Rgb = [34.0, 92.0, 108.0];

impl GroundMarks {
    /// Nature's marks on the ground when it shows; when it is hidden, as while comparing, only
    /// the shoals stay, which belong to the label list rather than to what happened. Where a
    /// behavior's land sank, its atoll gives way to the bank under the water.
    pub fn new(ground: &Ground, nature: &Nature, shown: bool, sunk: Option<Sunk>) -> Self {
        let shoals = nature.shoals.clone();
        let under_water = Box::new(move |x: f64, y: f64, sea: Rgb| {
            shoals
                .iter()
                .fold(sea, |sea, shoal| shoal_at(shoal, x, y, sea))
        });
        if !shown {
            return Self {
                ground: None,
                scar: None,
                under_water,
                bank: None,
                shown,
            };
        }
        let bank = sunk.map(|sunk| {
            let (x, y) = ground.centre[sunk.behavior];
            let bank = Bank {
                x,
                y,
                spread: 14.0 + 7.0 * (f64::from(sunk.before) / 30.0).sqrt(),
            };
            let at: Box<BankAt> = Box::new(move |x, y| bank.at(x, y));
            at
        });
        let scar = scars(ground, nature);
        let mut canvas = Canvas::new(WIDTH as u32, HEIGHT as u32);
        canvas.put_rgba(&scar);
        for feature in &nature.features {
            if matches!(feature.kind, Kind::Wildfire { burning: true, .. }) {
                ember_line(&mut canvas, &fire_front(ground, feature));
            }
        }
        for feature in nature.features.iter().filter(|f| f.stream().is_some()) {
            lay(&mut canvas, ground, feature);
        }
        for feature in &nature.features {
            match &feature.kind {
                Kind::Tornado { .. } => tornado_track(&mut canvas, ground, feature),
                Kind::River(_) | Kind::Current(_) => {}
                Kind::Atoll if sunk.is_some_and(|sunk| sunk.behavior == feature.behavior) => {}
                _ => lay(&mut canvas, ground, feature),
            }
        }
        Self {
            ground: Some(canvas.to_rgba()),
            scar: Some(scar),
            under_water,
            bank,
            shown,
        }
    }

    /// The marks as the ground's surface reads them.
    pub fn marks(&self) -> Marks<'_> {
        let under_water: &UnderWater = &*self.under_water;
        let sunk: Option<&LandSunk> = self.bank.as_deref().map(|bank| bank as &LandSunk);
        Marks {
            ground: self.ground.as_deref(),
            scar: self.scar.as_deref(),
            under_water: Some(under_water),
            sunk,
            icebergs: self.shown,
        }
    }

    /// Everything laid on the ground, scars included, as straight-alpha RGBA over the whole
    /// map; `None` while nature is hidden.
    pub fn laid(&self) -> Option<&[u8]> {
        self.ground.as_deref()
    }
}

/// Lays one of nature's marks that lies flat on the ground: water, a lake, a fault line, an atoll
/// or one of the land's own kinds. Anything else lays nothing here.
pub fn lay(canvas: &mut Canvas, ground: &Ground, feature: &Feature) {
    match &feature.kind {
        Kind::River(stream) => river(canvas, stream),
        Kind::Current(stream) => current_band(canvas, ground, feature, stream),
        Kind::Lake { .. } => lake(
            canvas,
            feature.x,
            feature.y,
            feature.radius,
            feature.behavior,
        ),
        Kind::Rift(rift) => fault(
            canvas,
            (feature.x, feature.y),
            rift.length,
            rift.angle,
            feature.behavior,
        ),
        Kind::Atoll => atoll(
            canvas,
            feature.x,
            feature.y,
            feature.radius,
            feature.behavior,
        ),
        Kind::Land(_) => {
            if let Some(mark) = feature.land_mark() {
                mark.draw(ground, canvas);
            }
        }
        _ => {}
    }
}

/// A path through points, unclosed.
pub fn through(points: &[(f64, f64)]) -> Path {
    let mut path = Path::new();
    for (i, &(x, y)) in points.iter().enumerate() {
        if i == 0 {
            path.move_to(x, y);
        } else {
            path.line_to(x, y);
        }
    }
    path
}

/// Floods and burn scars on the low skirt of their behavior's mountain: a flood on the side
/// facing the nearest sea, a scar downwind, each with a ragged noisy edge. A flood is muddy
/// water lit by streaks of sky; a scar grey ash with a rust edge where it scorched but did not
/// burn, so the relief still shows through it.
pub fn scars(ground: &Ground, nature: &Nature) -> Vec<u8> {
    let mut rgba = vec![0_u8; WIDTH * HEIGHT * 4];
    let g = COARSE as f64;
    let (wx, wy) = wind();
    for feature in &nature.features {
        let fire = match feature.kind {
            Kind::Flood { .. } => false,
            Kind::Wildfire { .. } => true,
            _ => continue,
        };
        let Some(own) = ground.mountains[feature.behavior].as_ref() else {
            continue;
        };
        let top = own.values().iter().copied().fold(0.0_f32, f32::max);
        if top <= 0.0 {
            continue;
        }
        let top = f64::from(top);
        let (px, py) = (feature.x, feature.y);
        let reach = 110.0;
        let seed = (feature.behavior * 13 + if fire { 5 } else { 1 }) as f64;
        let mut side = wy.atan2(wx);
        if !fire {
            let mut nearest = 1e9;
            for k in 0..24 {
                let a = f64::from(k) * PI / 12.0;
                let mut d = 10.0;
                while d < 200.0 {
                    if ground.land_at(px + a.cos() * d, py + a.sin() * d) < 1.0 {
                        if d < nearest {
                            nearest = d;
                            side = a;
                        }
                        break;
                    }
                    d += 6.0;
                }
            }
        }
        let rows =
            (py - reach).trunc().max(0.0) as usize..(py + reach).min(HEIGHT as f64).ceil() as usize;
        let columns =
            (px - reach).trunc().max(0.0) as usize..(px + reach).min(WIDTH as f64).ceil() as usize;
        for y in rows {
            if y as f64 >= (py + reach).min(HEIGHT as f64) {
                break;
            }
            for x in columns.clone() {
                let (xf, yf) = (x as f64, y as f64);
                if xf >= (px + reach).min(WIDTH as f64) {
                    break;
                }
                let (gx, gy) = (xf / g, yf / g);
                if ground.land.sample(gx, gy) < 1.03 {
                    continue;
                }
                let v = own.sample(gx, gy) / top;
                let skirt = smooth(0.03, 0.08, v) * (1.0 - smooth(0.3, 0.45, v));
                if skirt <= 0.01 {
                    continue;
                }
                let facing = ((yf - py).atan2(xf - px) - side).cos();
                let grain = fbm3(xf / 19.0, yf / 19.0, seed, 7.0, 4) + 0.12;
                let m = skirt * smooth(-0.01, 0.04, grain) * smooth(-0.25, 0.35, facing);
                if m <= 0.02 {
                    continue;
                }
                let at = (y * WIDTH + x) * 4;
                if fire {
                    let ash = smooth(0.1, 0.7, fbm3(xf / 3.5, yf / 3.5, seed + 9.0, 7.0, 2));
                    let edge = 1.0 - smooth(0.02, 0.09, grain);
                    let colour = mix_rgb([70.0, 62.0, 58.0], [138.0, 128.0, 118.0], ash * 0.6);
                    let colour = mix_rgb(colour, [150.0, 84.0, 44.0], edge * 0.8);
                    over(&mut rgba[at..at + 4], colour, (0.9 * m).min(0.88));
                } else {
                    let sheen = smooth(0.15, 0.6, fbm3(xf / 7.0, yf / 2.4, seed + 3.0, 7.0, 2));
                    let rim = 1.0 - smooth(0.0, 0.05, grain);
                    let colour = mix_rgb([38.0, 78.0, 104.0], [128.0, 168.0, 190.0], sheen * 0.55);
                    let colour = mix_rgb(colour, [120.0, 108.0, 80.0], rim * 0.6);
                    over(&mut rgba[at..at + 4], colour, (0.95 * m).min(1.0));
                }
            }
        }
    }
    rgba
}

/// Lays `colour` at strength `alpha` over a straight-alpha pixel.
fn over(pixel: &mut [u8], colour: Rgb, alpha: f64) {
    let under = f64::from(pixel[3]) / 255.0;
    let out = alpha + under * (1.0 - alpha);
    if out <= 0.0 {
        return;
    }
    for ch in 0..3 {
        pixel[ch] = byte((colour[ch] * alpha + f64::from(pixel[ch]) * under * (1.0 - alpha)) / out);
    }
    pixel[3] = byte(out * 255.0);
}

/// A burning front: where the scar meets the land downwind, a bowed line across the wind.
pub fn fire_front(ground: &Ground, feature: &Feature) -> Vec<(f64, f64)> {
    let (wx, wy) = wind();
    let (px, py) = (feature.x, feature.y);
    let top = ground.mountains[feature.behavior]
        .as_ref()
        .map_or(0.0, |own| {
            f64::from(own.values().iter().copied().fold(0.0_f32, f32::max))
        });
    let (mut fx, mut fy) = (px, py);
    let mut d = 0.0;
    while d < 120.0 {
        let (x, y) = (px + wx * d, py + wy * d);
        let own = ground.mountains[feature.behavior]
            .as_ref()
            .map_or(0.0, |own| own.sample(x / COARSE as f64, y / COARSE as f64));
        if top <= 0.0 || own / top < 0.06 || ground.land_at(x, y) < 1.1 {
            break;
        }
        (fx, fy) = (x, y);
        d += 2.0;
    }
    let (nx, ny) = (-wy, wx);
    (-16..=16)
        .map(|k| {
            let t = f64::from(k) / 16.0;
            let bow = 9.0 * (1.0 - t * t);
            let jitter = 2.4 * vnoise3(f64::from(k) * 0.35, feature.behavior as f64, 2.0, 23.0);
            (
                fx + nx * t * 28.0 + wx * (bow + jitter),
                fy + ny * t * 28.0 + wy * (bow + jitter),
            )
        })
        .collect()
}

/// A burning front's embers on the ground: a dark glowing bed, a red line and a bright core.
fn ember_line(canvas: &mut Canvas, front: &[(f64, f64)]) {
    let path = through(front);
    let pen = |width: f64| Pen::new(width).round();
    let bed = Colour::rgba(120, 30, 16, 0.55);
    canvas.stroke(&path, &pen(6.0), &Brush::new(bed).glow(bed, 6.0));
    canvas.stroke(
        &path,
        &pen(3.0),
        &Brush::new(Colour::rgba(200, 70, 24, 0.8)),
    );
    canvas.stroke(&path, &pen(1.2), &Brush::new(Colour::rgb(255, 170, 70)));
}

/// A river: the green strip it feeds, then the water, narrow at its source and wide at its
/// mouth, so it reads which way the agents moved.
pub fn river(canvas: &mut Canvas, stream: &Stream) {
    let path = &stream.path;
    canvas.stroke(
        &through(path),
        &Pen::new(stream.width + 6.0).round(),
        &Brush::new(Colour::rgba(28, 58, 34, 0.30)),
    );
    let last = (path.len() - 1).max(1) as f64;
    for i in 1..path.len() {
        let t = i as f64 / last;
        let width = mix(0.7, stream.width, t.powf(0.8));
        let segment = through(&path[i - 1..=i]);
        canvas.stroke(
            &segment,
            &Pen::new(width + 0.9).round(),
            &Brush::new(Colour::rgb(40, 86, 126)),
        );
        canvas.stroke(
            &segment,
            &Pen::new(width).round(),
            &Brush::new(Colour::rgb(78, 140, 180)),
        );
    }
}

/// An ocean current: a soft band of moving water only where it runs over the sea, as wide as the
/// move is common and narrowing to nothing at each coast, combed with faint streaks along the
/// flow so it reads as moving water even held still.
fn current_band(canvas: &mut Canvas, ground: &Ground, feature: &Feature, stream: &Stream) {
    let seed = (feature.behavior * 97 + stream.to * 13 + 5) as u32;
    current_runs_band(canvas, &current_runs(ground, stream), seed);
}

/// A current's band over its stretches at sea, its streaks drawn with `seed`.
pub fn current_runs_band(canvas: &mut Canvas, runs: &[CurrentRun], seed: u32) {
    for run in runs {
        let n = run.points.len();
        for (k, colour) in [
            (1.15, Colour::rgba(70, 160, 190, 0.10)),
            (0.85, Colour::rgba(76, 172, 198, 0.16)),
            (0.55, Colour::rgba(92, 186, 206, 0.2)),
        ] {
            let side = |i: usize| run.band * k * run.taper[i] / 2.0;
            let mut band = Path::new();
            for i in 0..n {
                let (h, (x, y), (nx, ny)) = (side(i), run.points[i], run.normals[i]);
                if i == 0 {
                    band.move_to(x + nx * h, y + ny * h);
                } else {
                    band.line_to(x + nx * h, y + ny * h);
                }
            }
            for i in (0..n).rev() {
                let (h, (x, y), (nx, ny)) = (side(i), run.points[i], run.normals[i]);
                band.line_to(x - nx * h, y - ny * h);
            }
            band.close();
            canvas.fill(&band, &Brush::new(colour));
        }
        let mut rng = Rng::new(seed);
        let mut s = 4.0;
        while s < run.total - 4.0 {
            let mut i = 1;
            while i < run.lengths.len() - 1 && run.lengths[i] < s {
                i += 1;
            }
            let span = run.lengths[i] - run.lengths[i - 1];
            let span = if span == 0.0 { 1.0 } else { span };
            let u = ((s - run.lengths[i - 1]) / span).clamp(0.0, 1.0);
            let (a, b) = (run.points[i - 1], run.points[i]);
            let taper = mix(run.taper[i - 1], run.taper[i], u);
            let lane = (rng.next_f64() - 0.5) * 0.9;
            let h = run.band * lane * taper;
            let (nx, ny) = run.normals[i];
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l = dx.hypot(dy);
            let l = if l == 0.0 { 1.0 } else { l };
            let length = 3.0 + 5.0 * rng.next_f64();
            let (x, y) = (mix(a.0, b.0, u) + nx * h, mix(a.1, b.1, u) + ny * h);
            let alpha = (0.22 * taper * (1.0 - lane.abs()) * 1000.0).round() / 1000.0;
            let streak = through(&[
                (x - dx / l * length / 2.0, y - dy / l * length / 2.0),
                (x + dx / l * length / 2.0, y + dy / l * length / 2.0),
            ]);
            canvas.stroke(
                &streak,
                &Pen::new(0.9).round(),
                &Brush::new(Colour::rgba(186, 232, 242, alpha)),
            );
            s += 3.2;
        }
    }
}

/// A lake: fresh water held in the land, with coves and points. A pale shore of sand and mud
/// all round with reeds along its south-west side, water lit by the sky and lightest to the
/// north where it mirrors it, the bank's shade along its west rim, two faint ripples and a glint.
pub fn lake(canvas: &mut Canvas, x: f64, y: f64, r: f64, behavior: usize) {
    let seed = behavior as f64;
    let shape = |radius: f64| blob(x, y, radius, seed, 0.4, 40, 2.1);
    canvas.fill(
        &shape(r + 2.8),
        &Brush::new(Colour::rgba(176, 162, 118, 0.95)),
    );
    canvas.with_clip(&shape(r + 2.6), Transform::IDENTITY, |canvas| {
        let mut reeds = Path::new();
        reeds.move_to(x, y);
        reeds.arc(x, y, r + 4.0, PI * 0.45, PI * 1.15, false);
        reeds.close();
        canvas.fill(&reeds, &Brush::new(Colour::rgba(80, 106, 54, 0.95)));
    });
    let water = Gradient::linear(x, y - r, x, y + r)
        .stop(0.0, Colour::rgb(112, 176, 206))
        .stop(1.0, Colour::rgb(56, 118, 162));
    canvas.fill(&shape(r), &Brush::new(water));
    canvas.with_clip(&shape(r), Transform::IDENTITY, |canvas| {
        let shade = Gradient::linear(x - r, y, x - r * 0.4, y)
            .stop(0.0, Colour::rgba(14, 34, 56, 0.55))
            .stop(1.0, Colour::rgba(14, 34, 56, 0.0));
        canvas.fill_rect(
            x - r - 3.0,
            y - r - 3.0,
            r * 2.0 + 6.0,
            r * 2.0 + 6.0,
            &Brush::new(shade),
        );
        for k in [0.25, 0.55] {
            let mut ripple = Path::new();
            ripple.ellipse(
                x + r * 0.1,
                y + r * (k - 0.2),
                r * 0.5,
                r * 0.12,
                0.0,
                0.2,
                PI - 0.2,
                false,
            );
            canvas.stroke(
                &ripple,
                &Pen::new(0.8),
                &Brush::new(Colour::rgba(200, 232, 244, 0.35)),
            );
        }
    });
    let mut glint = Path::new();
    glint.ellipse(
        x - r * 0.15,
        y - r * 0.42,
        r * 0.42,
        r * 0.11,
        -0.25,
        0.0,
        7.0,
        false,
    );
    canvas.fill(&glint, &Brush::new(Colour::rgba(246, 252, 255, 0.85)));
}

/// A fault line: a jagged scarp, its lit face a pixel north-west of its dark crack, with short
/// offsets along it.
pub fn fault(canvas: &mut Canvas, at: (f64, f64), length: f64, angle: f64, behavior: usize) {
    fault_through(canvas, at, length, angle, behavior, Transform::IDENTITY);
}

/// A fault line drawn through `place`, as a picture larger than the map draws it.
pub fn fault_through(
    canvas: &mut Canvas,
    (x, y): (f64, f64),
    length: f64,
    angle: f64,
    behavior: usize,
    place: Transform,
) {
    let mut rng = Rng::new((behavior * 7 + 3) as u32);
    let steps = (length / 4.0).ceil() as usize;
    let (mut px, mut py) = (
        x - angle.cos() * length / 2.0,
        y - angle.sin() * length / 2.0,
    );
    let mut points = Vec::with_capacity(steps + 1);
    for _ in 0..=steps {
        points.push((px, py));
        let a = angle + (rng.next_f64() - 0.5) * 1.1;
        px += a.cos() * 4.0;
        py += a.sin() * 4.0;
    }
    let lit: Vec<(f64, f64)> = points.iter().map(|&(u, v)| (u - 1.0, v - 1.0)).collect();
    canvas.stroke(
        &through(&lit),
        &Pen::new(1.2),
        &Brush::new(Colour::rgba(240, 226, 196, 0.7)).at(place),
    );
    let crack = Brush::new(Colour::rgba(28, 20, 18, 0.95)).at(place);
    canvas.stroke(&through(&points), &Pen::new(2.4), &crack);
    let (nx, ny) = (-angle.sin(), angle.cos());
    let mut s = 2;
    while s + 1 < points.len() {
        let (u, v) = points[s];
        canvas.stroke(
            &through(&[(u, v), (u + nx * 3.5, v + ny * 3.5)]),
            &Pen::new(1.2),
            &crack,
        );
        s += 3;
    }
}

/// An atoll: the ring of land left where a behavior's land sank. A bright ring of white sand with
/// green on its widest islets, broken by two passes, round a turquoise lagoon lighter than any
/// sea, on a pale reef that drops into the deep blue, with surf on its windward side.
pub fn atoll(canvas: &mut Canvas, x: f64, y: f64, r: f64, behavior: usize) {
    let seed = behavior as f64;
    let ring = |radius: f64, wobble: f64| -> Vec<(f64, f64)> {
        (0..=72)
            .map(|k| {
                let a = f64::from(k) / 72.0 * PI * 2.0;
                let q = radius * (1.0 + wobble * fbm3(a.cos() * 1.6, a.sin() * 1.6, seed, 19.0, 3));
                (x + a.cos() * q * 1.12, y + a.sin() * q * 0.9)
            })
            .collect()
    };
    let shape = |points: &[(f64, f64)]| {
        let mut path = through(points);
        path.close();
        path
    };
    let halo = Gradient::radial(x, y, r * 0.9, x, y, r * 1.8)
        .stop(0.0, Colour::rgba(46, 146, 164, 0.55))
        .stop(1.0, Colour::rgba(30, 100, 140, 0.0));
    canvas.fill(&shape(&ring(r * 1.8, 0.2)), &Brush::new(halo));
    canvas.fill(
        &shape(&ring(r * 1.14, 0.16)),
        &Brush::new(Colour::rgb(98, 186, 186)),
    );
    canvas.fill(
        &shape(&ring(r, 0.16)),
        &Brush::new(Colour::rgb(238, 230, 198)),
    );
    let lagoon = Gradient::radial(x - r * 0.15, y - r * 0.15, 0.0, x, y, r * 0.75)
        .stop(0.0, Colour::rgb(128, 226, 216))
        .stop(1.0, Colour::rgb(52, 178, 190));
    canvas.fill(&shape(&ring(r * 0.68, 0.16)), &Brush::new(lagoon));
    let mut rng = Rng::new((behavior * 41 + 3) as u32);
    let middle = ring(r * 0.84, 0.16);
    for k in 0..2 {
        let at = ((f64::from(k) * 0.5 + 0.12 + 0.2 * rng.next_f64()) * 72.0).floor() as usize;
        let pass = through(&[middle[at], middle[(at + 3) % 72]]);
        canvas.stroke(
            &pass,
            &Pen::new(3.2),
            &Brush::new(Colour::rgb(76, 196, 200)),
        );
    }
    for _ in 0..3 {
        let start = (rng.next_f64() * 72.0).floor() as usize;
        let length = 4 + (rng.next_f64() * 6.0).floor() as usize;
        let islet: Vec<(f64, f64)> = (0..=length).map(|q| middle[(start + q) % 72]).collect();
        canvas.stroke(
            &through(&islet),
            &Pen::new(1.6).cap(crate::world::raster::Cap::Round),
            &Brush::new(Colour::rgb(74, 128, 64)),
        );
    }
    let outer = ring(r * 1.18, 0.16);
    canvas.stroke(
        &through(&outer[26..=46]),
        &Pen::new(1.0).cap(crate::world::raster::Cap::Round),
        &Brush::new(Colour::rgba(244, 250, 252, 0.75)),
    );
}

/// The short track a tornado scoured over the land, running back from its foot away from its
/// behavior.
fn tornado_track(canvas: &mut Canvas, ground: &Ground, feature: &Feature) {
    let funnel = feature.radius - 4.0;
    let (lx, ly) = ground.centre[feature.behavior];
    let (x, y) = (feature.x, feature.y);
    let mut rng = Rng::new((feature.behavior * 17 + 5) as u32);
    let back = (ly - y).atan2(lx - x) + PI + (rng.next_f64() - 0.5) * 0.8;
    let length = 18.0 + funnel * 2.4;
    let foot = (x, y + funnel * 0.95);
    let mut points = vec![foot];
    for s in 1..=10 {
        let t = f64::from(s) / 10.0;
        let a = back + (t * 6.0).sin() * 0.25;
        points.push((foot.0 + a.cos() * length * t, foot.1 + a.sin() * length * t));
    }
    canvas.stroke(
        &through(&points),
        &Pen::new(2.0).cap(crate::world::raster::Cap::Round),
        &Brush::new(Colour::rgba(96, 74, 54, 0.6)),
    );
}

/// A shoal's sandbank seen through the water at a pixel: a long low bar a little bent, lighter
/// along its crest where it is shallowest, a faint rim of shallows round it and ripples across.
pub fn shoal_at(shoal: &Shoal, x: f64, y: f64, sea: Rgb) -> Rgb {
    let (dx, dy) = (x - shoal.x, y - shoal.y);
    if dx * dx + dy * dy > 24.0 * 24.0 {
        return sea;
    }
    let (sin, cos) = shoal.angle.sin_cos();
    let u = dx * cos + dy * sin;
    let v = -dx * sin + dy * cos - 0.004 * u * u + 0.4;
    let d = (u / 15.0).hypot(v / 5.0) + 0.18 * fbm3(u / 7.0, v / 7.0, shoal.x * 0.37, 9.0, 3);
    if d > 1.7 {
        return sea;
    }
    let colour = mix_rgb(sea, [30.0, 92.0, 112.0], 0.45 * smooth(1.7, 1.0, d));
    let colour = mix_rgb(colour, [60.0, 128.0, 138.0], 0.85 * smooth(1.1, 0.78, d));
    let ripple = 0.5 + 0.5 * (u * 1.3 + 2.0 * (v * 0.5).sin()).sin();
    let colour = mix_rgb(
        colour,
        [96.0, 160.0, 158.0],
        0.2 * ripple * smooth(1.0, 0.6, d),
    );
    mix_rgb(colour, [150.0, 190.0, 172.0], 0.75 * smooth(0.55, 0.2, d))
}
