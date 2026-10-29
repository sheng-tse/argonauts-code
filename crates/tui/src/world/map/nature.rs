//! Nature on the map: rivers, currents, lakes, floods, atolls, shoals, fire and the rest.
//!
//! Each kind keeps one meaning in the app's data, found in your agents' actions: a river is a
//! common next move, a lake a behavior sessions often end on, a hurricane a burst of one behavior
//! within an hour, a volcano actions that failed. [`Nature::find`] finds them all and places each
//! beside the behavior it belongs to, clear of the dots and of what was placed before it, in a
//! fixed order so the same actions always make the same world. The land's own kinds (swamps,
//! plateaus, basins and oases) are found in their turn among them.
//!
//! What lies on the ground is drawn into the ground's own picture through [`GroundMarks`], so
//! the light falls on it; what stands on the land or passes over it is the weather's.

mod facts;
mod find;
pub mod ground;
mod line;
mod water;

#[cfg(test)]
pub(crate) mod crews;
#[cfg(test)]
mod tests;

pub use facts::{NatureFacts, Unused};
pub use find::EQUATOR_GAP;
pub use ground::{GroundMarks, Sunk, fire_front, lay, scars, shoal_at, through};
pub use water::{CurrentRun, arc, current_runs, runs_at_sea, valley_path};

use argonauts_core::behavior_world::Crash;
use time::OffsetDateTime;

use crate::world::map::land::{LandKind, LandMark};

/// The way the smoke drifts and the shoals lie, a unit vector: east-north-east, so plumes never
/// cross their own cone.
pub fn wind() -> (f64, f64) {
    let length = 1.0_f64.hypot(-0.35);
    (1.0 / length, -0.35 / length)
}

/// Everything nature put on a crew's map, in the order it was placed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Nature {
    pub features: Vec<Feature>,
    /// Label places never used that fall at sea.
    pub shoals: Vec<Shoal>,
    /// The share of actions no label fits, when it is a tenth or more: the clouds, and their
    /// shadows drifting over the map.
    pub clouds: Option<f64>,
}

/// One mark of nature, by the behavior it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    pub kind: Kind,
    pub behavior: usize,
    pub x: f64,
    pub y: f64,
    /// How far it reaches as placed, which later marks keep clear of; 0 for what keeps nothing
    /// clear, such as a flood or a river.
    pub radius: f64,
    /// How far from its middle the pointer finds it.
    pub reach: f64,
}

/// Which kind of nature, with what it was found from.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// 25 or more actions of one behavior within an hour, named by the time zone.
    Hurricane(Hurricane),
    /// 6 or more agents did one behavior within 5 minutes.
    Tsunami(Wave),
    /// Its actions failed; smoking when one failed in the last 7 days.
    Volcano {
        failed: u32,
        smoking: bool,
        last: Option<u32>,
    },
    /// A quarter or more of its actions were in work a review sent back.
    Flood { sent_back: u32 },
    /// A quarter or more were in work that never merged; burning when the last was this week.
    Wildfire {
        dropped: u32,
        burning: bool,
        last: Option<u32>,
    },
    /// One agent did it 5 or more times in a row.
    Tornado { times: u32, longest: u32 },
    /// Sessions stopped on it: by you, a limit, or a full context.
    Thunderstorm { sessions: u32 },
    /// Sessions ended on it because their CLI failed: a fault line, which shakes as an
    /// earthquake while a crash is under 24 hours old.
    Rift(Rift),
    /// Sessions often ended on it.
    Lake { sessions: u32 },
    /// The land's own kinds, found in their turn.
    Land(LandKind),
    /// A common next move, over land.
    River(Stream),
    /// The same with sea between the two behaviors.
    Current(Stream),
    /// Used last period, not in this one, its place now at sea.
    Atoll,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hurricane {
    pub actions: u32,
    /// 1 to 5, at 25, 40, 60, 90 and 130 actions within the hour.
    pub category: u32,
    /// South of the equator it turns clockwise and its arms wind the other way.
    pub south: bool,
    pub at: OffsetDateTime,
}

/// A tsunami's run, from open sea to its behavior's coast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wave {
    pub agents: u32,
    pub at: OffsetDateTime,
    /// Where the wave starts out at sea, and where it ends 10 px off the coast.
    pub from: (f64, f64),
    pub to: (f64, f64),
    /// The way out from the behavior to the sea, a unit vector: the wave runs against it.
    pub out: (f64, f64),
    /// Where that way meets the coast.
    pub coast: (f64, f64),
    /// Its size in pixels, 40 at 6 agents and 3 more with each agent more, to 60.
    pub size: f64,
    /// -1 when it travels left, 1 right.
    pub face: f64,
}

/// A fault line, or an earthquake while a crash is fresh.
#[derive(Debug, Clone, PartialEq)]
pub struct Rift {
    pub sessions: u32,
    pub length: f64,
    /// The way the fault runs, across the way out from its behavior.
    pub angle: f64,
    /// Crashes in the 24 hours before the period's end, newest first: a ring each.
    pub recent: Vec<Crash>,
    /// The newest crash, for when the fault last moved.
    pub last: Option<OffsetDateTime>,
}

impl Rift {
    pub fn shaking(&self) -> bool {
        !self.recent.is_empty()
    }
}

/// A river's bed or a current's arc, from one behavior to the next.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub to: usize,
    pub moves: u32,
    pub path: Vec<(f64, f64)>,
    /// As wide as the move is common, to 4.2 px.
    pub width: f64,
}

/// A sandbank seen through the water where an unused label place falls at sea.
#[derive(Debug, Clone, PartialEq)]
pub struct Shoal {
    pub name: String,
    pub x: f64,
    pub y: f64,
    /// Along the wind, within 15° of it, so many read as one field of banks.
    pub angle: f64,
}

impl Kind {
    /// Its name in one word, as tests and pictures' notes name it.
    pub fn word(&self) -> &'static str {
        match self {
            Kind::Hurricane(_) => "hurricane",
            Kind::Tsunami(_) => "tsunami",
            Kind::Volcano { .. } => "volcano",
            Kind::Flood { .. } => "flood",
            Kind::Wildfire { .. } => "fire",
            Kind::Tornado { .. } => "tornado",
            Kind::Thunderstorm { .. } => "storm",
            Kind::Rift(rift) if rift.shaking() => "quake",
            Kind::Rift(_) => "fault",
            Kind::Lake { .. } => "lake",
            Kind::Land(LandKind::Swamp { .. }) => "swamp",
            Kind::Land(LandKind::Plateau { .. }) => "plateau",
            Kind::Land(LandKind::Basin { .. }) => "basin",
            Kind::Land(LandKind::Oasis) => "oasis",
            Kind::River(_) => "river",
            Kind::Current(_) => "current",
            Kind::Atoll => "atoll",
        }
    }
}

impl Feature {
    /// Whether it lies flat on the ground, where a cloud would hide it and nothing else flat may
    /// lie on it.
    pub fn is_flat(&self) -> bool {
        match &self.kind {
            Kind::Lake { .. } | Kind::Atoll => true,
            Kind::Rift(rift) => rift.shaking(),
            Kind::Land(kind) => !matches!(kind, LandKind::Plateau { .. }),
            _ => false,
        }
    }

    /// The land's kind as the ground places and draws it.
    pub fn land_mark(&self) -> Option<LandMark> {
        match &self.kind {
            Kind::Land(kind) => Some(LandMark {
                kind: kind.clone(),
                behavior: self.behavior,
                x: self.x,
                y: self.y,
                radius: self.radius,
                reach: self.reach,
            }),
            _ => None,
        }
    }

    /// The stream a river or a current runs along.
    pub fn stream(&self) -> Option<&Stream> {
        match &self.kind {
            Kind::River(stream) | Kind::Current(stream) => Some(stream),
            _ => None,
        }
    }
}
