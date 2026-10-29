//! The globe as what moves on it sees it: where a place of the map lands, how the globe bends
//! the map there, and how the sun lights it.

use crate::world::geo::{Globe, GlobePoint, Plate};
use crate::world::planet::{Side, Sun};
use crate::world::raster::Transform;
use crate::world::raster::noise::smooth;

/// A globe that weather is drawn on, onto a canvas whose top left is at `origin` of the scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobeGround {
    globe: Globe,
    sun: Sun,
    origin: (f64, f64),
}

/// One place of the map on the globe: where it lands on the canvas, how squarely it faces you
/// (`z`), the 2 × 2 map from the map's pixels there to the canvas's (`m`), the scale that map
/// keeps on average (`s`), how much of it shows toward the rim (`shows`), how much of the day it
/// is in, and how bright the planet's own clouds are there (`cloud_light`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub m: [f64; 4],
    pub s: f64,
    pub shows: f64,
    pub day: f64,
    pub cloud_light: f64,
}

impl Spot {
    /// The transform under which a drawing in the map's own pixels round (0, 0) lies flat on
    /// the globe at this place.
    pub fn on_ground(&self) -> Transform {
        Transform::new(self.m[0], self.m[1], self.m[2], self.m[3], self.x, self.y)
    }

    /// A step of the map's pixels, as the canvas's pixels here.
    pub fn along(&self, dx: f64, dy: f64) -> (f64, f64) {
        (
            self.m[0] * dx + self.m[2] * dy,
            self.m[1] * dx + self.m[3] * dy,
        )
    }

    /// How bright what lies here is drawn, as the clouds are lit.
    pub fn brightness(&self) -> f64 {
        self.cloud_light.clamp(0.02, 1.25)
    }
}

impl GlobeGround {
    pub fn new(globe: Globe, side: Side) -> Self {
        Self {
            globe,
            sun: Sun::of(side),
            origin: (0.0, 0.0),
        }
    }

    /// The same globe drawn onto a canvas whose top left is at (`left`, `top`) of the scene.
    pub fn onto(self, left: f64, top: f64) -> Self {
        Self {
            origin: (left, top),
            ..self
        }
    }

    pub fn globe(&self) -> &Globe {
        &self.globe
    }

    pub fn radius(&self) -> f64 {
        self.globe.radius
    }

    /// Where a pixel of the map lands on the canvas, and how squarely it faces you.
    pub fn project(&self, x: f64, y: f64) -> GlobePoint {
        let point = self.globe.project(Plate::MAP.place(x, y));
        GlobePoint {
            x: point.x - self.origin.0,
            y: point.y - self.origin.1,
            z: point.z,
        }
    }

    /// The place at pixel (x, y) of the map, while it faces you.
    pub fn at(&self, x: f64, y: f64) -> Option<Spot> {
        let p = self.project(x, y);
        if p.z <= 0.02 {
            return None;
        }
        let a = self.project(x + 1.5, y);
        let b = self.project(x, y + 1.5);
        let m = [
            (a.x - p.x) / 1.5,
            (a.y - p.y) / 1.5,
            (b.x - p.x) / 1.5,
            (b.y - p.y) / 1.5,
        ];
        let s = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
        // Faded at the rim as the dots are, and dimmed by the air there as the ground is.
        let shows = smooth(0.04, 0.28, p.z) * (1.0 - 0.5 * (1.0 - p.z).powf(2.4));
        let (cx, cy) = self.centre();
        let radius = self.globe.radius;
        let mu = self
            .sun
            .facing([(p.x - cx) / radius, -(p.y - cy) / radius, p.z]);
        let day = smooth(-0.1, 0.22, mu);
        Some(Spot {
            x: p.x,
            y: p.y,
            z: p.z,
            m,
            s,
            shows,
            day,
            cloud_light: 0.03 + day * (0.34 + 0.8 * mu.max(0.0)),
        })
    }

    /// A height `h` of the map's pixels above a spot, as canvas pixels: straight out from the
    /// globe, so it shortens toward you.
    pub fn up(&self, spot: &Spot, h: f64) -> (f64, f64) {
        let (cx, cy) = self.centre();
        let radius = self.globe.radius;
        (
            (spot.x - cx) / radius * h * spot.s,
            (spot.y - cy) / radius * h * spot.s,
        )
    }

    /// The globe's middle on the canvas.
    fn centre(&self) -> (f64, f64) {
        (
            self.globe.centre.0 - self.origin.0,
            self.globe.centre.1 - self.origin.1,
        )
    }
}
