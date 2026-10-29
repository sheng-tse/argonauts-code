//! A thunderstorm, seen from the south as a storm cloud reads at the map's size: a broad anvil
//! with a flat sharp top spread wider than its tower and furthest downwind, a billowing tower lit
//! from the west, a flat dark base with rain slanting from it to the ground, and one flash of
//! lightning to the ground for each session stopped there, up to 4 in every 4 seconds. Held
//! still, the bolt stays, dimmer.

use super::{Pad, Weather};
use crate::world::map::fields::{Rgb, byte, mix_rgb};
use crate::world::map::motion::{Flashes, LIGHTNING, Moment};
use crate::world::raster::noise::{Rng, fbm3, mix, smooth};
use crate::world::raster::{Blend, Canvas, Cap, Colour, Gradient, Join, Path, Pen};

/// How far above the ground a storm cloud flies, in pixels.
pub const ALTITUDE: f64 = 16.0;

/// A storm cloud's picture, and the middle of its base, which stands over its place.
#[derive(Debug, Clone)]
pub struct Cloud {
    pub canvas: Canvas,
    pub base: (f64, f64),
}

/// A storm cloud of size `r`: each part round lumps summed into one soft body, its billows from
/// noise, lit by its slope.
pub fn cloud(r: f64, seed: f64) -> Cloud {
    let (width, height) = ((r * 4.2).ceil() as usize, (r * 3.3).ceil() as usize);
    let mut rng = Rng::new((seed * 7.0 + 1.0) as u32);
    let (bx, by) = (width as f64 * 0.42, height as f64 - r * 0.3);
    // The anvil's flat top, and the bottom of its middle.
    let (top, anvil) = (by - r * 2.75, by - r * 2.3);
    let mut balls: Vec<(f64, f64, f64)> = Vec::new();
    // The base.
    for k in 0..9 {
        let size = r * 0.38 * (0.9 + 0.2 * rng.next_f64());
        balls.push((bx + (f64::from(k) - 4.0) * r * 0.24, by - r * 0.3, size));
    }
    // The tower, a little narrower midway.
    for k in 0..14 {
        let u = f64::from(k) / 13.0;
        let waist = 1.0 - 0.22 * (u * std::f64::consts::PI).sin();
        let x = bx + (rng.next_f64() - 0.5) * r * 0.85 * waist;
        let size = r * (0.56 - 0.1 * u) * (0.85 + 0.3 * rng.next_f64());
        balls.push((x, by - r * 0.55 - u * r * 1.6, size));
    }
    // The anvil: a wedge, thick over the tower, thin at its tips.
    for k in 0..26 {
        let u = f64::from(k) / 25.0;
        let offset = mix(-1.3, 1.9, u) * r;
        let reach = if offset < 0.0 { 1.3 * r } else { 1.9 * r };
        let t = (offset.abs() / reach).min(1.0);
        let size = r * (0.46 - 0.33 * t) * (0.92 + 0.16 * rng.next_f64());
        balls.push((bx + offset, top + size * 0.75, size));
    }
    let mut density = vec![0.0_f32; width * height];
    for j in 0..height {
        let jf = j as f64;
        // The anvil's flat top and the flat base, both cut sharp.
        if jf < top || jf > by + r * 0.04 {
            continue;
        }
        for i in 0..width {
            let mut v = 0.0;
            for &(cx, cy, radius) in &balls {
                let dx = (i as f64 + 0.5 - cx) / radius;
                let dy = (jf + 0.5 - cy) / radius;
                v += (-2.4 * (dx * dx + dy * dy)).exp();
            }
            let billow = fbm3(i as f64 / (r * 0.16), jf / (r * 0.16), seed, 11.0, 4);
            density[j * width + i] = (v * (1.0 + 0.3 * billow)) as f32;
        }
    }
    let light = {
        let l = (0.8_f64 * 0.8 + 0.55 * 0.55 + 0.4 * 0.4).sqrt();
        (-0.8 / l, -0.55 / l, 0.4 / l)
    };
    let k = r * 0.5;
    let mut rgba = vec![0_u8; width * height * 4];
    for j in 1..height.saturating_sub(1) {
        for i in 1..width.saturating_sub(1) {
            let at = j * width + i;
            let v = f64::from(density[at]);
            let alpha = smooth(0.5, 0.72, v);
            if alpha <= 0.01 {
                continue;
            }
            let gx = (f64::from(density[at + 1]) - f64::from(density[at - 1])) * k;
            let gy = (f64::from(density[at + width]) - f64::from(density[at - width])) * k;
            let length = (gx * gx + gy * gy + 1.0).sqrt();
            let lit = ((-gx * light.0 - gy * light.1 + light.2) / length).max(0.0);
            let y = j as f64 + 0.5;
            let across = i as f64 + 0.5 - bx;
            let mut colour: Rgb = mix_rgb(
                [112.0, 120.0, 142.0],
                [250.0, 252.0, 255.0],
                (0.2 + 0.9 * lit).clamp(0.0, 1.0),
            );
            // The tower's east side, away from the sun; the anvil's sunlit top and its shaded
            // underside; the dark base the rain falls from.
            colour = mix_rgb(
                colour,
                [118.0, 126.0, 150.0],
                smooth(-r * 0.3, r * 0.7, across) * 0.5,
            );
            colour = mix_rgb(colour, [255.0; 3], smooth(anvil - r * 0.1, top, y) * 0.6);
            colour = mix_rgb(
                colour,
                [128.0, 136.0, 160.0],
                smooth(top + r * 0.2, top + r * 0.5, y) * smooth(r * 0.7, r, across.abs()) * 0.6,
            );
            colour = mix_rgb(
                colour,
                [54.0, 60.0, 82.0],
                smooth(by - r * 0.85, by - r * 0.1, y) * 0.92,
            );
            let o = at * 4;
            rgba[o] = byte(colour[0]);
            rgba[o + 1] = byte(colour[1]);
            rgba[o + 2] = byte(colour[2]);
            rgba[o + 3] = byte(255.0 * alpha);
        }
    }
    let canvas =
        Canvas::from_rgba(width as u32, height as u32, &rgba).unwrap_or_else(|| Canvas::new(1, 1));
    Cloud {
        canvas,
        base: (bx, by),
    }
}

/// A storm cloud's size from its mark's radius.
pub fn size(radius: f64) -> f64 {
    (radius * 1.25 + 0.5).floor()
}

/// The cloud's soft shade on the ground under it, thrown east-south-east.
pub fn shade(pad: &mut Pad, (x, y): (f64, f64), radius: f64) {
    let rr = radius * 0.9;
    let dark = Gradient::radial(x, y, 0.0, x, y, rr)
        .stop(0.0, Colour::rgba(6, 8, 14, 0.34))
        .stop(1.0, Colour::rgba(6, 8, 14, 0.0));
    let mut patch = Path::new();
    patch.ellipse(x, y, rr, rr * 0.6, 0.0, 0.0, 7.0, false);
    pad.fill(&patch, &pad.brush(dark));
}

/// How brightly the storm flashes at a moment, 0 between its flashes; held still, a lasting
/// glow, so the meaning reads.
pub fn flash(flashes: &Flashes, moment: Moment) -> f64 {
    let Moment::At(_) = moment else {
        return 0.75;
    };
    flashes.at(moment.time(LIGHTNING)).0
}

/// Draws the storm: rain from its base to the ground, the cloud at its height, and when it
/// flashes, the cloud lit from inside and one bolt to the ground.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    pad: &mut Pad,
    weather: &Weather,
    cloud: (&Canvas, (f64, f64)),
    over: (f64, f64),
    radius: f64,
    flashes: Flashes,
    behavior: usize,
    moment: Moment,
) {
    let (x, y) = weather.screen(over.0, over.1, ALTITUDE);
    let (gx, gy) = weather.screen(over.0, over.1, 0.0);
    let size = size(radius);
    for k in 0..6 {
        let u = (f64::from(k) + 0.5) / 6.0;
        let x0 = x + (u - 0.5) * size * 1.5;
        let y1 = gy + 1.0 + f64::from(k % 2) * 2.0;
        let fall = Gradient::linear(0.0, y + 1.0, 0.0, y1)
            .stop(0.0, Colour::rgba(150, 160, 186, 0.5))
            .stop(1.0, Colour::rgba(150, 160, 186, 0.05));
        let mut streak = Path::new();
        streak.move_to(x0, y + 1.0);
        streak.line_to(x0 + 3.0, y1);
        pad.stroke(&streak, &Pen::new(1.4).cap(Cap::Round), &pad.brush(fall));
    }
    let (picture, base) = cloud;
    pad.draw(picture, x - base.0, y - base.1, &pad.brush(Colour::WHITE));
    let flash = flash(&flashes, moment);
    if flash <= 0.0 {
        return;
    }
    let still = moment.is_still();
    if !still {
        let cy = y - size * 0.55;
        let glow = Gradient::radial(x, cy, 0.0, x, cy, size * 0.95)
            .stop(0.0, Colour::rgba(200, 214, 255, 0.3 * flash))
            .stop(1.0, Colour::rgba(190, 210, 255, 0.0));
        let mut lit = Path::new();
        lit.circle(x, cy, size * 0.95);
        pad.fill(&lit, &pad.brush(glow).blend(Blend::Lighter));
    }
    let strike = if still {
        0
    } else {
        (moment.time(LIGHTNING) % LIGHTNING.seconds * 4.0).floor() as usize
    };
    let mut rng = Rng::new((behavior * 19 + 7 + strike) as u32);
    let x0 = x + (rng.next_f64() - 0.5) * size * 0.5;
    let mut bolt = Path::new();
    bolt.move_to(x0, y - 1.0);
    let steps = 4;
    let (ex, ey) = (gx + (rng.next_f64() - 0.5) * size * 0.5, gy + 2.0);
    for k in 1..=steps {
        let u = f64::from(k) / f64::from(steps);
        let kink = if k < steps {
            let side = if k % 2 == 1 { 1.0 } else { -1.0 };
            side * (2.0 + rng.next_f64() * 2.5)
        } else {
            0.0
        };
        bolt.line_to(mix(x0, ex, u) + kink, mix(y - 1.0, ey, u));
    }
    for (colour, width) in [
        (Colour::rgba(150, 170, 255, 0.55), 4.0),
        (Colour::rgba(255, 250, 214, 1.0), 1.6),
    ] {
        let pen = Pen::new(width).cap(Cap::Round).join(Join::Miter);
        pad.stroke(&bolt, &pen, &pad.brush(colour).alpha(flash));
    }
}
