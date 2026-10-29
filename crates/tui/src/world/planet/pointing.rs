//! What the pointer finds on the planet: the same things the map finds, at the same places, so
//! the planet writes the map's own line for the spot you point at, plus what only a globe has:
//! both ice caps, the icebergs, the aurora on the night side, and the clouds.

use std::f64::consts::{FRAC_PI_2, PI};

use super::standing::{self, Standing};
use super::{Planet, Side, Sun};
use crate::world::geo::{LonLat, Plate, View};
use crate::world::planet_fx::GlobeGround;
use crate::world::scene::{HitMap, Mark, Pointed, Shape, Under};

/// How near a dot the pointer must be to find it, in the scene's pixels.
const DOT_REACH: f32 = 6.0;
/// How far toward the rim a mark of the map still answers.
const RIM: f64 = 0.04;
/// How cloudy a place must be for the pointer to find the clouds over it.
const CLOUDY: f32 = 0.45;

/// What lies at a place of the globe where no mark of the map answers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OnGlobe {
    /// An iceberg off the south ice, by its quiet stretch's place in the list.
    Iceberg(usize),
    /// The north cap, for days since the last action.
    IceCap,
    /// The south ice, for quiet days.
    SouthIce,
    /// The aurora's curtain, which only the night side shows.
    Aurora,
    /// Clouds, for the actions no label fits.
    Clouds,
    /// The map's own ground, at this pixel of the flat map: its line says what lies there.
    Ground { x: f64, y: f64 },
    /// Nearer a pole than the map reaches, where it shows open sea.
    OpenSea,
}

impl Planet {
    /// What the pointer finds on the planet's scene `size` px facing `view`: each dot facing
    /// you within 6 px of it, then what stands upright on it, then the map's own `marks`
    /// carried to where they lie on the globe, then the ground under them, and the sky off the
    /// globe. Dots among `marks` are left to the planet's own.
    pub fn hits(
        &self,
        view: View,
        size: (usize, usize),
        marks: &[Mark],
        standing: &[&dyn Standing],
    ) -> HitMap {
        let globe = self.globe(view, size);
        let mut hits = HitMap::new();
        for (index, at) in self.dots_facing(&globe).into_iter().enumerate() {
            let Some((x, y)) = at else {
                continue;
            };
            let pointed = Pointed::Action(u32::try_from(index).unwrap_or(u32::MAX));
            hits.add(Mark::new(Shape::Point { x, y }, pointed).reach(DOT_REACH));
        }
        for thing in standing {
            if let Some(at) = standing::stands(*thing, &globe, Sun::of(Side::Day)) {
                let (left, top, right, bottom) = standing::reach(&at);
                let shape = Shape::Rect {
                    left,
                    top,
                    right,
                    bottom,
                };
                hits.add(Mark::new(shape, thing.pointed()).tier(1));
            }
        }
        let ground = GlobeGround::new(globe, Side::Day);
        for mark in marks {
            if matches!(mark.pointed, Pointed::Action(_)) {
                continue;
            }
            for carried in carried(mark, &ground) {
                hits.add(carried);
            }
        }
        hits.set_under(Under::Globe(globe));
        hits
    }

    /// What lies at `place` beyond the map's marks: the planet's own ice, aurora and clouds
    /// first, then the map's ground. `drift` is how far east the clouds the scene shows have
    /// drifted, in radians.
    pub fn on_globe(&self, place: LonLat, side: Side, drift: f64) -> OnGlobe {
        let (lon, lat) = (place.lon, place.lat);
        if self.nature {
            for berg in &self.polar.bergs {
                let down = (lat - berg.lat) / berg.radius;
                let across =
                    (lon - berg.lon).sin().atan2((lon - berg.lon).cos()) * lat.cos() / berg.radius;
                if down.hypot(across * 0.7) < 1.6 {
                    return OnGlobe::Iceberg(berg.index);
                }
            }
            if self.polar.cap_north.is_some() && lat > self.polar.edge_north() - 0.02 {
                return OnGlobe::IceCap;
            }
            if self.polar.cap_south.is_some() && lat < -self.polar.edge_south() + 0.02 {
                return OnGlobe::SouthIce;
            }
            if side == Side::Night && self.aurora && (lat.abs() - 1.16).abs() < 0.07 {
                return OnGlobe::Aurora;
            }
        }
        let map = Plate::MAP;
        if lat.abs() >= map.lat_max() {
            return OnGlobe::OpenSea;
        }
        if self.nature && self.cloudy(place, drift) {
            return OnGlobe::Clouds;
        }
        let (x, y) = map.pixel(place.wrapped());
        OnGlobe::Ground { x, y }
    }

    /// Whether a cloud the scene shows lies over `place`, the drifting ones moved `drift` east.
    fn cloudy(&self, place: LonLat, drift: f64) -> bool {
        let skin = &self.skin;
        let width = skin.texture.width;
        let height = skin.texture.height;
        let row =
            (((FRAC_PI_2 - place.lat) / PI * height as f64).max(0.0) as usize).min(height - 1);
        let column = |lon: f64| {
            let u = ((lon + PI) / std::f64::consts::TAU).rem_euclid(1.0);
            ((u * width as f64) as usize).min(width - 1)
        };
        let settled = skin
            .settled
            .as_ref()
            .is_some_and(|settled| settled[row * width + column(place.lon)] > CLOUDY);
        let drifting = skin
            .clouds
            .as_ref()
            .is_some_and(|clouds| clouds[row * width + column(place.lon - drift)] > CLOUDY);
        settled || drifting
    }
}

/// A mark of the flat map carried onto the globe: where its shape lies facing you, sized by how
/// the globe bends the map there, its reach widened toward the rim as the map's own reach is
/// read through it. A line that runs over the rim becomes one mark for each stretch facing you.
fn carried(mark: &Mark, ground: &GlobeGround) -> Vec<Mark> {
    // After the dots and what stands upright, in the map's own order among themselves.
    let tier = mark.tier.saturating_add(2);
    let near = |x: f32, y: f32| {
        ground
            .at(f64::from(x), f64::from(y))
            .filter(|spot| spot.z > RIM)
    };
    // Only where a shape lies, without how the globe bends the map there, which takes two more
    // projections: a line's points and a box's corners need no more.
    let facing = |x: f32, y: f32| {
        let point = ground.project(f64::from(x), f64::from(y));
        (point.z > RIM).then_some((point.x as f32, point.y as f32))
    };
    let reach = |spot: &crate::world::planet_fx::Spot| {
        (f64::from(mark.reach) * spot.s / spot.z.max(0.3)) as f32
    };
    let made = |shape: Shape, reach: f32| Mark {
        shape,
        reach,
        tier,
        pointed: mark.pointed,
    };
    match &mark.shape {
        Shape::Point { x, y } => near(*x, *y)
            .map(|spot| {
                let shape = Shape::Point {
                    x: spot.x as f32,
                    y: spot.y as f32,
                };
                made(shape, reach(&spot))
            })
            .into_iter()
            .collect(),
        Shape::Disc { x, y, radius } => near(*x, *y)
            .map(|spot| {
                let shape = Shape::Disc {
                    x: spot.x as f32,
                    y: spot.y as f32,
                    radius: (f64::from(*radius) * spot.s) as f32,
                };
                made(shape, reach(&spot))
            })
            .into_iter()
            .collect(),
        Shape::Ellipse {
            x,
            y,
            rx,
            ry,
            angle,
        } => near(*x, *y)
            .map(|spot| {
                let shape = Shape::Ellipse {
                    x: spot.x as f32,
                    y: spot.y as f32,
                    rx: (f64::from(*rx) * spot.s) as f32,
                    ry: (f64::from(*ry) * spot.s) as f32,
                    angle: *angle,
                };
                made(shape, reach(&spot))
            })
            .into_iter()
            .collect(),
        Shape::Rect {
            left,
            top,
            right,
            bottom,
        } => {
            let corners: Vec<_> = [
                (*left, *top),
                (*right, *top),
                (*left, *bottom),
                (*right, *bottom),
            ]
            .into_iter()
            .filter_map(|(x, y)| facing(x, y))
            .collect();
            if corners.len() < 4 {
                return Vec::new();
            }
            let Some(spot) = near((left + right) / 2.0, (top + bottom) / 2.0) else {
                return Vec::new();
            };
            let xs = corners.iter().map(|corner| corner.0);
            let ys = corners.iter().map(|corner| corner.1);
            let shape = Shape::Rect {
                left: xs.clone().fold(f32::INFINITY, f32::min),
                right: xs.fold(f32::NEG_INFINITY, f32::max),
                top: ys.clone().fold(f32::INFINITY, f32::min),
                bottom: ys.fold(f32::NEG_INFINITY, f32::max),
            };
            vec![made(shape, reach(&spot))]
        }
        Shape::Path { points, width } => {
            // Each stretch facing you, its points on the map and on the globe.
            type Point = (f32, f32);
            let mut stretches: Vec<Vec<(Point, Point)>> = vec![Vec::new()];
            for &(x, y) in points {
                match facing(x, y) {
                    Some(on_globe) => stretches
                        .last_mut()
                        .into_iter()
                        .for_each(|run| run.push(((x, y), on_globe))),
                    None if stretches.last().is_some_and(|run| !run.is_empty()) => {
                        stretches.push(Vec::new());
                    }
                    None => {}
                }
            }
            stretches
                .into_iter()
                .filter(|run| !run.is_empty())
                .filter_map(|run| {
                    let ((x, y), _) = run[run.len() / 2];
                    let middle = near(x, y)?;
                    let shape = Shape::Path {
                        points: run.iter().map(|&(_, on_globe)| on_globe).collect(),
                        width: (f64::from(*width) * middle.s) as f32,
                    };
                    Some(made(shape, reach(&middle)))
                })
                .collect()
        }
    }
}
