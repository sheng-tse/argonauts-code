//! What nature is found from, beside the ground's own facts: each behavior's bursts, breaks and
//! outcomes, the end of the period, the storm's word, and the label places never used.

use argonauts_core::behavior_map::BehaviorMap;
use argonauts_core::behavior_world::{BehaviorFacts, StormWord};
use argonauts_core::behaviors::LABELS;
use time::OffsetDateTime;

use crate::world::map::fields::Ground;

/// Nature's facts, one for each of the ground's behaviors in its order.
#[derive(Debug, Clone, PartialEq)]
pub struct NatureFacts {
    pub behaviors: Vec<BehaviorFacts>,
    /// The end of the period, now for the current one: a crash in the 24 hours before it shakes
    /// the ground.
    pub end: OffsetDateTime,
    /// What a storm of 25 or more actions in an hour is called where you are.
    pub storm: StormWord,
    /// Actions no label fits: the clouds, when a tenth or more of all.
    pub unlabelled: u32,
    /// Places on the label list never used, this period or the one before, in the list's order:
    /// a shoal where one falls at sea.
    pub unused: Vec<Unused>,
}

/// A label place no action has used.
#[derive(Debug, Clone, PartialEq)]
pub struct Unused {
    pub name: String,
    /// On the land's unit square, `x` east and `y` north.
    pub place: (f64, f64),
}

impl Default for NatureFacts {
    fn default() -> Self {
        Self {
            behaviors: Vec::new(),
            end: OffsetDateTime::UNIX_EPOCH,
            storm: StormWord::default(),
            unlabelled: 0,
            unused: Vec::new(),
        }
    }
}

impl NatureFacts {
    /// The facts the app worked out for a map, lined up with the ground grown from it. Without
    /// them every behavior is calm and only the unused places show.
    pub fn from_map(map: &BehaviorMap, ground: &Ground) -> Self {
        let world = map.world.as_deref();
        let behaviors = ground
            .facts
            .behaviors
            .iter()
            .map(|days| {
                world
                    .and_then(|world| world.behavior(&days.name))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        let end = world
            .map(|world| world.end)
            .filter(|end| *end != OffsetDateTime::UNIX_EPOCH)
            .or_else(|| map.actions.iter().map(|action| action.at).max())
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        let used = |name: &str| {
            ground
                .facts
                .behaviors
                .iter()
                .position(|days| days.name == name)
                .is_some_and(|at| {
                    ground.count[at] > 0 || ground.facts.behaviors[at].last_period > 0
                })
        };
        let unused = LABELS
            .iter()
            .filter(|label| !used(label.words))
            .map(|label| Unused {
                name: label.words.to_owned(),
                place: (f64::from(label.place.0), 1.0 - f64::from(label.place.1)),
            })
            .collect();
        Self {
            behaviors,
            end,
            storm: world.map(|world| world.crew.storm).unwrap_or_default(),
            unlabelled: map.unlabelled as u32,
            unused,
        }
    }

    /// A behavior's facts, or a calm behavior's past the list's end.
    pub fn of(&self, behavior: usize) -> &BehaviorFacts {
        static CALM: std::sync::OnceLock<BehaviorFacts> = std::sync::OnceLock::new();
        self.behaviors
            .get(behavior)
            .unwrap_or_else(|| CALM.get_or_init(BehaviorFacts::default))
    }
}
