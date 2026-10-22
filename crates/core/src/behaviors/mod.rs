//! Behaviors: every action an agent takes gets a label from a fixed list by rule, and each
//! label has a fixed place on the behavior map.

mod behavior;
mod list;

pub use behavior::Behavior;
pub use list::{Area, LABELS, LIST_VERSION, Label, LabelInfo};
