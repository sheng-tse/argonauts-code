//! A tsunami: one great wave standing on the water in profile, its white crest curled over toward
//! its behavior's coast, rolling in from open sea over 3 s and starting again. As it nears, the
//! sea draws back from the shore ahead of it, and where it lands white surge breaks on the coast.
//! Held still, it stands most of the way in, the sea drawn back before it.

use super::{Pad, Weather};
use crate::world::map::fields::Ground;
use crate::world::map::motion::{Moment, TSUNAMI};
use crate::world::map::nature::Wave;
use crate::world::raster::noise::{hash3, mix, smooth};
use crate::world::raster::{Brush, Canvas, Cap, Colour, Gradient, Join, Path, Pen, Transform};

/// Where a still wave stands along its run.
const STILL: f64 = 0.72;

/// A tsunami as the weather draws it: its run, its picture and the coast it runs at.
#[derive(Debug, Clone)]
pub struct Tsunami {
    pub behavior: usize,
    pub wave: Wave,
    picture: WavePicture,
    /// Points just off the shore across the wave's width, each with how far off its middle it
    /// lies, 0 to 0.9; `None` where no shore was found.
    coast: Vec<Option<(f64, f64, f64)>>,
}

/// The wave's picture and the point of it that stands on the water: the middle of its foot.
#[derive(Debug, Clone)]
pub struct WavePicture {
    pub canvas: Canvas,
    pub anchor: (f64, f64),
}

impl Tsunami {
    pub fn new(ground: &Ground, behavior: usize, wave: &Wave) -> Self {
        Self {
            behavior,
            wave: *wave,
            picture: wave_picture(wave.size, wave.face),
            coast: coast(ground, wave),
        }
    }

    /// The points just off the shore the wave runs at.
    pub fn shore(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.coast.iter().flatten().map(|&(x, y, _)| (x, y))
    }

    /// How far along its run the wave is at a moment, from 0 out at sea to 1 at the coast.
    pub fn progress(moment: Moment) -> f64 {
        moment.phase(TSUNAMI, STILL) % 1.0
    }

    /// Where the wave stands at `u` of its run, and how much of it shows.
    pub fn at(&self, u: f64) -> (f64, f64, f64) {
        let wave = &self.wave;
        let life = smooth(0.0, 0.1, u) * (1.0 - smooth(0.94, 1.0, u));
        (
            mix(wave.from.0, wave.to.0, u),
            mix(wave.from.1, wave.to.1, u),
            life,
        )
    }

    /// The box the wave fills on the screen at a moment, and its middle, for the pointer.
    pub fn bounds(&self, weather: &Weather, moment: Moment) -> ([f64; 4], (f64, f64)) {
        wave_bounds(&self.wave, weather, moment)
    }

    pub fn draw(&self, pad: &mut Pad, weather: &Weather, moment: Moment) {
        let u = Self::progress(moment);
        let (x, y, life) = self.at(u);
        if life < 0.02 {
            return;
        }
        let back = smooth(0.3, 0.75, u);
        if back > 0.0 {
            for &(qx, qy, off) in self.coast.iter().flatten() {
                let (sx, sy) = weather.screen(qx, qy, 0.0);
                let alpha = (0.6 * back * (1.0 - 0.6 * off) * 1000.0).round() / 1000.0;
                let mut seabed = Path::new();
                seabed.ellipse(sx, sy, 5.0, 2.8, 0.0, 0.0, 7.0, false);
                let brush = pad.brush(Colour::rgba(160, 172, 142, alpha)).alpha(life);
                pad.fill(&seabed, &brush);
            }
        }
        let (sx, sy) = weather.screen(x, y, 0.0);
        let left = (sx - self.picture.anchor.0 + 0.5).floor();
        let top = (sy - self.picture.anchor.1 + 0.5).floor();
        let brush = pad.brush(Colour::WHITE).alpha(life);
        pad.draw(&self.picture.canvas, left, top, &brush);
        let surge = if moment.is_still() {
            0.0
        } else {
            smooth(0.82, 0.96, u)
        };
        if surge > 0.0 {
            let (ux, uy) = self.wave.out;
            let spray = pad.brush(Colour::rgba(244, 250, 252, 0.92)).alpha(life);
            let l = self.behavior as f64;
            for (i, point) in self.coast.iter().enumerate() {
                let Some((qx, qy, off)) = *point else {
                    continue;
                };
                if off > 0.65 {
                    continue;
                }
                let (cx, cy) = weather.screen(qx - ux * 3.0, qy - uy * 3.0, 0.0);
                let i = i as f64;
                for k in 0..3 {
                    let k = f64::from(k);
                    let mut drop = Path::new();
                    drop.circle(
                        cx + (hash3(i, k, l, 3.0) - 0.5) * 5.0,
                        cy - surge * (2.0 + 4.0 * hash3(i, k, l, 5.0)),
                        1.0 + 1.6 * hash3(i, k, l, 7.0) * surge,
                    );
                    pad.fill(&drop, &spray);
                }
            }
        }
    }
}

/// The box a wave fills on the screen at a moment, as left, top, right and bottom, and the
/// middle the pointer's ring goes round.
pub fn wave_bounds(wave: &Wave, weather: &Weather, moment: Moment) -> ([f64; 4], (f64, f64)) {
    let u = Tsunami::progress(moment);
    let (x, y) = (
        mix(wave.from.0, wave.to.0, u),
        mix(wave.from.1, wave.to.1, u),
    );
    let (sx, sy) = weather.screen(x, y, 0.0);
    let (w, h) = ((wave.size * 1.5).ceil(), (wave.size * 0.86).ceil());
    let anchor = if wave.face > 0.0 {
        w - 0.75 * wave.size
    } else {
        0.75 * wave.size
    };
    let left = sx - anchor;
    (
        [left, sy - h, left + w, sy + 2.0],
        (left + w / 2.0, sy - h * 0.45),
    )
}

/// The coast a wave runs at, as points just off the shore across its width.
pub fn coast(ground: &Ground, wave: &Wave) -> Vec<Option<(f64, f64, f64)>> {
    let (ux, uy) = wave.out;
    let (px, py) = (-uy, ux);
    let mut points = Vec::new();
    let mut o = -0.9;
    while o <= 0.9001 {
        let mut x = wave.coast.0 + px * o * wave.size;
        let mut y = wave.coast.1 + py * o * wave.size;
        let mut k = 0;
        // To the shore if at sea, then just off it.
        while k < 40 && ground.land_at(x, y) < 0.95 {
            x -= ux * 2.0;
            y -= uy * 2.0;
            k += 1;
        }
        while k < 80 && ground.land_at(x, y) >= 0.95 {
            x += ux * 2.0;
            y += uy * 2.0;
            k += 1;
        }
        points.push((k < 80).then_some((x + ux * 2.0, y + uy * 2.0, f64::abs(o))));
        o += 0.1;
    }
    points
}

/// The wave itself, standing on the water in profile as Hokusai drew one: a long lit back slope,
/// a tall dark face under a white crest that curls over toward the coast, foam claws off its
/// lip, spray thrown ahead and whitewater at its foot. Drawn travelling left; `face` 1 mirrors
/// it.
pub fn wave_picture(size: f64, face: f64) -> WavePicture {
    let s = size;
    let (cw, ch) = ((s * 1.5).ceil(), (s * 0.86).ceil());
    let base = ch - 5.0;
    let mut canvas = Canvas::new(cw as u32, ch as u32);
    let mirror = if face > 0.0 {
        Transform::IDENTITY.translate(cw, 0.0).scale(-1.0, 1.0)
    } else {
        Transform::IDENTITY
    };
    let brush = |ink: Colour| Brush::new(ink).at(mirror);
    let p = |u: f64, v: f64| (u * s, base - v * s);
    let ellipse = |cx: f64, cy: f64, rx: f64, ry: f64, rotation: f64| {
        let mut path = Path::new();
        path.ellipse(cx, cy, rx, ry, rotation, 0.0, 7.0, false);
        path
    };
    let curve = |path: &mut Path, a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        let (a, b, c) = (p(a.0, a.1), p(b.0, b.1), p(c.0, c.1));
        path.cubic_to(a.0, a.1, b.0, b.1, c.0, c.1);
    };

    // Its shade on the sea.
    canvas.fill(
        &ellipse(0.82 * s, base + 1.5, 0.62 * s, 0.07 * s, 0.0),
        &brush(Colour::rgba(4, 10, 20, 0.32)),
    );
    let mut body = Path::new();
    let start = p(0.3, 0.0);
    body.move_to(start.0, start.1);
    let corner = p(1.42, 0.0);
    body.line_to(corner.0, corner.1);
    // The long back slope up to the top, the crest curling over, the lip hooking back in, and
    // down the hollow face.
    curve(&mut body, (1.06, 0.05), (0.88, 0.68), (0.6, 0.68));
    curve(&mut body, (0.4, 0.69), (0.18, 0.62), (0.17, 0.46));
    curve(&mut body, (0.17, 0.37), (0.27, 0.35), (0.34, 0.41));
    curve(&mut body, (0.43, 0.32), (0.41, 0.08), (0.3, 0.0));
    let water = Gradient::linear(0.0, base - 0.68 * s, 0.0, base)
        .stop(0.0, Colour::rgb(104, 176, 196))
        .stop(0.45, Colour::rgb(46, 112, 148))
        .stop(1.0, Colour::rgb(16, 54, 92));
    canvas.fill(&body, &Brush::new(water).at(mirror));
    canvas.with_clip(&body, mirror, |canvas| {
        // The back slope catches the low sun.
        let lit = Gradient::linear(1.3 * s, 0.0, 0.5 * s, 0.0)
            .stop(0.0, Colour::rgba(140, 200, 215, 0.0))
            .stop(1.0, Colour::rgba(150, 210, 222, 0.35));
        canvas.fill_rect(0.0, 0.0, cw, ch, &Brush::new(lit).at(mirror));
        // The face in its own shade.
        let mut hollow = Path::new();
        let lip = p(0.34, 0.41);
        hollow.move_to(lip.0, lip.1);
        curve(&mut hollow, (0.43, 0.32), (0.41, 0.08), (0.3, 0.0));
        let foot = p(0.62, 0.0);
        hollow.line_to(foot.0, foot.1);
        curve(&mut hollow, (0.64, 0.18), (0.58, 0.38), (0.46, 0.5));
        hollow.close();
        canvas.fill(&hollow, &brush(Colour::rgba(4, 24, 44, 0.78)));
        // Foam streaks down the back.
        let streaks = Pen::new((s * 0.022).max(1.0));
        for k in [0.25, 0.45, 0.65] {
            let mut streak = Path::new();
            let from = p(0.62 + 0.18 * k, 0.64 - 0.1 * k);
            streak.move_to(from.0, from.1);
            curve(
                &mut streak,
                (0.8 + 0.3 * k, 0.48 - 0.2 * k),
                (1.0 + 0.2 * k, 0.2),
                (1.1 + 0.2 * k, 0.04),
            );
            canvas.stroke(&streak, &streaks, &brush(Colour::rgba(226, 242, 248, 0.5)));
        }
    });
    let foam = Colour::rgb(246, 251, 253);
    let round = |width: f64| Pen::new(width).cap(Cap::Round).join(Join::Round);
    // The white crest.
    let mut crest = Path::new();
    let from = p(0.8, 0.6);
    crest.move_to(from.0, from.1);
    curve(&mut crest, (0.7, 0.69), (0.42, 0.7), (0.26, 0.6));
    curve(&mut crest, (0.18, 0.54), (0.16, 0.44), (0.22, 0.4));
    canvas.stroke(&crest, &round((s * 0.07).max(2.0)), &brush(foam));
    // Foam claws hanging off the lip.
    for k in 0..6 {
        let t = f64::from(k) / 5.0;
        let (u, v) = (mix(0.4, 0.2, t), mix(0.66, 0.43, t) - 0.02);
        let claw = ellipse(
            u * s - 0.03 * s,
            base - v * s + 0.05 * s,
            0.03 * s,
            0.05 * s,
            0.5,
        );
        canvas.fill(&claw, &brush(foam));
    }
    // Spray thrown ahead.
    for k in 0..9 {
        let k = f64::from(k);
        let u = 0.1 + 0.18 * hash3(k, 1.0, size, 3.0);
        let v = 0.45 + 0.28 * hash3(k, 2.0, size, 3.0);
        let r = (s * (0.012 + 0.016 * hash3(k, 3.0, size, 3.0))).max(0.8);
        let mut drop = Path::new();
        drop.circle(u * s, base - v * s, r);
        let alpha = 0.6 + 0.4 * hash3(k, 4.0, size, 3.0);
        canvas.fill(&drop, &brush(foam).alpha(alpha));
    }
    // Whitewater at the foot of its face, broken, so it stands in the water.
    for k in 0..5 {
        let kf = f64::from(k);
        let u = 0.2 + 0.09 * kf + 0.03 * hash3(kf, 5.0, size, 3.0);
        let rx = s * (0.07 + 0.04 * hash3(kf, 6.0, size, 3.0));
        let wash = ellipse(
            u * s,
            base - 1.0 - s * 0.012 * hash3(kf, 7.0, size, 3.0),
            rx,
            (s * 0.04).max(1.0),
            0.0,
        );
        canvas.fill(&wash, &brush(foam).alpha(0.95 - 0.15 * kf));
    }
    // The sea's own line, faint along its back.
    let mut line = Path::new();
    let from = p(0.62, 0.005);
    line.move_to(from.0, from.1);
    for k in 1..=8 {
        let k = f64::from(k);
        let to = p(0.62 + k * 0.1, 0.005 + 0.01 * (k * 2.1).sin());
        line.line_to(to.0, to.1);
    }
    canvas.stroke(
        &line,
        &round((s * 0.02).max(1.0)),
        &brush(Colour::rgba(200, 228, 240, 0.35)),
    );
    let anchor_x = if face > 0.0 { cw - 0.75 * s } else { 0.75 * s };
    WavePicture {
        canvas,
        anchor: (anchor_x, base),
    }
}
