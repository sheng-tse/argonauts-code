//! The planet turning: the map's ground wrapped round a globe, lit by a sun about 30° off your
//! eye so the night is a thin crescent, its air as thick as the crew was busy last week, the ice
//! at both poles, the aurora on the night side, and clouds as many as the actions no label fits.
//!
//! Every look means something. The ground is the map's own texture, so turning the globe and
//! unrolling it show the same world. Under 40 actions the planet is a gas giant whose bands are
//! the CLIs that did the work. The planet turns at the crew's pace: 24 s a turn at 30 actions a
//! day, and not at all under 1 a day.
//!
//! A [`Planet`] is worked out once per crew and grouping, and draws the globe at any size and
//! turn in three pictures that stack: the ground with its air, the clouds that drift over it, and
//! the dots on top.

mod air;
mod dots;
mod facts;
mod globe;
mod pointing;
mod skin;
mod sky;
mod spin;
mod standing;
mod unroll;

#[cfg(test)]
mod tests;

pub use air::{Air, Side, Sun, turn_seconds};
pub use dots::DotLook;
pub use globe::Bounds;
pub use pointing::OnGlobe;
pub use skin::{Band, Skin, has_aurora};
pub use spin::{Home, Moment, Shot, Spin, shown_drift, turned, turning_moment};
pub use standing::{ON_MAP, ON_PLANET, Standing};
pub use unroll::flat_ground;

use crate::world::geo::{Globe, View};
use crate::world::map::fields::{Ground, Look, Rgb};
use crate::world::map::land::{Polar, Texture};
use crate::world::raster::{Brush, Canvas, Colour};

/// What a crew's planet is drawn from besides its ground.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanetFacts {
    /// Actions a day over the last week: how thick the air is, and how fast the planet turns.
    pub per_day: f64,
    /// Actions no label fits: the clouds cover their share of these and the dots together.
    pub unlabelled: u32,
    /// Actions between 10:00 PM and 5:00 AM: the aurora, which only the night side shows.
    pub night: u32,
    /// Whether each dot's action came in the last 7 days, in the ground's order of dots: those
    /// glow brighter on the night side.
    pub fresh: Vec<bool>,
    /// A gas giant's bands, one for each CLI, as wide as its share of the actions.
    pub bands: Vec<Band>,
}

/// What the map's nature lends the planet: whether it shows at all (it doesn't while a group is
/// compared), and two pictures at the map's size, as straight RGBA rows: what lights the ground
/// at night, and the cloud nature leaves lying still.
#[derive(Debug, Clone, Copy, Default)]
pub struct Nature<'a> {
    pub shown: bool,
    pub lights: Option<&'a [u8]>,
    pub sky: Option<&'a [u8]>,
}

/// How the dots are coloured: each group's colour, and the group compared, if one is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DotColours {
    pub groups: Vec<Rgb>,
    pub compared: Option<usize>,
}

/// Whether the clouds that drift are drawn into the ground's picture, or kept apart on a picture
/// of their own that lies over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudsIn {
    Ground,
    Apart,
}

/// A picture of part of a scene: its canvas, with its top left at (`left`, `top`) of the scene.
#[derive(Debug, Clone)]
pub struct Framed {
    pub left: usize,
    pub top: usize,
    pub canvas: Canvas,
}

impl Framed {
    fn new(bounds: Bounds, canvas: Canvas) -> Self {
        Self {
            left: bounds.left,
            top: bounds.top,
            canvas,
        }
    }

    /// Lays the picture over `scene` where it belongs.
    pub fn lay_on(&self, scene: &mut Canvas) {
        scene.draw(
            &self.canvas,
            self.left as f64,
            self.top as f64,
            &Brush::new(Colour::WHITE),
        );
    }
}

/// A crew's planet, ready to draw at any turn and size.
#[derive(Debug, Clone)]
pub struct Planet {
    /// Tells this planet's pictures from any other's: the crew, its grouping and what is
    /// compared, as the caller keys them.
    pub key: u64,
    pub look: Look,
    pub air: Air,
    pub skin: Skin,
    /// Each dot's pixel on the map.
    dots: Vec<(f32, f32)>,
    dot_looks: Vec<DotLook>,
    fresh: Vec<bool>,
    turn: Option<f64>,
    /// The ice at the poles, its icebergs only while nature shows.
    polar: Polar,
    /// Nature shows, on a planet that has ground: the ice, the aurora and the clouds answer.
    nature: bool,
    /// Enough night actions for the aurora.
    aurora: bool,
}

impl Planet {
    /// The planet of a crew's ground, wrapped in `texture`, the ground as drawn for the planet.
    pub fn new(
        key: u64,
        ground: &Ground,
        texture: Texture,
        facts: &PlanetFacts,
        nature: Nature<'_>,
        colours: &DotColours,
    ) -> Self {
        let dot_looks = ground
            .facts
            .dots
            .iter()
            .map(|dot| DotLook::of(dot.group, colours))
            .collect();
        let shown = nature.shown && ground.look != Look::Gas;
        let polar = Polar::of(ground);
        Self {
            key,
            look: ground.look,
            air: Air::of(ground.look, facts.per_day),
            skin: Skin::new(ground, texture, facts, nature),
            dots: ground.dots.clone(),
            dot_looks,
            fresh: facts.fresh.clone(),
            turn: turn_seconds(facts.per_day),
            polar: if shown { polar } else { polar.without_bergs() },
            nature: shown,
            aurora: shown && skin::has_aurora(ground, facts),
        }
    }

    /// Seconds one turn takes: longer the fewer actions a day the crew took last week, and
    /// `None` under one a day, when the planet stands still.
    pub fn turn_seconds(&self) -> Option<f64> {
        self.turn
    }

    /// The globe facing `view` in the middle of a scene `size` px, as tall a part of it as on
    /// the map's 1152 × 546.
    pub fn globe(&self, view: View, size: (usize, usize)) -> Globe {
        Globe::in_scene(view, size.0 as f64, size.1 as f64)
    }

    /// The box the globe and its air take in a scene `size` px; every picture of the planet
    /// covers it.
    pub fn bounds(&self, globe: &Globe, size: (usize, usize)) -> Bounds {
        Bounds::round(globe, &self.air, size)
    }

    /// The ground's picture on a scene `size` px: the globe and its air over nothing, and on
    /// the night side the dots' glow. The clouds drifted `drift` radians east come with it
    /// unless kept apart.
    pub fn ground(
        &self,
        globe: &Globe,
        size: (usize, usize),
        side: Side,
        clouds: CloudsIn,
        drift: f64,
    ) -> Framed {
        let bounds = self.bounds(globe, size);
        let sun = Sun::of(side);
        let mut canvas = globe::paint(self, globe, bounds, sun, 1.0, clouds, drift);
        dots::glows(&mut canvas, self, globe, bounds, sun);
        Framed::new(bounds, canvas)
    }

    /// The globe and its air alone, as the universe shows your planet: no dots and no glow.
    pub fn bare_ground(
        &self,
        globe: &Globe,
        size: (usize, usize),
        side: Side,
        clouds: CloudsIn,
        drift: f64,
    ) -> Framed {
        let bounds = self.bounds(globe, size);
        let canvas = globe::paint(self, globe, bounds, Sun::of(side), 1.0, clouds, drift);
        Framed::new(bounds, canvas)
    }

    /// The clouds alone, drifted `drift` radians east over the ground, to lie over a ground
    /// drawn with them apart; `None` under a clear sky.
    pub fn clouds(
        &self,
        globe: &Globe,
        size: (usize, usize),
        side: Side,
        drift: f64,
    ) -> Option<Framed> {
        self.skin.clouds.as_ref()?;
        let bounds = self.bounds(globe, size);
        let canvas = globe::paint_clouds(self, globe, bounds, Sun::of(side), drift);
        Some(Framed::new(bounds, canvas))
    }

    /// The dots on the day side facing you, smaller toward the rim and faded at it, above the
    /// ground and the clouds.
    pub fn dots(&self, globe: &Globe, size: (usize, usize), side: Side) -> Framed {
        let bounds = self.bounds(globe, size);
        let mut canvas = bounds.canvas();
        dots::lit(&mut canvas, self, globe, bounds, Sun::of(side), 1.0);
        Framed::new(bounds, canvas)
    }

    /// Each dot's point on the screen while it faces you, by its action's index.
    pub fn dots_facing(&self, globe: &Globe) -> Vec<Option<(f32, f32)>> {
        dots::facing(self, globe)
    }

    /// The whole planet as one still picture `size` px: the sky (the plain one without
    /// `sky`), the ground with its clouds, and the dots.
    pub fn picture(
        &self,
        view: View,
        side: Side,
        size: (usize, usize),
        sky: Option<&Canvas>,
        drift: f64,
    ) -> Canvas {
        let mut scene = match sky {
            Some(sky) => sky.clone(),
            None => sky::plain(size.0, size.1),
        };
        let globe = self.globe(view, size);
        self.ground(&globe, size, side, CloudsIn::Ground, drift)
            .lay_on(&mut scene);
        self.dots(&globe, size, side).lay_on(&mut scene);
        scene
    }

    /// The planet drawn over whatever `canvas` holds, at any place and size, as the universe
    /// shows it: the globe and its air, and where `dots` is above 0 its dots that strong.
    pub fn draw_on(&self, canvas: &mut Canvas, globe: &Globe, side: Side, drift: f64, dots: f64) {
        let size = (canvas.width() as usize, canvas.height() as usize);
        self.bare_ground(globe, size, side, CloudsIn::Ground, drift)
            .lay_on(canvas);
        if dots > 0.0 {
            let bounds = self.bounds(globe, size);
            let mut lit = bounds.canvas();
            dots::lit(&mut lit, self, globe, bounds, Sun::of(side), dots);
            Framed::new(bounds, lit).lay_on(canvas);
        }
    }
}

/// The plain sky behind the planet, `size` px, for a scene the universe lends no sky.
pub fn plain_sky(size: (usize, usize)) -> Canvas {
    sky::plain(size.0, size.1)
}
