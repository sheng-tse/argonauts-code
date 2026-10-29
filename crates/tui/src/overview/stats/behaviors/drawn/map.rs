//! The map as the screen shows it: the lit land, what moves on it and stands on it, the dots,
//! and what passes over them, with what the pointer finds where.

use super::Grown;
use crate::world::map::fields::{HEIGHT, Rgb, WIDTH};
use crate::world::map::land::{Compared, Tint};
use crate::world::map::motion::Moment;
use crate::world::map::nature::GroundMarks;
use crate::world::map::relief::{DotStyle, Dots, Light, LitGround, Projected, STANDING};
use crate::world::map::weather::Weather;
use crate::world::{Depth, Layer, PartId, Picture, Scene, picture_key};

const LAND: PartId = PartId::new("map land", 0);
const DOTS: PartId = PartId::new("map dots", 0);

/// The tier of a dot right under the pointer, which answers before anything else, and of the
/// nearest dot a little further off, which answers after nature but before the bare ground.
const DOT_NEAR: u8 = 0;
const DOT_FAR: u8 = 3;

/// What one look of the map shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Look {
    /// `t` laid the land flat.
    pub flat: bool,
    /// The group compared with the rest, which hides nature and the wonders.
    pub compared: Option<usize>,
    /// Nature and the wonders show: `m` hides them.
    pub nature: bool,
    /// Each group's colour.
    pub colours: Vec<[u8; 3]>,
}

impl Look {
    /// Whether nature and the wonders show in this look.
    pub fn shows_nature(&self) -> bool {
        self.nature && self.compared.is_none()
    }
}

/// The pictures one look of the map is put together from.
pub struct MapLook {
    pub land: Projected,
    pub dots: Dots,
    /// The weather and the wonders where they stand, while nature shows.
    pub weather: Option<Weather>,
    land_picture: Picture,
    dots_picture: Picture,
}

impl MapLook {
    /// The look drawn from a crew's world; `key` tells its pictures from every other look's.
    pub fn new(grown: &Grown, look: &Look, key: u64) -> Self {
        let stand = if look.flat { 0.0 } else { STANDING };
        Self::standing(grown, look, key, stand, 2)
    }

    /// The look with its land standing `stand` px a unit of height, sampled `fine` times a
    /// pixel each way: a zoom arriving from the planet stands the land up through the stands
    /// between flat and [`STANDING`], each drawn once and coarser.
    pub fn standing(grown: &Grown, look: &Look, key: u64, stand: f64, fine: usize) -> Self {
        Self::standing_unless(grown, look, key, (stand, fine), &|| false)
            .expect("nothing stops a look drawn in one go")
    }

    /// The look drawn from a crew's world as [`Self::new`] draws it, given up between its steps
    /// as soon as `stop` says so.
    pub fn unless(grown: &Grown, look: &Look, key: u64, stop: &dyn Fn() -> bool) -> Option<Self> {
        let stand = if look.flat { 0.0 } else { STANDING };
        Self::standing_unless(grown, look, key, (stand, 2), stop)
    }

    /// [`Self::standing`], given up between its steps as soon as `stop` says so.
    pub fn standing_unless(
        grown: &Grown,
        look: &Look,
        key: u64,
        (stand, fine): (f64, usize),
        stop: &dyn Fn() -> bool,
    ) -> Option<Self> {
        let ground = &grown.ground;
        let compared = look.compared.and_then(|group| Compared::new(ground, group));
        let tint = compared.map(|compared| Tint::new(ground, compared));
        let nature = look.shows_nature();
        let marks = GroundMarks::new(ground, &grown.nature, nature, grown.sunk);
        let light = Light::new(ground, &grown.relief, stand);
        if stop() {
            return None;
        }
        let lit = LitGround::new(
            ground,
            &grown.relief,
            &light,
            tint.as_ref(),
            marks.marks(),
            Some(&grown.polar),
        );
        if stop() {
            return None;
        }
        let land = Projected::new(&grown.relief, &lit, stand, fine);
        if stop() {
            return None;
        }
        let groups: Vec<Rgb> = look.colours.iter().map(|rgb| rgb.map(f64::from)).collect();
        let style = DotStyle {
            groups: &groups,
            compared: look.compared,
        };
        let dots = Dots::new(ground, &grown.relief, style, stand);
        if stop() {
            return None;
        }
        let weather = nature
            .then(|| Weather::new(ground, &grown.relief, &grown.nature, &land, &grown.wonders));
        if stop() {
            return None;
        }
        let land_picture = Picture::new(picture_key([key, LAND.get()]), land.picture.to_image());
        let dots_picture = Picture::new(picture_key([key, DOTS.get()]), dots.picture.to_image());
        Some(Self {
            land,
            dots,
            weather,
            land_picture,
            dots_picture,
        })
    }

    /// The map at `moment`: the land, what moves on the ground and stands on the land, the
    /// dots, what passes over them, and what the pointer finds where. `key` tells the look's
    /// pictures from every other look's.
    pub fn scene(&self, moment: Moment, key: u64) -> Scene {
        let mut scene = Scene::new(WIDTH, HEIGHT);
        scene.add_layer(Layer {
            id: LAND,
            depth: Depth::Ground,
            picture: self.land_picture.clone(),
        });
        // The dots lie between what moves on the ground and what passes over it. A layer stacks
        // under the sprites at its own depth, so the dots go in first at the weather's.
        scene.add_layer(Layer {
            id: DOTS,
            depth: Depth::Weather,
            picture: self.dots_picture.clone(),
        });
        for mark in self.dots.marks(DOT_NEAR, DOT_FAR) {
            scene.hits.add(mark);
        }
        if let Some(weather) = &self.weather {
            if let Some(shadows) = weather.shadows(moment, key) {
                scene.add_layer(shadows);
            }
            for sprite in weather.sprites(moment, key) {
                scene.add_sprite(sprite);
            }
            for mark in weather.marks(moment) {
                scene.hits.add(mark);
            }
            scene.moves = !moment.is_still() && weather.moves();
        }
        scene.hits.set_under(self.land.under());
        scene
    }
}
