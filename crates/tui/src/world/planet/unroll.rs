//! The planet unrolling into the map, and the map rolling up into the planet: the globe turns
//! to face the map's middle, then every point of its ground slides from where the globe shows it
//! to where the map does, while the sky behind gives way to the map's paper. Turning the globe
//! and unrolling it show the same world, so nothing jumps at either end.

use super::globe::Bounds;
use super::standing::{ON_MAP, ON_PLANET, Standing};
use super::{Air, CloudsIn, Framed, Planet, Side, Sun, dots, globe, plain_sky};
use crate::base::draw::geo::{Globe, Plate, View};
use crate::world::map::fields::{Ground, HEIGHT, Rgb, WIDTH, byte, in_rows, to_bytes};
use crate::world::map::land::{Marks, Polar, SeaColours, Surface, Tint};
use crate::world::raster::noise::{mix, smooth};
use crate::world::raster::{Brush, Canvas, Colour};

/// The map's paper, which the sky gives way to.
const PAPER: Rgb = [31.0, 32.0, 47.0];

impl Planet {
    /// The picture `through` of the way from the planet facing `from` to the map, from 0 to 1,
    /// at the map's 1152 × 546: drawn from the map laid flat as the map shows it, and the flat
    /// ground the globe is wrapped in, both at the map's size. `None` for pictures of another
    /// size.
    pub fn unrolled(
        &self,
        from: View,
        through: f64,
        side: Side,
        flat_map: &Canvas,
        flat_ground: &Canvas,
        standing: &[&dyn Standing],
    ) -> Option<Canvas> {
        let size = (WIDTH, HEIGHT);
        let fits = |canvas: &Canvas| (canvas.width() as usize, canvas.height() as usize) == size;
        if !fits(flat_map) || !fits(flat_ground) {
            return None;
        }
        let t = smooth(0.0, 1.0, through);
        let turn = smooth(0.0, 0.35, t);
        let open = smooth(0.25, 1.0, t);
        let view = View {
            lon: from.lon * (1.0 - turn),
            tilt: from.tilt * (1.0 - turn),
        };
        let globe = self.globe(view, size);
        let sun = Sun::of(side);

        let sky = plain_sky(size).to_rgba();
        let mut rgba: Vec<u8> = sky
            .chunks(4)
            .flat_map(|pixel| {
                let paper = |ch: usize| byte(mix(f64::from(pixel[ch]), PAPER[ch], open));
                [paper(0), paper(1), paper(2), 255]
            })
            .collect();
        let fade = 1.0 - smooth(0.0, 0.3, open);
        if fade > 0.01 {
            let bounds = Bounds::round(&globe, &self.air, size);
            let disc = globe::paint(self, &globe, bounds, sun, fade, CloudsIn::Ground, 0.0);
            let mut scene = Canvas::from_rgba(size.0 as u32, size.1 as u32, &rgba)?;
            Framed::new(bounds, disc).lay_on(&mut scene);
            rgba = scene.to_rgba();
        }
        let points = Points {
            globe: &globe,
            air: &self.air,
            sun,
            open,
            map: &flat_map.to_rgba(),
            ground: &flat_ground.to_rgba(),
        };
        // The far side first, once it has opened enough to show, then the near side over it.
        if open > 0.12 {
            points.slide(&mut rgba, false);
        }
        points.slide(&mut rgba, true);
        let mut canvas = Canvas::from_rgba(size.0 as u32, size.1 as u32, &rgba)?;
        dots::unrolled(&mut canvas, self, &globe, open);
        ride_along(&mut canvas, standing, &globe, open);
        Some(canvas)
    }
}

/// What stands on the planet rides along as it unrolls, growing from its size on the planet to
/// its size on the map, lit as the map lights it; what was on the far side shows as the map
/// opens.
fn ride_along(canvas: &mut Canvas, standing: &[&dyn Standing], globe: &Globe, open: f64) {
    let (azimuth, altitude) = (315_f64.to_radians(), 42_f64.to_radians());
    let light = [
        azimuth.sin() * altitude.cos(),
        -azimuth.cos() * altitude.cos(),
        altitude.sin(),
    ];
    let grow = mix(ON_PLANET / ON_MAP, 1.0, open);
    for thing in standing {
        let (mx, my) = thing.place();
        let at = globe.project(Plate::MAP.place(mx, my));
        let shows = if at.z < 0.12 {
            smooth(0.6, 1.0, open)
        } else {
            1.0
        };
        if shows <= 0.01 {
            continue;
        }
        let size = thing.size();
        let (picture, (gx, gy)) = thing.picture(size, light);
        let (x, y) = (mix(at.x, mx, open), mix(at.y, my, open));
        canvas.draw_sized(
            &picture,
            x - gx * grow,
            y - gy * grow,
            f64::from(picture.width()) * grow,
            f64::from(picture.height()) * grow,
            &Brush::new(Colour::WHITE).alpha(shows),
        );
    }
}

/// Every other pixel of the flat map, each slid from where the globe shows it toward where the
/// map does, as a block of 3 × 3 so the gaps between them close.
struct Points<'a> {
    globe: &'a Globe,
    air: &'a Air,
    sun: Sun,
    open: f64,
    map: &'a [u8],
    ground: &'a [u8],
}

impl Points<'_> {
    fn slide(&self, rgba: &mut [u8], near: bool) {
        let plate = Plate::MAP;
        let (cx, cy) = self.globe.centre;
        let radius = self.globe.radius;
        let (sin_tilt, cos_tilt) = self.globe.view.tilt.sin_cos();
        let open = self.open;
        for y in (1..HEIGHT).step_by(2) {
            let lat = plate.place(0.0, y as f64).lat;
            let (sin_lat, cos_lat) = lat.sin_cos();
            for x in (1..WIDTH).step_by(2) {
                let lon = plate.place(x as f64, 0.0).lon;
                let (sin_d, cos_d) = (lon - self.globe.view.lon).sin_cos();
                let px = cos_lat * sin_d;
                let py = cos_tilt * sin_lat - sin_tilt * cos_lat * cos_d;
                let pz = sin_tilt * sin_lat + cos_tilt * cos_lat * cos_d;
                if (pz >= 0.0) != near {
                    continue;
                }
                let sx = mix(cx + radius * px, x as f64, open).trunc() as i64;
                let sy = mix(cy - radius * py, y as f64, open).trunc() as i64;
                let (light, limb) = if pz >= 0.0 {
                    let mu = self.sun.facing([px, py, pz]);
                    let lit = 0.035 + smooth(-0.1, 0.22, mu) * (0.16 + 0.95 * mu.max(0.0));
                    let limb =
                        (1.0 - pz).powf(2.4) * (0.22 + 0.68 * self.air.thickness) * (1.0 - open);
                    (mix(lit, 1.0, open), limb)
                } else {
                    (mix(0.22, 1.0, open), 0.0)
                };
                let from = (y * WIDTH + x) * 4;
                let colour: [u8; 3] = [0, 1, 2].map(|ch| {
                    let seen = mix(
                        f64::from(self.ground[from + ch]),
                        f64::from(self.map[from + ch]),
                        open,
                    );
                    byte(mix(seen * light, self.air.colour[ch] * 0.6, limb))
                });
                for yy in sy..sy + 3 {
                    for xx in sx..sx + 3 {
                        if xx < 0 || yy < 0 || xx >= WIDTH as i64 || yy >= HEIGHT as i64 {
                            continue;
                        }
                        let to = (yy as usize * WIDTH + xx as usize) * 4;
                        rgba[to..to + 3].copy_from_slice(&colour);
                        rgba[to + 3] = 255;
                    }
                }
            }
        }
    }
}

/// The ground the globe is wrapped in, laid flat at the map's size and lit by the map's own
/// light, in the planet's sea: what the globe's points are read from as it unrolls.
pub fn flat_ground(ground: &Ground, tint: Option<&Tint>, marks: Marks<'_>) -> Canvas {
    let polar = Polar::of(ground);
    let polar = if marks.icebergs {
        polar
    } else {
        polar.without_bergs()
    };
    let surface = Surface {
        sea: SeaColours::planet(ground.look),
        tint,
        step_lines: false,
        shade: true,
        marks,
        ..Surface::map(ground, Some(&polar))
    };
    let mut pixels = vec![[0_u8; 4]; WIDTH * HEIGHT];
    in_rows(&mut pixels, WIDTH, |y, row| {
        for (x, pixel) in row.iter_mut().enumerate() {
            let [r, g, b] = to_bytes(surface.at(x as f64 + 0.5, y as f64 + 0.5, None).colour);
            *pixel = [r, g, b, 255];
        }
    });
    let rgba: Vec<u8> = pixels.into_iter().flatten().collect();
    Canvas::from_rgba(WIDTH as u32, HEIGHT as u32, &rgba)
        .unwrap_or_else(|| Canvas::new(WIDTH as u32, HEIGHT as u32))
}
