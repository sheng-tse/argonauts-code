//! Every threshold the universe reads, in one place. They are fixed, so a red giant or a binary
//! means the same in every crew's sky.

/// A maker under this share of your actions is a dwarf galaxy, with no arms.
pub const DWARF_SHARE: f64 = 0.03;
/// A galaxy turns once in this many seconds when all of its maker's actions came in the last
/// 7 days, slower in proportion, and stands still when none did. The hover line tells the turn
/// and the disc stays one picture, since a turning disc would be a new full picture each step.
pub const TURN_SECONDS: f64 = 60.0;
/// A star breathes every 2.4 s with actions in the last hour, every 4.8 s with actions in the
/// last 24 hours only.
pub const PULSE_HOUR: f64 = 2.4;
pub const PULSE_DAY: f64 = 4.8;
/// A protostar: under 30 actions, first used in the last 14 days.
pub const PROTO_MAX: u32 = 30;
pub const PROTO_DAYS: u32 = 14;
/// A red giant: 100 or more actions, none in the last 7 days.
pub const GIANT_MIN: u32 = 100;
/// A black hole: 20 or more actions, and none of its work merged.
pub const HOLE_MIN: u32 = 20;
/// A supernova remnant: its CLI failed in 3 sessions on one day.
pub const SUPERNOVA_FAILURES: usize = 3;
/// Two models work together when they share 5 or more runs, a quarter more often than chance.
pub const TOGETHER_RUNS: u32 = 5;
pub const TOGETHER_LIFT: f32 = 1.25;
/// Only the strongest three tidal streams between two makers' models are drawn.
pub const STREAMS_MAX: usize = 3;
/// A comet flies for each of the latest 6 runs of the last 24 hours with two makers' models: 5 s
/// across, then 3 s at rest.
pub const COMETS_MAX: usize = 6;
pub const COMET_TRAVEL: f64 = 5.0;
pub const COMET_REST: f64 = 3.0;
/// A meteor falls for each action of the last 10 minutes, replayed in a 10 s loop, 1.1 s each.
pub const METEOR_MINUTES: f64 = 10.0;
pub const METEOR_LOOP: f64 = 10.0;
pub const METEOR_TRAVEL: f64 = 1.1;
/// A system's planets: its six busiest behaviors, each with 3 or more actions and 4% or more of
/// the model's.
pub const PLANETS_MAX: usize = 6;
pub const PLANET_MIN: u32 = 3;
pub const PLANET_SHARE: f64 = 0.04;
/// A planet is a lava world when 3 or more of its actions failed and they are 30% or more.
pub const LAVA_MIN: u32 = 3;
pub const LAVA_SHARE: f64 = 0.3;
/// Flares stand for the latest 5 sessions its CLI failed in.
pub const FLARES_MAX: usize = 5;
/// A sunspot for every 2% of the model's actions that failed, one at least, 14 at most.
pub const SUNSPOT_SHARE: f64 = 0.02;
pub const SUNSPOTS_MAX: u32 = 14;
/// The sky holds one star for each earlier action, up to this many.
pub const EARLIER_MAX: u32 = 6_000;
/// Each further month's cluster is dimmer, and the sky keeps at most this many months.
pub const MONTHS_MAX: usize = 12;
/// A zoom between two levels takes this long.
pub const ZOOM_SECONDS: f64 = 0.7;
