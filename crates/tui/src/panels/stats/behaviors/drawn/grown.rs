//! The world a map grows: its ground from the actions' places and days, nature found on it from
//! the app's facts, and the wonders the crew raised, each where it stands.

use argonauts_core::behaviors::behavior_map::BehaviorMap;
use argonauts_core::behaviors::behavior_world::RisenWonder;

use crate::world::map::fields::{Ground, GroundFacts};
use crate::world::map::land::Polar;
use crate::world::map::nature::{Nature, NatureFacts, Sunk};
use crate::world::map::relief::Relief;
use crate::world::map::weather::{FlatLayers, Keep};
use crate::world::wonders::{self, Placed};

/// The noise every world grows from. It never changes, so the same actions always grow the same
/// world, and a world changes only as what the agents did changes.
pub const SEED: u32 = 41;

/// A crew's world grown from one map.
pub struct Grown {
    pub ground: Ground,
    pub facts: NatureFacts,
    pub nature: Nature,
    pub risen: Vec<RisenWonder>,
    /// The land that sank under a wonder, where one did.
    pub sunk: Option<Sunk>,
    pub relief: Relief,
    pub polar: Polar,
    /// The wonders where they stand on the map.
    pub wonders: Vec<Placed>,
}

impl std::fmt::Debug for Grown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grown")
            .field("behaviors", &self.ground.behaviors())
            .field("nature", &self.nature.features.len())
            .field("wonders", &self.wonders.len())
            .finish_non_exhaustive()
    }
}

impl Grown {
    /// The world `map` grows from the noise of `seed`, [`SEED`] in the app.
    pub fn new(map: &BehaviorMap, seed: u32) -> Self {
        Self::unless(map, seed, &|| false).expect("nothing stops a world grown in one go")
    }

    /// [`Self::new`], given up between its steps as soon as `stop` says so, so a world no
    /// longer wanted costs no more than the step under way.
    pub fn unless(map: &BehaviorMap, seed: u32, stop: &dyn Fn() -> bool) -> Option<Self> {
        let ground = Ground::grow(GroundFacts::from_map(map, seed));
        if stop() {
            return None;
        }
        let facts = NatureFacts::from_map(map, &ground);
        let nature = Nature::find(&ground, &facts);
        if stop() {
            return None;
        }
        let risen = map
            .world
            .as_ref()
            .map(|world| world.wonders.clone())
            .unwrap_or_default();
        let sunk = wonders::sunk(&ground, &facts, &risen);
        let flat = FlatLayers::new(&ground, &nature, Keep::All, sunk);
        if stop() {
            return None;
        }
        let wonders = wonders::place(&ground, &nature, &flat, &risen);
        let relief = Relief::new(&ground);
        if stop() {
            return None;
        }
        let polar = Polar::of(&ground);
        Some(Self {
            ground,
            facts,
            nature,
            risen,
            sunk,
            relief,
            polar,
            wonders,
        })
    }
}
