//! The globe drawn pixel by pixel: the ground read from the skin between texels, lit by the sun
//! with the ground's own relief, the sea glinting, the clouds and their shade, the night's
//! lights, the haze toward the rim, and the air glowing past it.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use super::{Air, CloudsIn, Planet, Sun};
use crate::base::draw::geo::Globe;
use crate::world::map::fields::{Look, Rgb, in_rows};
use crate::world::raster::Canvas;
use crate::world::raster::noise::{mix, smooth};

/// The sky's warm light where the sun sets behind the rim.
const SUNSET: Rgb = [255.0, 150.0, 90.0];
/// Texels west of a cloud that its shade falls on.
const SHADE_SHIFT: usize = 6;

/// The pixels a globe and its air cover in a scene, clipped to the scene: from (`left`, `top`),
/// `width` × `height`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
}

impl Bounds {
    /// The box round `globe` and as far as its air glows, in a scene `size` px.
    pub fn round(globe: &Globe, air: &Air, size: (usize, usize)) -> Self {
        let reach = globe.radius + air.reach(globe.radius) + 2.0;
        let (cx, cy) = globe.centre;
        let clip = |from: f64, to: f64, most: usize| {
            let from = from.floor().clamp(0.0, most as f64) as usize;
            let to = to.ceil().clamp(0.0, most as f64) as usize;
            (from, to.max(from))
        };
        let (left, right) = clip(cx - reach, cx + reach, size.0);
        let (top, bottom) = clip(cy - reach, cy + reach, size.1);
        Self {
            left,
            top,
            width: (right - left).max(1),
            height: (bottom - top).max(1),
        }
    }

    pub fn canvas(&self) -> Canvas {
        Canvas::new(self.width as u32, self.height as u32)
    }
}

/// One pixel's straight colour and alpha.
type Pixel = [f64; 4];

/// The globe and its air painted over nothing into a canvas the size of `bounds`, `fade`
/// strong. The clouds come with it, drifted `drift` radians east, unless they are kept apart.
pub fn paint(
    planet: &Planet,
    globe: &Globe,
    bounds: Bounds,
    sun: Sun,
    fade: f64,
    clouds: CloudsIn,
    drift: f64,
) -> Canvas {
    let skin = &planet.skin;
    let texture = &skin.texture;
    let gas = planet.look == Look::Gas;
    let air = planet.air;
    let thick = air.thickness;
    let radius = globe.radius;
    let reach = air.reach(radius);
    let (sin_tilt, cos_tilt) = globe.view.tilt.sin_cos();
    let width = texture.width;
    let height = texture.height;
    let shift = drift_texels(drift, width);
    let with_clouds = clouds == CloudsIn::Ground;
    paint_pixels(bounds, globe, |nx, ny, r2| {
        if r2 > 1.0 {
            return air_glow(nx, ny, r2, radius, reach, thick, air.colour, sun, fade);
        }
        let nz = (1.0 - r2).sqrt();
        let lat = (nz * sin_tilt + ny * cos_tilt).clamp(-1.0, 1.0).asin();
        let lon = globe.view.lon + nx.atan2(nz * cos_tilt - ny * sin_tilt);
        let u = ((lon + PI) / TAU).rem_euclid(1.0);
        let v = (FRAC_PI_2 - lat) / PI;
        let column = ((u * width as f64) as usize).min(width - 1);
        let row = ((v * height as f64).max(0.0) as usize).min(height - 1);
        let at = row * width + column;
        let mut colour = between_texels(&texture.rgb, width, height, u, v);

        // The ground's relief tips the normal the light sees.
        let normal = [nx, ny, nz];
        let mut bent = normal;
        let (east, north) = (
            f64::from(texture.slope_east[at]),
            f64::from(texture.slope_north[at]),
        );
        if east != 0.0 || north != 0.0 {
            let mut e = [cos_tilt * nz - sin_tilt * ny, sin_tilt * nx, -cos_tilt * nx];
            let length = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
            let length = if length > 0.0 { length } else { 1.0 };
            e = e.map(|c| c / length);
            let o = [
                ny * e[2] - nz * e[1],
                nz * e[0] - nx * e[2],
                nx * e[1] - ny * e[0],
            ];
            for axis in 0..3 {
                bent[axis] += east * e[axis] + north * o[axis];
            }
            let length = (bent[0] * bent[0] + bent[1] * bent[1] + bent[2] * bent[2]).sqrt();
            bent = bent.map(|c| c / length);
        }
        let mu = sun.facing(normal);
        let lambert = sun.facing(bent).max(0.0);
        let day = smooth(-0.1, 0.22, mu);
        let light = if gas {
            0.03 + day * (0.16 + 0.95 * lambert) * (0.6 + 0.4 * nz.powf(0.35))
        } else {
            0.035 + day * (0.16 + 0.95 * lambert)
        };
        colour = colour.map(|c| c * light);
        // The light reddens where the sun is low.
        let twilight = (-((mu - 0.05) / 0.11).powi(2)).exp() * (0.25 + 0.5 * thick);
        colour[0] *= 1.0 + 0.16 * twilight;
        colour[1] *= 1.0 - 0.02 * twilight;
        colour[2] *= 1.0 - 0.18 * twilight;
        if texture.wet[at] && !gas {
            let h = (nx * sun.half[0] + ny * sun.half[1] + nz * sun.half[2]).max(0.0);
            let glint = (h.powi(220) * 0.6 + h.powi(24) * 0.07) * day;
            let rim = (1.0 - nz).powi(5) * 0.35 * day;
            for (ch, sunlit) in [255.0, 244.0, 228.0].into_iter().enumerate() {
                colour[ch] += sunlit * glint + air.colour[ch] * rim * 0.5;
            }
        }
        let (mut cloud, mut shade) = settled_at(skin, column, row);
        if with_clouds {
            let (drifting, its_shade) = drifting_at(skin, column, row, shift);
            cloud = cloud.max(drifting);
            shade = shade.max(its_shade);
        }
        colour = clouded(colour, cloud, shade, day, mu);
        if let Some(glow) = &skin.glow {
            // Lava, burning fronts, lightning and the aurora shine on the night side.
            let k = (1.0 - day) * 0.95 + 0.06;
            for ch in 0..3 {
                colour[ch] += f64::from(glow[at][ch]) * k;
            }
        }
        let haze = haze(nz, mu, thick);
        let lift = 0.3 + 0.9 * (mu + 0.15).max(0.0);
        for (channel, of_air) in colour.iter_mut().zip(air.colour) {
            *channel = mix(*channel, of_air * lift, haze);
        }
        let cover = ((1.0 - r2.sqrt()) * radius + 0.5).clamp(0.0, 1.0) * fade;
        [colour[0], colour[1], colour[2], cover]
    })
}

/// The clouds alone, for a frame drawn with [`CloudsIn::Apart`]: each pixel the cloud's colour
/// as the frame would have mixed it, hazed toward the rim, and as opaque as the cloud and its
/// shade together, so laid over the frame it shows what the frame would have.
pub fn paint_clouds(
    planet: &Planet,
    globe: &Globe,
    bounds: Bounds,
    sun: Sun,
    drift: f64,
) -> Canvas {
    let skin = &planet.skin;
    let width = skin.texture.width;
    let height = skin.texture.height;
    let thick = planet.air.thickness;
    let colour = planet.air.colour;
    let radius = globe.radius;
    let (sin_tilt, cos_tilt) = globe.view.tilt.sin_cos();
    let shift = drift_texels(drift, width);
    paint_pixels(bounds, globe, |nx, ny, r2| {
        if r2 > 1.0 {
            return [0.0; 4];
        }
        let nz = (1.0 - r2).sqrt();
        let lat = (nz * sin_tilt + ny * cos_tilt).clamp(-1.0, 1.0).asin();
        let lon = globe.view.lon + nx.atan2(nz * cos_tilt - ny * sin_tilt);
        let u = ((lon + PI) / TAU).rem_euclid(1.0);
        let column = ((u * width as f64) as usize).min(width - 1);
        let row = (((FRAC_PI_2 - lat) / PI * height as f64).max(0.0) as usize).min(height - 1);
        let (cloud, shade) = drifting_at(skin, column, row, shift);
        if cloud <= 0.0 && shade <= 0.0 {
            return [0.0; 4];
        }
        let mu = sun.facing([nx, ny, nz]);
        let day = smooth(-0.1, 0.22, mu);
        let dark = (0.35 * shade * day).clamp(0.0, 1.0);
        let lit = 0.03 + day * (0.34 + 0.8 * mu.max(0.0));
        let haze = haze(nz, mu, thick);
        let lift = 0.3 + 0.9 * (mu + 0.15).max(0.0);
        let white = [240.0, 244.0, 250.0].map(|c| c * lit);
        let hazed = [0, 1, 2].map(|ch| mix(white[ch], colour[ch] * lift, haze));
        let alpha = 1.0 - (1.0 - dark) * (1.0 - cloud);
        if alpha <= 0.0 {
            return [0.0; 4];
        }
        let cover = ((1.0 - r2.sqrt()) * radius + 0.5).clamp(0.0, 1.0);
        let k = cloud / alpha;
        [hazed[0] * k, hazed[1] * k, hazed[2] * k, alpha * cover]
    })
}

/// How far clouds drifted `drift` radians east have moved, in whole texels of a texture `width`
/// round.
pub fn drift_texels(drift: f64, width: usize) -> usize {
    let texels = (drift / TAU * width as f64).round() as i64;
    texels.rem_euclid(width as i64) as usize
}

/// The cloud that stays put over a texel, and the shade the cloud just east of it casts there.
fn settled_at(skin: &super::Skin, column: usize, row: usize) -> (f64, f64) {
    let width = skin.texture.width;
    skin.settled.as_ref().map_or((0.0, 0.0), |settled| {
        (
            f64::from(settled[row * width + column]),
            f64::from(settled[row * width + west(column, SHADE_SHIFT, width)]),
        )
    })
}

/// The drifting cloud over a texel and its neighbour's shade, the clouds moved `shift` texels
/// east.
fn drifting_at(skin: &super::Skin, column: usize, row: usize, shift: usize) -> (f64, f64) {
    let width = skin.texture.width;
    skin.clouds.as_ref().map_or((0.0, 0.0), |clouds| {
        let from = west(column, shift, width);
        (
            f64::from(clouds[row * width + from]),
            f64::from(clouds[row * width + west(from, SHADE_SHIFT, width)]),
        )
    })
}

/// The column `by` west of `column`, round a texture `width` wide.
fn west(column: usize, by: usize, width: usize) -> usize {
    (column + width - by % width) % width
}

/// A colour under a cloud `cloud` strong, darkened first by the shade of the cloud beside it.
fn clouded(colour: Rgb, cloud: f64, shade: f64, day: f64, mu: f64) -> Rgb {
    let mut colour = colour;
    if shade > 0.0 {
        let k = 1.0 - 0.35 * shade * day;
        colour = colour.map(|c| c * k);
    }
    if cloud > 0.0 {
        let lit = 0.03 + day * (0.34 + 0.8 * mu.max(0.0));
        for (ch, white) in [240.0, 244.0, 250.0].into_iter().enumerate() {
            colour[ch] = mix(colour[ch], white * lit, cloud);
        }
    }
    colour
}

/// How much the air hazes a point that faces you `nz` squarely and the sun `mu`.
fn haze(nz: f64, mu: f64, thick: f64) -> f64 {
    (1.0 - nz).powf(2.4) * (0.22 + 0.68 * thick) * (0.12 + 0.88 * smooth(-0.25, 0.45, mu))
}

/// The air past the globe's edge: a glow toward the sun, warm where the sun sets behind the rim,
/// and a thin bright rim, as light over nothing.
#[allow(clippy::too_many_arguments)]
fn air_glow(
    nx: f64,
    ny: f64,
    r2: f64,
    radius: f64,
    reach: f64,
    thick: f64,
    colour: Rgb,
    sun: Sun,
    fade: f64,
) -> Pixel {
    let r = r2.sqrt();
    let out = (r - 1.0) * radius;
    if out > reach {
        return [0.0; 4];
    }
    let toward = (nx * sun.to[0] + ny * sun.to[1]) / r;
    let lit = smooth(-0.45, 0.55, toward + 0.2) * if sun.to[2] < 0.0 { 0.6 } else { 1.0 };
    let t = out / reach;
    let glow = (-3.4 * t).exp() * (1.0 - t) * (0.16 + 0.6 * thick) * lit * fade;
    let rim = (-out / 1.3).exp() * 0.5 * lit * fade;
    let warm = (-((toward + 0.12) / 0.22).powi(2)).exp() * 0.55;
    let tint = [0, 1, 2].map(|ch| mix(colour[ch], SUNSET[ch], warm));
    let light = [
        (tint[0] * glow + 230.0 * rim).min(255.0),
        (tint[1] * glow + 240.0 * rim).min(255.0),
        (tint[2] * glow + 255.0 * rim).min(255.0),
    ];
    light_over_nothing(light)
}

/// Light added over a clear pixel, as a straight colour and alpha that shows the same over
/// black: as opaque as its brightest channel.
fn light_over_nothing(light: Rgb) -> Pixel {
    let alpha = light[0].max(light[1]).max(light[2]) / 255.0;
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    [light[0] / alpha, light[1] / alpha, light[2] / alpha, alpha]
}

/// The colour at (u, v) of a texture read between its texels, so the globe's middle, where a
/// texel covers more than a pixel, shows no steps; columns wrap round.
fn between_texels(rgb: &[u8], width: usize, height: usize, u: f64, v: f64) -> Rgb {
    let fu = u * width as f64 - 0.5;
    let fv = v * height as f64 - 0.5;
    let i0 = fu.floor();
    let j0 = fv.floor().clamp(0.0, (height - 2) as f64);
    let du = fu - i0;
    let dv = (fv - j0).clamp(0.0, 1.0);
    let w = width as i64;
    let ia = (i0 as i64).rem_euclid(w) as usize;
    let ib = (i0 as i64 + 1).rem_euclid(w) as usize;
    let j0 = j0 as usize;
    let texel =
        |column: usize, row: usize, ch: usize| f64::from(rgb[(row * width + column) * 3 + ch]);
    [0, 1, 2].map(|ch| {
        texel(ia, j0, ch) * (1.0 - du) * (1.0 - dv)
            + texel(ib, j0, ch) * du * (1.0 - dv)
            + texel(ia, j0 + 1, ch) * (1.0 - du) * dv
            + texel(ib, j0 + 1, ch) * du * dv
    })
}

/// Fills `bounds` pixel by pixel from `pixel`, called with the pixel's centre on the globe's disc
/// (x right and y up, in radii) and its squared distance from the middle.
fn paint_pixels(
    bounds: Bounds,
    globe: &Globe,
    pixel: impl Fn(f64, f64, f64) -> Pixel + Sync,
) -> Canvas {
    let mut rgba = vec![[0_u8; 4]; bounds.width * bounds.height];
    let (cx, cy) = globe.centre;
    let radius = globe.radius;
    in_rows(&mut rgba, bounds.width, |row, out| {
        let y = (bounds.top + row) as f64 + 0.5;
        let ny = -(y - cy) / radius;
        for (column, out) in out.iter_mut().enumerate() {
            let x = (bounds.left + column) as f64 + 0.5;
            let nx = (x - cx) / radius;
            let [r, g, b, a] = pixel(nx, ny, nx * nx + ny * ny);
            if a <= 0.0 {
                continue;
            }
            let byte = |c: f64| c.clamp(0.0, 255.0).round() as u8;
            *out = [byte(r), byte(g), byte(b), byte(a * 255.0)];
        }
    });
    let flat: Vec<u8> = rgba.into_iter().flatten().collect();
    Canvas::from_rgba(bounds.width as u32, bounds.height as u32, &flat)
        .unwrap_or_else(|| bounds.canvas())
}
