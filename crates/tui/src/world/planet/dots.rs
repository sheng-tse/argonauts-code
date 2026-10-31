//! Your actions' dots on the globe: on the day side as on the map, only smaller toward the rim
//! and faded at it; on the night side a soft glow each, brighter for the last 7 days' actions.

use super::globe::Bounds;
use super::{DotColours, Planet, Sun};
use crate::base::draw::geo::{Globe, GlobePoint, Plate};
use crate::world::map::fields::Rgb;
use crate::world::raster::noise::{mix, smooth};
use crate::world::raster::{Blend, Brush, Canvas, Colour, Gradient, Path, Transform};

/// A dot's radius on the map's 1152 px, as the map's own dots are drawn.
const DOT: f64 = 0.82 * 1152.0 / 720.0;
/// The planet's dots keep the size the map's had before the map took the whole planet, so a dot
/// stands to the land on the globe about as it does on the map.
const PLANET_DOT: f64 = 0.62 / 0.82;
/// How far toward the rim a dot still shows: hidden behind it under this, faded in above.
const RIM: f64 = 0.04;
const EDGE: Rgb = [20.0, 20.0, 30.0];
const WHITE: Rgb = [246.0, 243.0, 236.0];
const GREY: Rgb = [150.0, 152.0, 170.0];

/// How one dot is drawn: its colour, radius at the map's size and strength, a compared dot's
/// ring in its group's colour, and whether its group is the one compared, or none is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DotLook {
    pub colour: Rgb,
    pub radius: f64,
    pub alpha: f64,
    pub ring: Option<Rgb>,
    pub own: Rgb,
    pub shown: bool,
    pub lifted: bool,
}

impl DotLook {
    pub fn of(group: usize, colours: &DotColours) -> Self {
        let own = colours.groups.get(group).copied().unwrap_or(GREY);
        match colours.compared {
            None => Self {
                colour: own,
                radius: 2.7 * DOT,
                alpha: 1.0,
                ring: None,
                own,
                shown: true,
                lifted: false,
            },
            Some(compared) if compared == group => Self {
                colour: WHITE,
                radius: 2.4 * DOT,
                alpha: 1.0,
                ring: Some(own),
                own,
                shown: true,
                lifted: true,
            },
            Some(_) => Self {
                colour: GREY,
                radius: 2.2 * DOT,
                alpha: 0.45,
                ring: None,
                own,
                shown: false,
                lifted: false,
            },
        }
    }

    /// The colour its glow takes at night: white for the compared group's, its group's colour
    /// when nothing is compared, grey for the rest.
    fn glow(&self) -> Rgb {
        match (self.shown, self.lifted) {
            (true, true) => WHITE,
            (true, false) => self.own,
            (false, _) => GREY,
        }
    }
}

/// Where a dot lands on the globe and how it is lit: on the disc, how far from the rim it shows,
/// and how much of the day it is in.
struct Placed {
    at: GlobePoint,
    shows: f64,
    day: f64,
}

fn placed(planet: &Planet, globe: &Globe, sun: Sun, index: usize) -> Option<Placed> {
    let (x, y) = planet.dots[index];
    let at = globe.project(Plate::MAP.place(f64::from(x), f64::from(y)));
    if at.z <= RIM {
        return None;
    }
    let (cx, cy) = globe.centre;
    let normal = [
        (at.x - cx) / globe.radius,
        -(at.y - cy) / globe.radius,
        at.z,
    ];
    Some(Placed {
        at,
        shows: smooth(RIM, 0.28, at.z),
        day: smooth(-0.06, 0.12, sun.facing(normal)),
    })
}

/// Each dot's point on the globe while it faces you, by its action's index, for the pointer.
pub fn facing(planet: &Planet, globe: &Globe) -> Vec<Option<(f32, f32)>> {
    (0..planet.dots.len())
        .map(|index| {
            let (x, y) = planet.dots[index];
            let at = globe.project(Plate::MAP.place(f64::from(x), f64::from(y)));
            (at.z > RIM).then_some((at.x as f32, at.y as f32))
        })
        .collect()
}

/// The night side's dots, a soft glow each, laid onto a canvas of `bounds` by screen.
pub fn glows(canvas: &mut Canvas, planet: &Planet, globe: &Globe, bounds: Bounds, sun: Sun) {
    let shift = Transform::IDENTITY.translate(-(bounds.left as f64), -(bounds.top as f64));
    let size = 0.75 + 0.25 * globe.radius / 240.0;
    for index in 0..planet.dots.len() {
        let Some(dot) = placed(planet, globe, sun, index) else {
            continue;
        };
        let night = 1.0 - dot.day;
        if night <= 0.01 {
            continue;
        }
        let look = planet.dot_looks[index];
        let fresh = planet.fresh.get(index).copied().unwrap_or(false);
        let alpha = dot.shows
            * night
            * if fresh { 0.42 } else { 0.12 }
            * if look.shown { 1.0 } else { 0.4 };
        let across = if fresh { 11.0 } else { 8.0 } * size * globe.radius / 240.0;
        let colour = look.glow();
        let (x, y, r) = (dot.at.x, dot.at.y, across / 2.0);
        let gradient = Gradient::radial(x, y, 0.0, x, y, r)
            .stop(0.0, Colour::of(mix_white(colour, [255.0; 3], 0.55), 1.0))
            .stop(
                0.18,
                Colour::of(mix_white(colour, [255.0, 250.0, 235.0], 0.2), 0.75),
            )
            .stop(0.5, Colour::of(colour, 0.18))
            .stop(1.0, Colour::of(colour, 0.0));
        let mut disc = Path::new();
        disc.circle(x, y, r);
        canvas.fill(
            &disc,
            &Brush::new(gradient)
                .at(shift)
                .alpha(alpha)
                .blend(Blend::Screen),
        );
    }
}

fn mix_white(colour: Rgb, toward: Rgb, t: f64) -> Rgb {
    [0, 1, 2].map(|ch| mix(colour[ch], toward[ch], t))
}

/// The day side's dots onto a canvas of `bounds`, `strength` strong: a compared group's last, so
/// they sit on top.
pub fn lit(
    canvas: &mut Canvas,
    planet: &Planet,
    globe: &Globe,
    bounds: Bounds,
    sun: Sun,
    strength: f64,
) {
    let shift = Transform::IDENTITY.translate(-(bounds.left as f64), -(bounds.top as f64));
    let scale = PLANET_DOT * globe.radius / 240.0;
    let mut order: Vec<usize> = (0..planet.dots.len()).collect();
    order.sort_by_key(|index| planet.dot_looks[*index].lifted);
    let disc = |x: f64, y: f64, r: f64| {
        let mut path = Path::new();
        path.circle(x, y, r);
        path
    };
    for index in order {
        let Some(dot) = placed(planet, globe, sun, index) else {
            continue;
        };
        if dot.day <= 0.01 {
            continue;
        }
        let look = planet.dot_looks[index];
        let alpha = look.alpha * strength * dot.shows * dot.day;
        if alpha <= 0.01 {
            continue;
        }
        let r = look.radius * scale * (0.55 + 0.45 * dot.at.z);
        let (x, y) = (dot.at.x, dot.at.y);
        let rim = if look.ring.is_some() { 2.6 } else { 1.2 } * scale;
        let brush = |colour: Rgb| Brush::new(Colour::of(colour, alpha)).at(shift);
        canvas.fill(&disc(x, y, r + rim), &brush(EDGE));
        if let Some(ring) = look.ring {
            canvas.fill(&disc(x, y, r + 1.6 * scale), &brush(ring));
        }
        canvas.fill(&disc(x, y, r), &brush(look.colour));
    }
}

/// The dots as the planet unrolls, `open` of the way to the map: each slid from where the globe
/// shows it to its place on the map and grown to the map's size, those on the far side coming
/// into view as the map opens.
pub fn unrolled(canvas: &mut Canvas, planet: &Planet, globe: &Globe, open: f64) {
    let scale = mix(PLANET_DOT, 1.0, open);
    let mut order: Vec<usize> = (0..planet.dots.len()).collect();
    order.sort_by_key(|index| planet.dot_looks[*index].lifted);
    for index in order {
        let (mx, my) = planet.dots[index];
        let (mx, my) = (f64::from(mx), f64::from(my));
        let at = globe.project(Plate::MAP.place(mx, my));
        if at.z < 0.0 && open < 0.6 {
            continue;
        }
        let shows = if at.z < 0.0 {
            smooth(0.6, 1.0, open)
        } else {
            1.0
        };
        let look = planet.dot_looks[index];
        let alpha = look.alpha * shows;
        if alpha <= 0.01 {
            continue;
        }
        let (x, y) = (mix(at.x, mx, open), mix(at.y, my, open));
        let r = look.radius * scale;
        let rim = if look.ring.is_some() { 2.6 } else { 1.2 } * scale;
        let disc = |r: f64| {
            let mut path = Path::new();
            path.circle(x, y, r);
            path
        };
        let brush = |colour: Rgb| Brush::new(Colour::of(colour, alpha));
        canvas.fill(&disc(r + rim), &brush(EDGE));
        if let Some(ring) = look.ring {
            canvas.fill(&disc(r + 1.6 * scale), &brush(ring));
        }
        canvas.fill(&disc(r), &brush(look.colour));
    }
}
