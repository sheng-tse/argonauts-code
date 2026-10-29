//! Where things are on the planet, and where they land on the screen. The map is the whole planet
//! unrolled in plate carrée, the same number of pixels a radian across and up; the planet is a
//! globe that texture of the map is wrapped round, seen from outside.

use std::f64::consts::{PI, TAU};

/// A place on the planet in radians: longitude east of the map's middle, from -π to π, and
/// latitude north of the equator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LonLat {
    pub lon: f64,
    pub lat: f64,
}

impl LonLat {
    pub fn new(lon: f64, lat: f64) -> Self {
        Self { lon, lat }
    }

    /// The same place with its longitude brought back between -π and π, as after a globe turned
    /// past the map's edge.
    pub fn wrapped(self) -> Self {
        Self {
            lon: self.lon.sin().atan2(self.lon.cos()),
            lat: self.lat,
        }
    }
}

/// A plate carrée picture of the planet: 360° across its width, and at the same scale up and
/// down, as far north and south as its height reaches. The map is one, and so is the texture a
/// globe is drawn from, whose height reaches the poles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plate {
    width: f64,
    height: f64,
}

impl Plate {
    /// The map at the size the world's numbers are given for: 64 × 13 cells of 18 × 42 px. It
    /// reaches 85.3° north and south.
    pub const MAP: Plate = Plate {
        width: 1152.0,
        height: 546.0,
    };

    /// The texture the planet is drawn from: 1536 texels round, so the globe at its usual size
    /// shows about one texel a pixel at its middle, and pole to pole up and down.
    pub const TEXTURE: Plate = Plate {
        width: 1536.0,
        height: 768.0,
    };

    /// How much of the map's width and height the land takes, so the open sea and the polar ice
    /// show round it.
    pub const LAND: f64 = 0.74;

    pub fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    pub fn width(&self) -> f64 {
        self.width
    }

    pub fn height(&self) -> f64 {
        self.height
    }

    /// Pixels a radian, across and up alike.
    pub fn scale(&self) -> f64 {
        self.width / TAU
    }

    /// How far north and south the plate reaches, in radians.
    pub fn lat_max(&self) -> f64 {
        self.height / 2.0 / self.scale()
    }

    /// The place at a pixel.
    pub fn place(&self, x: f64, y: f64) -> LonLat {
        LonLat {
            lon: (x / self.width - 0.5) * TAU,
            lat: (self.height / 2.0 - y) / self.scale(),
        }
    }

    /// The pixel a place is drawn at.
    pub fn pixel(&self, at: LonLat) -> (f64, f64) {
        (
            (at.lon / TAU + 0.5) * self.width,
            self.height / 2.0 - self.scale() * at.lat,
        )
    }

    /// Whether a place lies on the plate. One the planet shows past its edge, on the far side of
    /// the map or nearer a pole than the map reaches, doesn't.
    pub fn holds(&self, at: LonLat) -> bool {
        at.lon.abs() < PI && at.lat.abs() < self.lat_max()
    }

    /// Where a point of the land's unit square is drawn, `x` east and `y` north from its bottom
    /// left. The land fills [`Plate::LAND`] of the plate each way inside a margin of a sixtieth of
    /// its width, so every label's place keeps its spot relative to the others at any size.
    pub fn land_pixel(&self, x: f64, y: f64) -> (f64, f64) {
        let margin = self.width / 60.0;
        (
            self.width / 2.0 + (x - 0.5) * (self.width - 2.0 * margin) * Self::LAND,
            self.height / 2.0 - (y - 0.5) * (self.height - 2.0 * margin) * Self::LAND,
        )
    }
}

/// Which way a globe faces: the longitude at its middle, and how far its north pole tips toward
/// you, in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub lon: f64,
    pub tilt: f64,
}

impl View {
    /// The terminal turns the planet in steps of 10°, keeping one picture a step.
    pub const STEPS: u32 = 36;

    /// The tilt the terminal shows the planet at.
    pub const TILT: f64 = 0.32;

    /// The terminal's view nearest to a longitude: turned to the closest of its steps, at its
    /// tilt.
    pub fn stepped(lon: f64) -> Self {
        let step = TAU / f64::from(Self::STEPS);
        Self {
            lon: (lon / step).round() * step,
            tilt: Self::TILT,
        }
    }

    /// Which of the terminal's steps the view is turned to, from 0, however many times round it
    /// has been.
    pub fn step(&self) -> u32 {
        let steps = f64::from(Self::STEPS);
        // Rounded and brought between 0 and STEPS - 1 first, so the cast is exact.
        (self.lon / TAU * steps).round().rem_euclid(steps) as u32
    }
}

/// A globe on the screen: which way it faces, its centre and its radius, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Globe {
    pub view: View,
    pub centre: (f64, f64),
    pub radius: f64,
}

/// Where a place lands on a globe: its point on the screen, and how squarely it faces you, from
/// 1 at the middle of the disc through 0 at its rim to -1 behind it. Only a place above 0 shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobePoint {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// The place of the planet under a point of a globe's disc, and how squarely it faces you, from
/// 1 at the middle to 0 at the rim.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    pub place: LonLat,
    pub facing: f64,
}

impl Globe {
    /// The planet's radius on the map's 1152 × 546 px, where the world's numbers are given.
    pub const RADIUS_ON_MAP: f64 = 240.0;

    pub fn new(view: View, centre: (f64, f64), radius: f64) -> Self {
        Self {
            view,
            centre,
            radius,
        }
    }

    /// The planet as a scene `width` × `height` px shows it: in the middle, as tall a part of
    /// the scene as it is of the map.
    pub fn in_scene(view: View, width: f64, height: f64) -> Self {
        Self {
            view,
            centre: (width / 2.0, height / 2.0),
            radius: Self::RADIUS_ON_MAP * height / Plate::MAP.height(),
        }
    }

    /// Where a place lands.
    pub fn project(&self, at: LonLat) -> GlobePoint {
        let (sin_tilt, cos_tilt) = self.view.tilt.sin_cos();
        let (sin_lat, cos_lat) = at.lat.sin_cos();
        let (sin_lon, cos_lon) = (at.lon - self.view.lon).sin_cos();
        let x = cos_lat * sin_lon;
        let y = cos_tilt * sin_lat - sin_tilt * cos_lat * cos_lon;
        let z = sin_tilt * sin_lat + cos_tilt * cos_lat * cos_lon;
        GlobePoint {
            x: self.centre.0 + self.radius * x,
            y: self.centre.1 - self.radius * y,
            z,
        }
    }

    /// The place under a point of the screen, or `None` off the disc.
    pub fn surface_at(&self, x: f64, y: f64) -> Option<Surface> {
        let nx = (x - self.centre.0) / self.radius;
        let ny = -(y - self.centre.1) / self.radius;
        let r2 = nx * nx + ny * ny;
        if r2 > 1.0 {
            return None;
        }
        let nz = (1.0 - r2).sqrt();
        let (sin_tilt, cos_tilt) = self.view.tilt.sin_cos();
        let lat = (nz * sin_tilt + ny * cos_tilt).clamp(-1.0, 1.0).asin();
        let lon = self.view.lon + nx.atan2(nz * cos_tilt - ny * sin_tilt);
        Some(Surface {
            place: LonLat { lon, lat }.wrapped(),
            facing: nz,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_map_is_the_whole_planet_at_one_scale_both_ways() {
        let map = Plate::MAP;
        let corner = map.place(0.0, 0.0);
        assert!(near(corner.lon, -PI));
        assert!((map.lat_max().to_degrees() - 85.3).abs() < 0.3);
        let (x, y) = map.pixel(LonLat::new(PI / 3.0, 0.5));
        assert!(near(map.place(x, y).lat, 0.5));
        assert!(near(map.scale(), 1152.0 / TAU));
    }

    #[test]
    fn the_texture_reaches_the_poles() {
        assert!(near(Plate::TEXTURE.lat_max(), PI / 2.0));
        let (x, y) = Plate::TEXTURE.pixel(LonLat::new(0.0, PI / 2.0));
        assert!(near(x, 768.0) && near(y, 0.0));
    }

    #[test]
    fn a_pixel_and_its_place_go_back_and_forth() {
        let map = Plate::new(1024.0, 481.0);
        for (x, y) in [(0.0, 0.0), (17.5, 400.25), (512.0, 240.5), (1023.0, 480.0)] {
            let (back_x, back_y) = map.pixel(map.place(x, y));
            assert!(near(back_x, x) && near(back_y, y), "{x}, {y}");
        }
    }

    #[test]
    fn only_places_on_the_map_are_held() {
        let map = Plate::MAP;
        assert!(map.holds(LonLat::new(3.0, 1.4)));
        assert!(!map.holds(LonLat::new(3.0, 1.5)));
        assert!(!map.holds(LonLat::new(-PI, 0.0)));
    }

    #[test]
    fn the_land_fills_the_middle_of_the_map() {
        let map = Plate::MAP;
        let (x, y) = map.land_pixel(0.5, 0.5);
        assert!(near(x, 576.0) && near(y, 273.0));
        let margin = 1152.0 / 60.0;
        let (left, bottom) = map.land_pixel(0.0, 0.0);
        assert!(near(left, 576.0 - 0.5 * (1152.0 - 2.0 * margin) * 0.74));
        assert!(near(bottom, 273.0 + 0.5 * (546.0 - 2.0 * margin) * 0.74));
    }

    #[test]
    fn the_terminal_turns_the_planet_in_ten_degree_steps() {
        let view = View::stepped(0.19);
        assert!(near(view.lon, 10f64.to_radians()));
        assert!(near(view.tilt, 0.32));
        assert_eq!(view.step(), 1);
        assert_eq!(View::stepped(TAU * 3.0 - 0.01).step(), 0);
        assert_eq!(View::stepped(-10f64.to_radians()).step(), 35);
    }

    #[test]
    fn the_place_the_globe_faces_lands_at_its_centre() {
        let globe = Globe::new(
            View {
                lon: 1.0,
                tilt: 0.32,
            },
            (576.0, 273.0),
            240.0,
        );
        let middle = globe.project(LonLat::new(1.0, 0.32));
        assert!(near(middle.x, 576.0) && near(middle.y, 273.0) && near(middle.z, 1.0));
        let surface = globe
            .surface_at(576.0, 273.0)
            .expect("the middle is on the disc");
        assert!(near(surface.place.lon, 1.0) && near(surface.place.lat, 0.32));
        assert!(near(surface.facing, 1.0));
    }

    #[test]
    fn a_place_on_the_near_side_is_found_where_it_lands() {
        let globe = Globe::in_scene(View::stepped(2.5), 1152.0, 546.0);
        assert!(near(globe.radius, 240.0));
        for place in [
            LonLat::new(2.0, 0.4),
            LonLat::new(3.1, -0.9),
            LonLat::new(-3.0, 1.1),
        ] {
            let point = globe.project(place);
            assert!(point.z > 0.0, "{place:?} faces away");
            let found = globe.surface_at(point.x, point.y).expect("on the disc");
            assert!(near(found.place.lon, place.lon) && near(found.place.lat, place.lat));
            assert!(near(found.facing, point.z));
        }
    }

    #[test]
    fn the_far_side_faces_away_and_off_the_disc_is_nothing() {
        let globe = Globe::new(
            View {
                lon: 0.0,
                tilt: 0.0,
            },
            (100.0, 100.0),
            50.0,
        );
        assert!(globe.project(LonLat::new(PI, 0.0)).z < 0.0);
        assert!(globe.surface_at(151.0, 100.0).is_none());
        assert!(globe.surface_at(149.0, 100.0).is_some());
    }
}
