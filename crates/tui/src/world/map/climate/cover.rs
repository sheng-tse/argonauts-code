//! What grows on the land, by climate and height. Each climate runs from a light low plain to a
//! darker upland, so the height shows in the colour as well as the shade; past 30 actions the
//! land is bare rock and past 100 snow and glacier, as above a tree line.

use super::Climate;
use crate::world::map::fields::{Ground, Rgb, mix_rgb, scale_rgb};
use crate::world::raster::noise::{smooth, vnoise3};

pub const ROCK: Rgb = [122.0, 110.0, 98.0];
pub const ROCK_DRY: Rgb = [150.0, 118.0, 92.0];
pub const SNOW: Rgb = [236.0, 240.0, 245.0];

/// A climate's low plain, its upland, and for the open ones the lighter ground between its
/// plants: dunes in desert, straw in savanna, a meadow's grass.
struct Cover {
    low: Rgb,
    high: Rgb,
    light: Option<Rgb>,
}

const COVER: [Cover; 5] = [
    Cover {
        low: [212.0, 180.0, 124.0],
        high: [180.0, 136.0, 96.0],
        light: Some([228.0, 204.0, 154.0]),
    },
    Cover {
        low: [196.0, 166.0, 92.0],
        high: [158.0, 128.0, 70.0],
        light: Some([212.0, 186.0, 112.0]),
    },
    Cover {
        low: [126.0, 146.0, 74.0],
        high: [94.0, 114.0, 58.0],
        light: Some([142.0, 158.0, 86.0]),
    },
    Cover {
        low: [70.0, 100.0, 50.0],
        high: [46.0, 72.0, 38.0],
        light: None,
    },
    // A rainforest is a yellower, darker green, as it looks from orbit.
    Cover {
        low: [34.0, 78.0, 36.0],
        high: [20.0, 54.0, 28.0],
        light: None,
    },
];

/// A climate's colour `up` of the way from its plain to its upland, with the edge noise `noise`
/// lightening the open ground and a canopy's mottle on the forests.
pub fn cover_colour(climate: Climate, up: f64, noise: f64, x: f64, y: f64) -> Rgb {
    let cover = &COVER[climate.index()];
    let base = mix_rgb(cover.low, cover.high, up);
    match cover.light {
        Some(light) => mix_rgb(base, light, (0.5 + 0.9 * noise).clamp(0.0, 1.0) * 0.55),
        None => {
            let mottle = if climate == Climate::Rainforest {
                0.1
            } else {
                0.07
            };
            let k = climate.index() as f64;
            scale_rgb(base, 1.0 + mottle * vnoise3(x / 2.8, y / 2.8, k, 5.0))
        }
    }
}

/// The land's colour at pixel (x, y), coarse point (gx, gy), where the heights stand at
/// `height` actions; `cold` turns an idle crew's land to tundra and brings its snow down.
pub fn land_cover(
    ground: &Ground,
    height: f64,
    x: f64,
    y: f64,
    gx: f64,
    gy: f64,
    cold: f64,
) -> Rgb {
    let noise = ground.climate.edge_noise.sample(x / 2.0, y / 2.0);
    let wet = ground.climate.field.sample(gx, gy) + 0.55 * noise;
    let log = height.max(1e-3).log10() + 0.05 * noise;
    let up = smooth(-0.6, 1.4, log);
    // One climate gives way to the next over 0.16 of the field, each edge at the half.
    let low = (wet - 0.42).floor().clamp(0.0, 4.0) as usize;
    let into = if low < 4 {
        smooth(low as f64 + 0.42, low as f64 + 0.58, wet)
    } else {
        0.0
    };
    let mut colour = cover_colour(Climate::ALL[low], up, noise, x, y);
    if into > 0.0 {
        colour = mix_rgb(
            colour,
            cover_colour(Climate::ALL[low + 1], up, noise, x, y),
            into,
        );
    }
    // The tree line and the snow line are broken by the ground's own grain, so rock and snow
    // edge in patches rather than an outlined disc.
    let grain = rock_grain(x, y);
    let rock = smooth(1.36, 1.56, log + grain);
    if rock > 0.0 {
        let bare = if wet < 0.6 { ROCK_DRY } else { ROCK };
        colour = mix_rgb(colour, bare, rock * 0.92);
    }
    let snow = smooth(1.92, 2.06, log + grain);
    if snow > 0.0 {
        colour = mix_rgb(colour, SNOW, snow);
    }
    let mesa = ground.mesa_at(gx, gy);
    if mesa > 0.04 {
        colour = plateau_cover(colour, mesa, height, snow, x, y);
    }
    if cold > 0.0 {
        let tundra = mix_rgb([132.0, 126.0, 108.0], [112.0, 114.0, 104.0], up);
        colour = mix_rgb(colour, tundra, (cold * 1.6).min(0.8));
        let frost = mix_rgb([224.0, 230.0, 238.0], [246.0, 248.0, 251.0], up);
        colour = mix_rgb(
            colour,
            frost,
            smooth(0.55, 0.95, log + 0.25 * noise) * (cold * 2.0).min(1.0),
        );
    }
    colour
}

/// The fine grain that breaks the tree and snow lines, in tens of the count's logarithm.
pub fn rock_grain(x: f64, y: f64) -> f64 {
    0.07 * vnoise3(x / 7.0, y / 7.0, 3.3, 17.0) + 0.04 * vnoise3(x / 3.0, y / 3.0, 5.1, 19.0)
}

/// A plateau's cliffs show their layers, rust and buff bands by height; its flat top is caprock
/// whose edge catches the sun, so the top has a clear rim above its cliffs.
fn plateau_cover(colour: Rgb, mesa: f64, height: f64, snow: f64, x: f64, y: f64) -> Rgb {
    let cliff = smooth(0.04, 0.2, mesa) * (1.0 - smooth(0.8, 0.96, mesa));
    let top = smooth(0.84, 1.0, mesa);
    let z = 5.5 * (1.0 + height).log2();
    let band = 0.5 + 0.5 * (z * 1.9 + 0.8 * vnoise3(x / 23.0, y / 23.0, 2.0, 41.0)).sin();
    let layers = mix_rgb(
        [140.0, 74.0, 52.0],
        [218.0, 172.0, 122.0],
        smooth(0.25, 0.75, band),
    );
    let mut colour = mix_rgb(colour, layers, cliff * 0.96);
    if snow < 0.5 {
        colour = mix_rgb(colour, [168.0, 142.0, 108.0], top * 0.8);
    }
    mix_rgb(
        colour,
        [226.0, 204.0, 162.0],
        smooth(0.62, 0.86, mesa) * (1.0 - smooth(0.9, 0.99, mesa)) * 0.75,
    )
}
