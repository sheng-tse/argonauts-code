//! Behaviors: every action an agent takes gets a label from a fixed list by rule, and each
//! label has a fixed place on the behavior map.

pub mod behavior_map;
pub mod behavior_world;
pub mod crew_stats;
pub mod deep_map;

mod behavior;
mod list;

pub use behavior::Behavior;
pub use list::{Area, LABELS, LIST_VERSION, Label, LabelInfo};
