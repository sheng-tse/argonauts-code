//! What the planet wears over the map's ground: its clouds, the light its night side shows, and
//! for a gas giant, bands in place of the ground.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use super::{Nature, PlanetFacts};
use crate::world::geo::{LonLat, Plate};
use crate::world::map::fields::{Ground, HEIGHT, Look, Rgb, WIDTH, in_rows, mix_rgb, to_bytes};
use crate::world::map::land::Texture;
use crate::world::raster::noise::{fbm3, hash3, smooth};

/// Under this share of actions no label fits, the sky is clear.
const CLOUD_FLOOR: f64 = 0.1;
/// Night actions under this share of all raise no aurora.
const AURORA_FLOOR: f64 = 0.03;

/// One band of a gas giant: the colour of the CLI it stands for and its share of the actions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub colour: Rgb,
    pub share: f64,
}

/// The planet's ground and what lies over it, texel by texel round the globe.
#[derive(Debug, Clone)]
pub struct Skin {
    /// The map's ground wrapped round the globe, or a gas giant's bands.
    pub texture: Texture,
    /// The crew's clouds, from 0 to 0.86 a texel, which drift east over the ground: as much of
    /// the sky as the share of actions no label fits. `None` for a clear sky.
    pub clouds: Option<Vec<f32>>,
    /// Cloud that stays where the map's nature put it.
    pub settled: Option<Vec<f32>>,
    /// Light that shines on the night side: lava, burning fronts, lightning and the aurora.
    pub glow: Option<Vec<[f32; 3]>>,
}

impl Skin {
    pub fn new(ground: &Ground, texture: Texture, facts: &PlanetFacts, nature: Nature<'_>) -> Self {
        if ground.look == Look::Gas {
            return Self {
                texture: gas_texture(ground, &facts.bands),
                clouds: None,
                settled: None,
                glow: None,
            };
        }
        let actions = ground.facts.dots.len().max(1) as f64;
        let aurora = (nature.shown && has_aurora(ground, facts))
            .then(|| (f64::from(facts.night) / actions / 0.25).clamp(0.25, 1.0));
        let lent = if nature.shown {
            nature
        } else {
            Nature::default()
        };
        let (glow, settled) = lent_layers(&texture, lent, aurora);
        let cover = f64::from(facts.unlabelled) / (actions + f64::from(facts.unlabelled));
        Self {
            clouds: (cover >= CLOUD_FLOOR).then(|| clouds(&texture, ground.seed(), cover)),
            texture,
            settled,
            glow,
        }
    }

    /// The texel under a place, and the texel `back` columns west of it, wrapping round.
    pub fn texel(&self, lon: f64, lat: f64) -> (usize, usize) {
        let (width, height) = (self.texture.width, self.texture.height);
        let u = ((lon + PI) / TAU).rem_euclid(1.0);
        let column = ((u * width as f64) as usize).min(width - 1);
        let row = (((FRAC_PI_2 - lat) / PI * height as f64).max(0.0) as usize).min(height - 1);
        (column, row)
    }
}

/// Whether a crew took enough of its actions at night for the aurora.
pub fn has_aurora(ground: &Ground, facts: &PlanetFacts) -> bool {
    let actions = ground.facts.dots.len().max(1) as f64;
    ground.look != Look::Gas && f64::from(facts.night) / actions >= AURORA_FLOOR
}

/// The place at the middle of a texel of a texture `width` × `height`.
fn texel_place(column: usize, row: usize, width: usize, height: usize) -> (f64, f64) {
    let lat = FRAC_PI_2 - (row as f64 + 0.5) / height as f64 * PI;
    let lon = (column as f64 + 0.5) / width as f64 * TAU - PI;
    (lon, lat)
}

fn on_sphere(lon: f64, lat: f64) -> (f64, f64, f64) {
    (lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin())
}

/// Clouds as weather has them on Earth: streaks drawn out along the latitudes, fronts as long
/// ridges, and more cloud in the storm belts near 55° and at the equator than in the dry belts
/// near 25°. Where they lie means nothing; how much of the sky they cover is `cover`.
fn clouds(texture: &Texture, seed: f64, cover: f64) -> Vec<f32> {
    let (width, height) = (texture.width, texture.height);
    let mut field = vec![0_f32; width * height];
    in_rows(&mut field, width, |row, out| {
        for (column, value) in out.iter_mut().enumerate() {
            let (lon, lat) = texel_place(column, row, width, height);
            let (x0, y0, z0) = on_sphere(lon, lat);
            // Turned off the poles, so no streak circles a pole.
            let sx = 0.8 * x0 + 0.36 * y0 - 0.48 * z0;
            let sy = -0.6 * x0 + 0.48 * y0 - 0.64 * z0;
            let qx = fbm3(sx * 1.9, sy * 1.9, z0 * 3.2, seed + 51.0, 4);
            let qy = fbm3(sx * 1.9 + 5.2, sy * 1.9, z0 * 3.2, seed + 52.0, 4);
            let body = fbm3(
                sx * 3.0 + 2.1 * qx,
                sy * 3.0 + 2.1 * qy,
                z0 * 8.0 + 1.4 * qx,
                seed + 53.0,
                6,
            );
            let ridge = 1.0
                - fbm3(
                    sx * 2.2 + 1.2 * qy,
                    sy * 2.2 + 1.2 * qx,
                    z0 * 6.0,
                    seed + 54.0,
                    4,
                )
                .abs();
            let away = lat.abs();
            let belt = 0.09 * (-((away - 0.95) / 0.22).powi(2)).exp()
                + 0.05 * (-(lat / 0.1).powi(2)).exp()
                - 0.07 * (-((away - 0.44) / 0.14).powi(2)).exp();
            *value = (0.62 * body + 0.34 * ridge.powi(3) + belt) as f32;
        }
    });
    // The threshold that leaves `cover` of the sky clouded, read on every other texel, each row
    // as often as its share of the sphere's area.
    let mut sample: Vec<f32> = Vec::new();
    for row in (0..height).step_by(2) {
        let weight = (FRAC_PI_2 - (row as f64 + 0.5) / height as f64 * PI).cos();
        for column in (0..width).step_by(2) {
            if hash3(column as f64, row as f64, 3.0, 5.0) < weight {
                sample.push(field[row * width + column]);
            }
        }
    }
    sample.sort_by(f32::total_cmp);
    let at = ((sample.len() as f64 * (1.0 - cover)) as usize).min(sample.len().saturating_sub(1));
    let threshold = f64::from(sample.get(at).copied().unwrap_or(f32::MAX));
    // The ramp is centred on the threshold, so the clouded share is the share asked for.
    for value in &mut field {
        *value = (smooth(threshold - 0.11, threshold + 0.11, f64::from(*value)) * 0.86) as f32;
    }
    field
}

/// The light the night side shows and the cloud that stays put, from what the map's nature
/// lends and the aurora's curtain at about 67° north and south, as strong as `aurora`.
fn lent_layers(
    texture: &Texture,
    nature: Nature<'_>,
    aurora: Option<f64>,
) -> (Option<Vec<[f32; 3]>>, Option<Vec<f32>>) {
    let wanted = WIDTH * HEIGHT * 4;
    let lights = nature.lights.filter(|layer| layer.len() == wanted);
    let sky = nature.sky.filter(|layer| layer.len() == wanted);
    if lights.is_none() && sky.is_none() && aurora.is_none() {
        return (None, None);
    }
    let (width, height) = (texture.width, texture.height);
    let map = Plate::MAP;
    let mut texels = vec![([0_f32; 3], 0_f32); width * height];
    in_rows(&mut texels, width, |row, out| {
        for (column, texel) in out.iter_mut().enumerate() {
            let (lon, lat) = texel_place(column, row, width, height);
            if lon.abs() < PI && lat.abs() < map.lat_max() {
                let (x, y) = map.pixel(LonLat::new(lon, lat));
                let at = ((y.max(0.0) as usize).min(HEIGHT - 1) * WIDTH
                    + (x.max(0.0) as usize).min(WIDTH - 1))
                    * 4;
                // The map's last few degrees fade into the open sea, and what lies on them too.
                let kept = 1.0
                    - smooth(map.lat_max() - 0.16, map.lat_max(), lat.abs()).max(smooth(
                        PI - 0.2,
                        PI,
                        lon.abs(),
                    ));
                if let Some(lights) = lights {
                    let strength = f64::from(lights[at + 3]) / 255.0 * kept;
                    if strength > 0.0 {
                        texel.0 =
                            [0, 1, 2].map(|ch| (f64::from(lights[at + ch]) * strength) as f32);
                    }
                }
                if let Some(sky) = sky {
                    // Only the bright cloud, not its shade.
                    let light =
                        (f64::from(sky[at]) + f64::from(sky[at + 1]) + f64::from(sky[at + 2]))
                            / 3.0;
                    let cloud = f64::from(sky[at + 3]) / 255.0 * smooth(70.0, 170.0, light) * kept;
                    texel.1 = (cloud * 0.97) as f32;
                }
            }
            if let Some(strength) = aurora {
                let light = curtain(lon, lat, strength);
                for (channel, added) in texel.0.iter_mut().zip(light) {
                    *channel += added as f32;
                }
            }
        }
    });
    let glow = (lights.is_some() || aurora.is_some())
        .then(|| texels.iter().map(|(light, _)| *light).collect());
    let settled = sky.map(|_| texels.iter().map(|(_, cloud)| *cloud).collect());
    (glow, settled)
}

/// The aurora as it looks from orbit: a thin ring with a sharp lower edge that fades toward the
/// pole, folded, with fine curtains and a faint violet top. Only the night shows it, and it is as
/// bright as the share of night actions.
fn curtain(lon: f64, lat: f64, strength: f64) -> Rgb {
    let away = lat.abs();
    if away <= 1.08 || away >= 1.38 {
        return [0.0; 3];
    }
    let (sx, sy, sz) = on_sphere(lon, lat);
    let sway = 0.035 * (lon * 4.0 + 2.5 * fbm3(sx * 2.0, sy * 2.0, sz * 2.0, 73.0, 2)).sin()
        + 0.012 * (lon * 13.0 + 3.0 * fbm3(sx * 4.0, sy * 4.0, sz * 4.0, 74.0, 2)).sin();
    let past = away - sway - 1.15;
    let band = if past < 0.0 {
        (-(past / 0.012).powi(2)).exp()
    } else {
        (-past / 0.035).exp()
    };
    let top = (-((past - 0.065) / 0.03).powi(2)).exp() * 0.28;
    if band <= 0.02 && top <= 0.02 {
        return [0.0; 3];
    }
    let rays = 0.4
        + 0.6
            * (0.5 + 0.5 * (lon * 58.0 + 8.0 * fbm3(sx * 3.0, sy * 3.0, sz * 3.0, 71.0, 3)).sin())
                .powi(2);
    let k = strength * 1.6 * rays * (0.55 + 0.45 * (lon * 2.0 + 1.0).sin());
    [
        (60.0 * band + 170.0 * top) * k,
        (255.0 * band + 90.0 * top) * k,
        (150.0 * band + 255.0 * top) * k,
    ]
}

/// A gas giant's face: bands of the CLIs' colours, each as wide as its share, pale zones and
/// darker belts within each, and a great storm as big as the busiest behavior's share.
fn gas_texture(ground: &Ground, bands: &[Band]) -> Texture {
    let plate = Plate::TEXTURE;
    let (width, height) = (plate.width() as usize, plate.height() as usize);
    let actions = ground.facts.dots.len().max(1) as f64;
    let busiest = ground
        .busiest
        .and_then(|at| ground.count.get(at))
        .map_or(0.0, |count| f64::from(*count));
    let storm = (busiest / actions).sqrt();
    let bands: Vec<Band> = if bands.is_empty() {
        vec![Band {
            colour: [150.0, 152.0, 170.0],
            share: 1.0,
        }]
    } else {
        bands.to_vec()
    };
    let seed = ground.seed();
    let mut colours = vec![[0_u8; 3]; width * height];
    in_rows(&mut colours, width, |row, out| {
        for (column, colour) in out.iter_mut().enumerate() {
            let (lon, lat) = texel_place(column, row, width, height);
            *colour = to_bytes(gas_at(&bands, storm, lon, lat, seed));
        }
    });
    Texture {
        width,
        height,
        rgb: colours.into_iter().flatten().collect(),
        wet: vec![false; width * height],
        slope_east: vec![0.0; width * height],
        slope_north: vec![0.0; width * height],
    }
}

fn gas_at(bands: &[Band], storm: f64, lon: f64, lat: f64, seed: f64) -> Rgb {
    let (sx, sy, sz) = on_sphere(lon, lat);
    let warp =
        0.11 * fbm3(sx * 3.0, sy * 3.0, sz * 6.0, seed, 4) + 0.03 * (lon * 3.0 + lat * 9.0).sin();
    let t = ((lat + warp).sin() + 1.0) / 2.0 * 3.0;
    let t = t - t.floor();
    let mut reached = 0.0;
    let mut at = bands.len() - 1;
    for (index, band) in bands.iter().enumerate() {
        reached += band.share;
        if t <= reached {
            at = index;
            break;
        }
    }
    let band = bands[at];
    let local = (t - (reached - band.share)) / band.share.max(1e-9);
    let zone = mix_rgb(band.colour, [236.0, 226.0, 214.0], 0.62);
    let belt = mix_rgb(band.colour, [40.0, 34.0, 44.0], 0.35);
    let mut colour = mix_rgb(
        zone,
        belt,
        smooth(0.35, 0.65, local) * (1.0 - smooth(0.85, 1.0, local)),
    );
    colour = mix_rgb(
        colour,
        [240.0, 236.0, 228.0],
        0.12 * (fbm3(sx * 9.0, sy * 9.0, sz * 20.0, seed + 4.0, 3) + 0.5),
    );
    let down = lat + 0.38;
    let across = (lon - 0.7).sin().atan2((lon - 0.7).cos());
    let r = (across / (0.55 * storm + 0.05)).hypot(down / (0.25 * storm + 0.03));
    if r < 1.0 {
        let eye = mix_rgb([200.0, 110.0, 80.0], [250.0, 220.0, 190.0], r);
        colour = mix_rgb(colour, eye, smooth(1.0, 0.6, r) * 0.9);
    }
    colour
}
